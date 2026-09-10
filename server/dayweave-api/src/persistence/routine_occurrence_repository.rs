//! Durable publication-qualified occurrence state. No operation here changes a
//! canonical template, credits execution, or guesses completion from history.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row as _, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{DatabaseScope, item_completion_repository, item_repository};
use crate::{
    item_completion::ItemCompletionReopenState,
    items::{Item, ItemKind, ItemStatus},
    routine_occurrences::{
        MAX_ROUTINE_OCCURRENCE_BYTES, MAX_ROUTINE_OCCURRENCE_MEMBERS, RoutineOccurrenceAggregate,
        RoutineOccurrenceCommand, RoutineOccurrenceError, RoutineOccurrenceEvidence,
        RoutineOccurrenceManifest, RoutineOccurrenceMemberDefinition, RoutineOccurrenceSnapshot,
        RoutineOccurrenceSourceEvidence, RoutineOccurrenceWorkUnit, initialize_routine_occurrence,
        plan_routine_occurrence, routine_occurrence_snapshot, validate_occurrence_lookup,
    },
    scheduling::ComposeScheduleResult,
};

const PAGE_BYTES: usize = 8 * 1024 * 1024;
const CURSOR_PREFIX: &str = "DWR1.";

#[derive(Clone)]
pub struct PostgresRoutineOccurrenceRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceMutation {
    pub operation_id: Uuid,
    pub replayed: bool,
    pub occurrence: RoutineOccurrenceSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceChange {
    pub sequence: u64,
    pub occurrence: RoutineOccurrenceSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrencePage {
    pub schema_version: u16,
    pub changes: Vec<RoutineOccurrenceChange>,
    pub cursor: String,
    pub has_more: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoutineOccurrencePlanningEvidence {
    pub change_head: u64,
    pub instances: Vec<RoutineOccurrencePlanningInstance>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoutineOccurrencePlanningInstance {
    pub aggregate: RoutineOccurrenceAggregate,
    pub current_source_revisions: BTreeMap<Uuid, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoutineOccurrenceAdmission {
    pub admitted_instance_ids: Vec<Uuid>,
    pub unsupported_root_ids: BTreeSet<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    workspace: Uuid,
    /// Some pins a list/current-state capture; None is ordinary delta.
    list_head: Option<u64>,
    after: u64,
}

struct CurrentEvidence {
    items: Vec<Item>,
    by_id: BTreeMap<Uuid, usize>,
    children: BTreeMap<Uuid, Vec<Uuid>>,
    definition_hashes: Mutex<BTreeMap<Uuid, Result<String, RoutineOccurrenceError>>>,
    execution_revision: u64,
    live_work_units: BTreeSet<RoutineOccurrenceWorkUnit>,
}

impl CurrentEvidence {
    fn subtree(&self, root_id: Uuid) -> Result<Vec<&Item>, RoutineOccurrenceError> {
        if !self.by_id.contains_key(&root_id) {
            return Err(RoutineOccurrenceError::OccurrenceMissing);
        }
        let mut selected = BTreeSet::new();
        let mut pending = vec![root_id];
        while let Some(id) = pending.pop() {
            if !selected.insert(id) {
                return Err(RoutineOccurrenceError::Invalid);
            }
            if selected.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS {
                return Err(RoutineOccurrenceError::TooLarge);
            }
            pending.extend(self.children.get(&id).into_iter().flatten().copied());
        }
        Ok(selected
            .into_iter()
            .map(|id| &self.items[self.by_id[&id]])
            .collect())
    }

    fn definition_hash(&self, root_id: Uuid) -> Result<String, RoutineOccurrenceError> {
        if let Some(value) = self
            .definition_hashes
            .lock()
            .map_err(|_| RoutineOccurrenceError::Unavailable)?
            .get(&root_id)
        {
            return value.clone();
        }
        let value = self
            .subtree(root_id)
            .and_then(|subtree| routine_definition_hash(&subtree, root_id));
        self.definition_hashes
            .lock()
            .map_err(|_| RoutineOccurrenceError::Unavailable)?
            .insert(root_id, value.clone());
        value
    }
}

impl PostgresRoutineOccurrenceRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }

    #[must_use]
    pub fn scope(&self) -> DatabaseScope {
        self.scope
    }

    /// # Errors
    /// Rejects unavailable scope, absent instances or malformed retained proof.
    pub async fn get(&self, id: Uuid) -> Result<RoutineOccurrenceSnapshot, RoutineOccurrenceError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_read(&mut tx, self.scope).await?;
        let aggregate = read_aggregate(&mut tx, self.scope, id).await?;
        let current = current_evidence(&mut tx, self.scope).await?;
        let snapshot = snapshot(&aggregate, &current)?;
        tx.commit().await.map_err(storage)?;
        Ok(snapshot)
    }

    /// Resolves the exact public calendar identity to a complete current private
    /// review. This does not admit an occurrence or grant planning authority.
    /// Historical instances remain readable when their source is no longer eligible.
    ///
    /// # Errors
    /// Rejects invalid selectors before storage access, unavailable scope, absent
    /// instances or malformed retained proof.
    pub async fn lookup(
        &self,
        series_item_id: Uuid,
        occurrence_id: Uuid,
    ) -> Result<RoutineOccurrenceSnapshot, RoutineOccurrenceError> {
        validate_occurrence_lookup(series_item_id, occurrence_id)?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_read(&mut tx, self.scope).await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM routine_occurrences WHERE workspace_id=$1 AND series_item_id=$2 AND occurrence_id=$3",
        )
        .bind(self.scope.workspace_id)
        .bind(series_item_id)
        .bind(occurrence_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let aggregate = read_aggregate(
            &mut tx,
            self.scope,
            id.ok_or(RoutineOccurrenceError::OccurrenceMissing)?,
        )
        .await?;
        let current = current_evidence(&mut tx, self.scope).await?;
        let snapshot = snapshot(&aggregate, &current)?;
        tx.commit().await.map_err(storage)?;
        Ok(snapshot)
    }

    /// # Errors
    /// Exact replay is resolved before fresh source/revision checks. Uncertain
    /// storage failure never manufactures a definitive operation result.
    pub async fn put(
        &self,
        id: Uuid,
        member_id: Uuid,
        command: RoutineOccurrenceCommand,
        actor_session_id: Option<Uuid>,
    ) -> Result<RoutineOccurrenceMutation, RoutineOccurrenceError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_read(&mut tx, self.scope).await?;
        let request = bounded_json(&command)?;
        if let Some(row) = sqlx::query("SELECT instance_id,member_item_id,request_json,result_json FROM routine_occurrence_operations WHERE workspace_id=$1 AND operation_id=$2")
            .bind(self.scope.workspace_id).bind(command.operation_id).fetch_optional(&mut *tx).await.map_err(storage)?
        {
            if row.try_get::<Uuid,_>("instance_id").map_err(storage)?!=id
                || row.try_get::<Uuid,_>("member_item_id").map_err(storage)?!=member_id
                || row.try_get::<Value,_>("request_json").map_err(storage)?!=request
            { return Err(RoutineOccurrenceError::OperationReused); }
            let occurrence: RoutineOccurrenceSnapshot = serde_json::from_value(row.try_get("result_json").map_err(storage)?)
                .map_err(|_| RoutineOccurrenceError::Unavailable)?;
            occurrence.aggregate.validate().map_err(|_| RoutineOccurrenceError::Unavailable)?;
            tx.commit().await.map_err(storage)?;
            return Ok(RoutineOccurrenceMutation { operation_id:command.operation_id,replayed:true,occurrence });
        }
        command.validate(member_id)?;
        let before = read_aggregate(&mut tx, self.scope, id).await?;
        let current = current_evidence(&mut tx, self.scope).await?;
        let evidence = evidence_for(&before.manifest, &current)?;
        let now = database_now(&mut tx).await?;
        let plan = plan_routine_occurrence(&before, &evidence, member_id, &command, now)?;
        let effects = plan
            .effects
            .iter()
            .map(|effect| {
                json!({
                    "before":effect.before,"after":effect.after,"reason":effect.reason
                })
            })
            .collect::<Vec<_>>();
        let sequence = write_change(
            &mut tx,
            self.scope,
            &plan.snapshot.aggregate,
            Some(&before),
            Some(command.operation_id),
            json!(effects),
            now,
        )
        .await?;
        let result = bounded_json(&plan.snapshot)?;
        sqlx::query("INSERT INTO routine_occurrence_operations(workspace_id,operation_id,instance_id,member_item_id,actor_user_id,actor_session_id,change_sequence,request_json,result_json,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(self.scope.workspace_id).bind(command.operation_id).bind(id).bind(member_id).bind(self.scope.user_id).bind(actor_session_id)
            .bind(signed(sequence)?).bind(request).bind(result).bind(now).execute(&mut *tx).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(RoutineOccurrenceMutation {
            operation_id: command.operation_id,
            replayed: false,
            occurrence: plan.snapshot,
        })
    }

    /// Immutable-head current-state paging. Install only after the terminal
    /// page; its cursor is also the ordinary ordered-delta checkpoint.
    /// # Errors
    /// Rejects foreign/malformed cursors and any indivisible oversized snapshot.
    pub async fn list(
        &self,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<RoutineOccurrencePage, RoutineOccurrenceError> {
        self.page(cursor, limit, true).await
    }

    /// # Errors
    /// Rejects unbounded, foreign, malformed or ahead-of-head cursor requests.
    pub async fn delta(
        &self,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<RoutineOccurrencePage, RoutineOccurrenceError> {
        self.page(cursor, limit, false).await
    }

    #[allow(clippy::too_many_lines)] // Keep the immutable capture, whole-record budget and cursor under one transaction.
    async fn page(
        &self,
        cursor: Option<&str>,
        limit: u16,
        list: bool,
    ) -> Result<RoutineOccurrencePage, RoutineOccurrenceError> {
        if !(1..=100).contains(&limit) {
            return Err(RoutineOccurrenceError::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_read(&mut tx, self.scope).await?;
        let head = change_head(&mut tx, self.scope).await?;
        let checkpoint = match cursor {
            Some(value) => decode_cursor(value, self.scope)?,
            None => Cursor {
                workspace: self.scope.workspace_id,
                list_head: list.then_some(head),
                after: 0,
            },
        };
        if checkpoint.after > head
            || checkpoint
                .list_head
                .is_some_and(|value| value > head || checkpoint.after > value)
            || list != checkpoint.list_head.is_some()
        {
            return Err(RoutineOccurrenceError::InvalidCursor);
        }
        let capture_head = checkpoint.list_head.unwrap_or(head);
        // Fetch only content-free pointers first; never allocate limit×8MiB.
        let query = if list {
            "SELECT sequence,instance_id,revision,octet_length(aggregate_json::text)::bigint AS bytes FROM routine_occurrence_changes change WHERE workspace_id=$1 AND sequence>$2 AND sequence<=$3 AND NOT EXISTS(SELECT 1 FROM routine_occurrence_changes newer WHERE newer.workspace_id=change.workspace_id AND newer.instance_id=change.instance_id AND newer.sequence>change.sequence AND newer.sequence<=$3) ORDER BY sequence LIMIT $4"
        } else {
            "SELECT sequence,instance_id,revision,octet_length(aggregate_json::text)::bigint AS bytes FROM routine_occurrence_changes WHERE workspace_id=$1 AND sequence>$2 AND sequence<=$3 ORDER BY sequence LIMIT $4"
        };
        let rows = sqlx::query(query)
            .bind(self.scope.workspace_id)
            .bind(signed(checkpoint.after)?)
            .bind(signed(capture_head)?)
            .bind(i64::from(limit) + 1)
            .fetch_all(&mut *tx)
            .await
            .map_err(storage)?;
        let current = current_evidence(&mut tx, self.scope).await?;
        let mut changes = Vec::new();
        let mut bytes = 1024_usize;
        let mut has_more = rows.len() > usize::from(limit);
        for row in rows.iter().take(usize::from(limit)) {
            let sequence = unsigned(row.try_get("sequence").map_err(storage)?)?;
            let stored_bytes = usize::try_from(row.try_get::<i64, _>("bytes").map_err(storage)?)
                .map_err(|_| RoutineOccurrenceError::Unavailable)?;
            if stored_bytes > PAGE_BYTES {
                return Err(RoutineOccurrenceError::TooLarge);
            }
            let aggregate:RoutineOccurrenceAggregate=serde_json::from_value(sqlx::query_scalar::<_,Value>("SELECT aggregate_json FROM routine_occurrence_changes WHERE workspace_id=$1 AND sequence=$2")
                .bind(self.scope.workspace_id).bind(signed(sequence)?).fetch_one(&mut *tx).await.map_err(storage)?).map_err(|_|RoutineOccurrenceError::Unavailable)?;
            let mut occurrence = snapshot(&aggregate, &current)?;
            let latest:i64=sqlx::query_scalar("SELECT revision FROM routine_occurrence_state WHERE workspace_id=$1 AND instance_id=$2")
                .bind(self.scope.workspace_id).bind(aggregate.manifest.id).fetch_one(&mut *tx).await.map_err(storage)?;
            if unsigned(latest)? != aggregate.revision {
                occurrence.fresh_edit_eligible = false;
            }
            let change = RoutineOccurrenceChange {
                sequence,
                occurrence,
            };
            let size = serde_json::to_vec(&change)
                .map_err(|_| RoutineOccurrenceError::Unavailable)?
                .len();
            if size + 1024 > PAGE_BYTES {
                return Err(RoutineOccurrenceError::TooLarge);
            }
            if bytes
                .checked_add(size)
                .is_none_or(|value| value > PAGE_BYTES)
            {
                has_more = true;
                break;
            }
            bytes += size;
            changes.push(change);
        }
        let after = changes
            .last()
            .map_or(checkpoint.after, |change| change.sequence);
        let next = if has_more {
            Cursor {
                after,
                ..checkpoint
            }
        } else {
            Cursor {
                workspace: self.scope.workspace_id,
                list_head: None,
                after: capture_head,
            }
        };
        let page = RoutineOccurrencePage {
            schema_version: 1,
            changes,
            cursor: encode_cursor(&next)?,
            has_more,
        };
        if serde_json::to_vec(&page)
            .map_err(|_| RoutineOccurrenceError::Unavailable)?
            .len()
            > PAGE_BYTES
        {
            return Err(RoutineOccurrenceError::TooLarge);
        }
        tx.commit().await.map_err(storage)?;
        Ok(page)
    }

    pub(crate) async fn planning_evidence(
        &self,
        identities: &[(Uuid, Uuid)],
    ) -> Result<RoutineOccurrencePlanningEvidence, RoutineOccurrenceError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_read(&mut tx, self.scope).await?;
        let result =
            routine_occurrence_planning_evidence_tx(&mut tx, self.scope, identities).await?;
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }
}

pub(crate) async fn lock_routine_occurrence_space(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
) -> Result<(), RoutineOccurrenceError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||$1::text,0))")
        .bind(workspace).execute(&mut **tx).await.map_err(storage)?;
    Ok(())
}

async fn lock_read(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), RoutineOccurrenceError> {
    admit(tx, scope).await?;
    item_repository::lock_execution_item_batch_tx(tx, scope.workspace_id)
        .await
        .map_err(|_| RoutineOccurrenceError::Unavailable)?;
    lock_routine_occurrence_space(tx, scope.workspace_id).await
}

async fn admit(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), RoutineOccurrenceError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1',0))")
        .execute(&mut **tx).await.map_err(storage)?;
    let permitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_members member JOIN workspaces workspace ON workspace.id=member.workspace_id JOIN users owner ON owner.id=workspace.owner_user_id WHERE member.workspace_id=$1 AND member.user_id=$2 AND workspace.owner_user_id=$2 AND member.role='owner' AND member.removed_at IS NULL AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL) AND NOT EXISTS(SELECT 1 FROM account_deletion_fences WHERE workspace_id=$1 OR user_id=$2)")
        .bind(scope.workspace_id).bind(scope.user_id).fetch_one(&mut **tx).await.map_err(storage)?;
    if permitted {
        Ok(())
    } else {
        Err(RoutineOccurrenceError::Unavailable)
    }
}

async fn change_head(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<u64, RoutineOccurrenceError> {
    unsigned(sqlx::query_scalar("SELECT COALESCE(max(sequence),0) FROM routine_occurrence_changes WHERE workspace_id=$1")
        .bind(scope.workspace_id).fetch_one(&mut **tx).await.map_err(storage)?)
}

async fn current_evidence(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<CurrentEvidence, RoutineOccurrenceError> {
    let items = item_repository::list_active_completion_items_tx(tx, scope.workspace_id)
        .await
        .map_err(|_| RoutineOccurrenceError::Unavailable)?;
    let execution_revision = unsigned(
        sqlx::query_scalar(
            "SELECT COALESCE((SELECT revision FROM execution_state WHERE workspace_id=$1),0)",
        )
        .bind(scope.workspace_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)?,
    )?;
    let rows=sqlx::query("SELECT item_id,occurrence_id FROM execution_sessions WHERE workspace_id=$1 AND state IN ('active','paused') ORDER BY id LIMIT 10001")
        .bind(scope.workspace_id).fetch_all(&mut **tx).await.map_err(storage)?;
    if rows.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    let mut live_work_units = BTreeSet::new();
    for row in rows {
        if let Some(occurrence_id) = row
            .try_get::<Option<Uuid>, _>("occurrence_id")
            .map_err(storage)?
        {
            live_work_units.insert(RoutineOccurrenceWorkUnit {
                item_id: row.try_get("item_id").map_err(storage)?,
                occurrence_id,
            });
        }
    }
    let by_id = items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id, index))
        .collect();
    let mut children = BTreeMap::<Uuid, Vec<Uuid>>::new();
    for item in &items {
        if let Some(parent) = item.parent_id {
            children.entry(parent).or_default().push(item.id);
        }
    }
    Ok(CurrentEvidence {
        items,
        by_id,
        children,
        definition_hashes: Mutex::new(BTreeMap::new()),
        execution_revision,
        live_work_units,
    })
}

fn evidence_for(
    manifest: &RoutineOccurrenceManifest,
    current: &CurrentEvidence,
) -> Result<RoutineOccurrenceEvidence, RoutineOccurrenceError> {
    let current_definition_hash = match current.definition_hash(manifest.series_item_id) {
        Ok(value) => value,
        Err(
            RoutineOccurrenceError::SourceIneligible
            | RoutineOccurrenceError::OccurrenceMissing
            | RoutineOccurrenceError::DefinitionChanged
            | RoutineOccurrenceError::TooLarge,
        ) => format!(
            "sha256:{:x}",
            Sha256::digest(b"dayweave.routine-definition.unavailable.v1")
        ),
        Err(error) => return Err(error),
    };
    let sources = manifest
        .members
        .iter()
        .map(|member| {
            let item = current
                .by_id
                .get(&member.item_id)
                .map(|index| &current.items[*index]);
            RoutineOccurrenceSourceEvidence {
                item_id: member.item_id,
                current_revision: item.map(|item| item.revision),
                eligible: item.is_some_and(source_eligible),
            }
        })
        .collect();
    Ok(RoutineOccurrenceEvidence {
        current_definition_hash,
        sources,
        execution_revision: current.execution_revision,
        live_work_units: current.live_work_units.clone(),
    })
}

fn snapshot(
    aggregate: &RoutineOccurrenceAggregate,
    current: &CurrentEvidence,
) -> Result<RoutineOccurrenceSnapshot, RoutineOccurrenceError> {
    let result =
        routine_occurrence_snapshot(aggregate, &evidence_for(&aggregate.manifest, current)?)?;
    bounded_json(&result)?;
    Ok(result)
}

fn source_eligible(item: &Item) -> bool {
    matches!(
        item.status,
        ItemStatus::Inbox | ItemStatus::Planned | ItemStatus::Blocked
    )
}

/// Semantic definition only; mutable title/notes/estimates/lifecycle and
/// instance-local policy are deliberately excluded from this identity.
fn routine_definition_hash(
    subtree: &[&Item],
    root_id: Uuid,
) -> Result<String, RoutineOccurrenceError> {
    let root = subtree
        .iter()
        .find(|item| item.id == root_id)
        .ok_or(RoutineOccurrenceError::OccurrenceMissing)?;
    if !matches!(root.kind, ItemKind::Task | ItemKind::Routine) || root.recurrence.is_none() {
        return Err(RoutineOccurrenceError::SourceIneligible);
    }
    let members=subtree.iter().map(|item|json!({"item_id":item.id,"parent_id":(item.id!=root_id).then_some(item.parent_id).flatten(),"kind":item.kind,"recurrence":item.recurrence,"timezone_name":item.timezone_name,"routine_ordered":item.flexible_constraints.get("routine_ordered").and_then(Value::as_bool).unwrap_or(false),"sibling_order":item.sibling_order})).collect::<Vec<_>>();
    let value = json!({"domain":"dayweave.routine-definition.v1","root_id":root_id,"root_parent_id":root.parent_id,"recurrence":root.recurrence,"timezone_name":root.timezone_name,"members":members});
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(
            serde_json::to_vec(&value).map_err(|_| RoutineOccurrenceError::Unavailable)?
        )
    ))
}

/// Caller owns execution → canonical → habit (when used) → occurrence locks.
/// Missing keys are not yet managed. Present but drifted definitions must never
/// disappear from authoritative evidence as if their outcomes did not exist.
pub(crate) async fn routine_occurrence_planning_evidence_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    identities: &[(Uuid, Uuid)],
) -> Result<RoutineOccurrencePlanningEvidence, RoutineOccurrenceError> {
    if identities.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    let mut unique = BTreeSet::new();
    if identities
        .iter()
        .any(|identity| identity.0.is_nil() || identity.1.is_nil() || !unique.insert(*identity))
    {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let current = current_evidence(tx, scope).await?;
    let mut instances = Vec::new();
    let mut total_bytes = 0_usize;
    let mut total_members = 0_usize;
    for (series, occurrence) in unique {
        let id:Option<Uuid>=sqlx::query_scalar("SELECT id FROM routine_occurrences WHERE workspace_id=$1 AND series_item_id=$2 AND occurrence_id=$3")
            .bind(scope.workspace_id).bind(series).bind(occurrence).fetch_optional(&mut **tx).await.map_err(storage)?;
        let Some(id) = id else { continue };
        let aggregate = read_aggregate(tx, scope, id).await?;
        let evidence = evidence_for(&aggregate.manifest, &current)?;
        if evidence.current_definition_hash != aggregate.manifest.definition_hash {
            return Err(RoutineOccurrenceError::DefinitionChanged);
        }
        if evidence.sources.iter().any(|source| !source.eligible) {
            return Err(RoutineOccurrenceError::SourceIneligible);
        }
        // Snapshot validation checks every current source revision against the
        // immutable first-source revision before exposing planning authority.
        if !routine_occurrence_snapshot(&aggregate, &evidence)?.fresh_edit_eligible {
            return Err(RoutineOccurrenceError::SourceIneligible);
        }
        total_bytes = total_bytes
            .checked_add(
                serde_json::to_vec(&aggregate)
                    .map_err(|_| RoutineOccurrenceError::Unavailable)?
                    .len(),
            )
            .ok_or(RoutineOccurrenceError::TooLarge)?;
        total_members = total_members
            .checked_add(aggregate.members.len())
            .ok_or(RoutineOccurrenceError::TooLarge)?;
        if total_bytes > MAX_ROUTINE_OCCURRENCE_BYTES
            || total_members > MAX_ROUTINE_OCCURRENCE_MEMBERS
        {
            return Err(RoutineOccurrenceError::TooLarge);
        }
        let current_source_revisions = evidence
            .sources
            .into_iter()
            .map(|source| {
                source
                    .current_revision
                    .map(|revision| (source.item_id, revision))
                    .ok_or(RoutineOccurrenceError::SourceIneligible)
            })
            .collect::<Result<_, _>>()?;
        instances.push(RoutineOccurrencePlanningInstance {
            aggregate,
            current_source_revisions,
        });
    }
    Ok(RoutineOccurrencePlanningEvidence {
        change_head: change_head(tx, scope).await?,
        instances,
    })
}

/// Admit complete source subtrees while their exact publication transaction is
/// open. Both new and content-identical publication paths call this helper.
/// Unsupported legacy initial states withhold controls instead of inventing
/// opening status or making unrelated historical schedules unreadable.
#[allow(clippy::too_many_lines)] // Admission and its complete immutable publication witness must be committed together.
pub(crate) async fn record_published_routine_occurrences_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    schedule_revision_id: Uuid,
    result: &ComposeScheduleResult,
    _published_at: DateTime<Utc>,
) -> Result<RoutineOccurrenceAdmission, RoutineOccurrenceError> {
    admit(tx, scope).await?;
    lock_routine_occurrence_space(tx, scope.workspace_id).await?;
    // Pair with the canonical history UPDATE/DELETE statement barrier before
    // reading sources, not merely after we have selected their revisions.
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.item-bootstrap.history-immutability.v1',0))")
        .execute(&mut **tx).await.map_err(storage)?;
    let current = current_evidence(tx, scope).await?;
    let now = database_now(tx).await?;
    let mut admission = RoutineOccurrenceAdmission::default();
    for occurrence in &result.plan.occurrences {
        let root_id = occurrence.series_item_id.0;
        let Some(root) = current
            .by_id
            .get(&root_id)
            .map(|index| &current.items[*index])
        else {
            return Err(RoutineOccurrenceError::SourceIneligible);
        };
        if !matches!(root.kind, ItemKind::Task | ItemKind::Routine) || root.recurrence.is_none() {
            continue;
        }
        let subtree = current.subtree(root_id)?;
        if subtree
            .iter()
            .any(|item| result.source_item_revisions.get(&item.id) != Some(&item.revision))
        {
            return Err(RoutineOccurrenceError::EvidenceStale);
        }
        let definition_hash = current.definition_hash(root_id)?;
        let existing:Option<Uuid>=sqlx::query_scalar("SELECT id FROM routine_occurrences WHERE workspace_id=$1 AND series_item_id=$2 AND occurrence_id=$3")
            .bind(scope.workspace_id).bind(root_id).bind(occurrence.id.0).fetch_optional(&mut **tx).await.map_err(storage)?;
        let instance_id = if let Some(id) = existing {
            let existing = read_aggregate(tx, scope, id).await?;
            if existing.manifest.definition_hash != definition_hash
                || existing.manifest.identity != occurrence.identity
            {
                return Err(RoutineOccurrenceError::DefinitionChanged);
            }
            id
        } else {
            if occurrence.state != dayweave_core::OccurrenceState::Generated
                || subtree.iter().any(|item| !source_eligible(item))
                || current.live_work_units.iter().any(|unit| {
                    subtree.iter().any(|item| item.id == unit.item_id)
                        && unit.occurrence_id == occurrence.id.0
                })
            {
                admission.unsupported_root_ids.insert(root_id);
                continue;
            }
            let ids = subtree.iter().map(|item| item.id).collect::<Vec<_>>();
            let policies = item_completion_repository::load_completion_states_tx(tx, scope, &ids)
                .await
                .map_err(|_| RoutineOccurrenceError::Unavailable)?;
            let members = subtree
                .iter()
                .map(|item| RoutineOccurrenceMemberDefinition {
                    item_id: item.id,
                    parent_id: if item.id == root_id {
                        None
                    } else {
                        item.parent_id
                    },
                    source_revision: item.revision,
                    title: item.title.clone(),
                    kind: item.kind,
                    recurs: item.recurrence.is_some(),
                    sibling_order: item.sibling_order,
                    required_for_parent: policies
                        .get(&item.id)
                        .is_none_or(|state| state.required_for_parent),
                    initial_open: ItemCompletionReopenState {
                        status: item.status,
                        blocked_reason_kind: item.blocked_reason_kind,
                        blocked_by_item_id: item.blocked_by_item_id,
                        blocked_reason: item.blocked_reason.clone(),
                    },
                })
                .collect();
            let id = Uuid::new_v4();
            let manifest = RoutineOccurrenceManifest {
                schema_version: 1,
                id,
                series_item_id: root_id,
                occurrence_id: occurrence.id.0,
                identity: occurrence.identity,
                nominal_start: core_instant(occurrence.nominal_start)?,
                nominal_end: core_instant(occurrence.nominal_end)?,
                window_start: core_instant(occurrence.window_start)?,
                window_end: core_instant(occurrence.window_end)?,
                timezone_name: root.timezone_name.clone(),
                definition_hash,
                members,
            };
            let aggregate = initialize_routine_occurrence(manifest, now)?;
            // Ensure an instance remains readable as one protected snapshot.
            snapshot(&aggregate, &current)?;
            let manifest_json = bounded_json(&aggregate.manifest)?;
            sqlx::query("INSERT INTO routine_occurrences(workspace_id,id,user_id,series_item_id,occurrence_id,definition_hash,manifest_json,member_count,first_schedule_revision_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(scope.workspace_id).bind(id).bind(scope.user_id).bind(root_id).bind(occurrence.id.0).bind(&aggregate.manifest.definition_hash).bind(manifest_json)
                .bind(i32::try_from(aggregate.members.len()).map_err(|_|RoutineOccurrenceError::TooLarge)?).bind(schedule_revision_id).bind(now).execute(&mut **tx).await.map_err(storage)?;
            for member in &aggregate.manifest.members {
                sqlx::query("INSERT INTO routine_occurrence_members(workspace_id,instance_id,item_id,parent_item_id,source_revision) VALUES($1,$2,$3,$4,$5)")
                    .bind(scope.workspace_id).bind(id).bind(member.item_id).bind(member.parent_id).bind(signed(member.source_revision)?).execute(&mut **tx).await.map_err(storage)?;
            }
            write_change(tx, scope, &aggregate, None, None, json!([]), now).await?;
            id
        };
        let source_revisions = subtree
            .iter()
            .map(|item| (item.id, item.revision))
            .collect::<BTreeMap<_, _>>();
        let existing:Option<Value>=sqlx::query_scalar("SELECT source_revisions FROM routine_occurrence_publications WHERE workspace_id=$1 AND instance_id=$2 AND schedule_revision_id=$3")
            .bind(scope.workspace_id).bind(instance_id).bind(schedule_revision_id).fetch_optional(&mut **tx).await.map_err(storage)?;
        let source_json = bounded_json(&source_revisions)?;
        if let Some(existing) = existing {
            if existing != source_json {
                return Err(RoutineOccurrenceError::EvidenceStale);
            }
        } else {
            sqlx::query("INSERT INTO routine_occurrence_publications(workspace_id,instance_id,schedule_revision_id,source_revisions,recorded_at) VALUES($1,$2,$3,$4,$5)")
                .bind(scope.workspace_id).bind(instance_id).bind(schedule_revision_id).bind(source_json).bind(now).execute(&mut **tx).await.map_err(storage)?;
        }
        admission.admitted_instance_ids.push(instance_id);
    }
    Ok(admission)
}

fn core_instant(value: time::OffsetDateTime) -> Result<DateTime<Utc>, RoutineOccurrenceError> {
    if !value.nanosecond().is_multiple_of(1000) {
        return Err(RoutineOccurrenceError::Invalid);
    }
    DateTime::from_timestamp(value.unix_timestamp(), value.nanosecond())
        .ok_or(RoutineOccurrenceError::Invalid)
}

async fn read_aggregate(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    id: Uuid,
) -> Result<RoutineOccurrenceAggregate, RoutineOccurrenceError> {
    let size:Option<i64>=sqlx::query_scalar("SELECT octet_length(aggregate_json::text)::bigint FROM routine_occurrence_state WHERE workspace_id=$1 AND instance_id=$2")
        .bind(scope.workspace_id).bind(id).fetch_optional(&mut **tx).await.map_err(storage)?;
    if size.is_none() {
        return Err(RoutineOccurrenceError::OccurrenceMissing);
    }
    if size.is_some_and(|value| {
        usize::try_from(value)
            .ok()
            .is_none_or(|bytes| bytes > MAX_ROUTINE_OCCURRENCE_BYTES)
    }) {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    let value:Value=sqlx::query_scalar("SELECT aggregate_json FROM routine_occurrence_state WHERE workspace_id=$1 AND instance_id=$2")
        .bind(scope.workspace_id).bind(id).fetch_one(&mut **tx).await.map_err(storage)?;
    let aggregate: RoutineOccurrenceAggregate =
        serde_json::from_value(value).map_err(|_| RoutineOccurrenceError::Unavailable)?;
    aggregate
        .validate()
        .map_err(|_| RoutineOccurrenceError::Unavailable)?;
    if aggregate.manifest.id != id {
        return Err(RoutineOccurrenceError::Unavailable);
    }
    Ok(aggregate)
}

async fn write_change(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    after: &RoutineOccurrenceAggregate,
    before: Option<&RoutineOccurrenceAggregate>,
    operation: Option<Uuid>,
    effects: Value,
    now: DateTime<Utc>,
) -> Result<u64, RoutineOccurrenceError> {
    let aggregate = bounded_json(after)?;
    let previous = before.map(bounded_json).transpose()?;
    let sequence:i64=sqlx::query_scalar("INSERT INTO routine_occurrence_changes(workspace_id,instance_id,revision,operation_id,before_json,aggregate_json,effects_json,changed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING sequence")
        .bind(scope.workspace_id).bind(after.manifest.id).bind(signed(after.revision)?).bind(operation).bind(previous).bind(&aggregate).bind(effects).bind(now)
        .fetch_one(&mut **tx).await.map_err(storage)?;
    sqlx::query("INSERT INTO routine_occurrence_state(workspace_id,instance_id,revision,aggregate_json,updated_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(workspace_id,instance_id) DO UPDATE SET revision=EXCLUDED.revision,aggregate_json=EXCLUDED.aggregate_json,updated_at=EXCLUDED.updated_at")
        .bind(scope.workspace_id).bind(after.manifest.id).bind(signed(after.revision)?).bind(aggregate).bind(now).execute(&mut **tx).await.map_err(storage)?;
    unsigned(sequence)
}

fn bounded_json(value: &impl Serialize) -> Result<Value, RoutineOccurrenceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| RoutineOccurrenceError::Unavailable)?;
    if bytes.len() > MAX_ROUTINE_OCCURRENCE_BYTES {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| RoutineOccurrenceError::Unavailable)
}
fn signed(value: u64) -> Result<i64, RoutineOccurrenceError> {
    i64::try_from(value).map_err(|_| RoutineOccurrenceError::TooLarge)
}
fn unsigned(value: i64) -> Result<u64, RoutineOccurrenceError> {
    u64::try_from(value).map_err(|_| RoutineOccurrenceError::Unavailable)
}
fn storage(_: sqlx::Error) -> RoutineOccurrenceError {
    RoutineOccurrenceError::Unavailable
}
async fn database_now(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<DateTime<Utc>, RoutineOccurrenceError> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)
}
fn encode_cursor(cursor: &Cursor) -> Result<String, RoutineOccurrenceError> {
    Ok(format!(
        "{CURSOR_PREFIX}{}",
        URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(cursor).map_err(|_| RoutineOccurrenceError::Unavailable)?)
    ))
}
fn decode_cursor(value: &str, scope: DatabaseScope) -> Result<Cursor, RoutineOccurrenceError> {
    if value.len() > 512 {
        return Err(RoutineOccurrenceError::InvalidCursor);
    }
    let encoded = value
        .strip_prefix(CURSOR_PREFIX)
        .ok_or(RoutineOccurrenceError::InvalidCursor)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| RoutineOccurrenceError::InvalidCursor)?;
    let cursor: Cursor =
        serde_json::from_slice(&bytes).map_err(|_| RoutineOccurrenceError::InvalidCursor)?;
    if cursor.workspace != scope.workspace_id
        || cursor.after > i64::MAX as u64
        || cursor.list_head.is_some_and(|head| head > i64::MAX as u64)
        || encode_cursor(&cursor)? != value
    {
        return Err(RoutineOccurrenceError::InvalidCursor);
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::NewItem;

    #[test]
    #[allow(clippy::too_many_lines)] // Keep retained proof, oversized current graph and fresh-command rejection in one regression.
    fn historical_instance_stays_readable_after_current_tree_exceeds_admission_limit() {
        let now: DateTime<Utc> = "2026-09-10T00:00:00Z".parse().unwrap();
        let root_id = Uuid::from_u128(1);
        let input: NewItem = serde_json::from_value(json!({
            "id":root_id,"is_sensitive":false,"kind":"routine","status":"planned",
            "title":"Synthetic retained root","notes":null,"timezone_name":"UTC",
            "duration_seconds":null,"deadline_at":null,"earliest_start_at":null,
            "recurrence":{"type":"daily","times_per_day":1},"flexible_constraints":{},
            "split_policy":{"type":"indivisible"},"importance":1,"urgency":1,
            "parent_id":null,"sibling_order":0
        }))
        .unwrap();
        let root = Item::new(input, now).unwrap();
        let open = ItemCompletionReopenState {
            status: ItemStatus::Planned,
            blocked_reason_kind: None,
            blocked_by_item_id: None,
            blocked_reason: None,
        };
        let manifest = RoutineOccurrenceManifest {
            schema_version: 1,
            id: Uuid::from_u128(50_001),
            series_item_id: root_id,
            occurrence_id: "10000000-0000-5000-8000-000000000001".parse().unwrap(),
            identity: dayweave_core::RecurrenceOccurrenceIdentity::CalendarDay {
                date: time::Date::from_calendar_date(2026, time::Month::September, 10).unwrap(),
                bucket_ordinal: 0,
            },
            nominal_start: now,
            nominal_end: now + chrono::Duration::days(1),
            window_start: now,
            window_end: now + chrono::Duration::days(1),
            timezone_name: "UTC".into(),
            definition_hash: routine_definition_hash(&[&root], root_id).unwrap(),
            members: vec![RoutineOccurrenceMemberDefinition {
                item_id: root_id,
                parent_id: None,
                source_revision: 1,
                title: root.title.clone(),
                kind: ItemKind::Routine,
                recurs: true,
                sibling_order: 0,
                required_for_parent: true,
                initial_open: open,
            }],
        };
        let aggregate = initialize_routine_occurrence(manifest, now).unwrap();
        let mut items = vec![root.clone()];
        for index in 0..MAX_ROUTINE_OCCURRENCE_MEMBERS {
            let mut child = root.clone();
            child.id = Uuid::from_u128(index as u128 + 2);
            child.parent_id = Some(root_id);
            child.kind = ItemKind::Task;
            child.recurrence = None;
            items.push(child);
        }
        let current = CurrentEvidence {
            by_id: items
                .iter()
                .enumerate()
                .map(|(index, item)| (item.id, index))
                .collect(),
            children: BTreeMap::from([(
                root_id,
                items.iter().skip(1).map(|item| item.id).collect(),
            )]),
            items,
            definition_hashes: Mutex::new(BTreeMap::new()),
            execution_revision: 0,
            live_work_units: BTreeSet::new(),
        };
        assert_eq!(
            current.definition_hash(root_id).unwrap_err(),
            RoutineOccurrenceError::TooLarge
        );
        let retained = snapshot(&aggregate, &current).unwrap();
        assert_eq!(retained.aggregate, aggregate);
        assert!(!retained.fresh_edit_eligible);
        let command:RoutineOccurrenceCommand=serde_json::from_value(json!({
            "schema_version":1,"operation_id":Uuid::from_u128(50_002),"expected_instance_revision":1,
            "expected_member_revision":1,"expected_evidence_hash":retained.evidence_hash,
            "action":{"type":"set_outcome","status":"completed"}
        })).unwrap();
        assert_eq!(
            plan_routine_occurrence(
                &aggregate,
                &evidence_for(&aggregate.manifest, &current).unwrap(),
                root_id,
                &command,
                now
            )
            .unwrap_err(),
            RoutineOccurrenceError::DefinitionChanged
        );
    }
}
