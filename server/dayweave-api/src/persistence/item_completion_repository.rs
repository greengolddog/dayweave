//! Atomic completion projection shared by every canonical writer.
//!
//! Callers own execution state before canonical workspace/row locks and close
//! the primary delta group before entering this finalizer. No partial forest,
//! background repair, or direct-operation receipt substitution is permitted.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Timelike as _, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{DatabaseScope, item_repository};
use crate::{
    item_completion::{
        ItemCompletionCommand, ItemCompletionEffect, ItemCompletionError,
        ItemCompletionExecutionEvidence, ItemCompletionMode, ItemCompletionMutation,
        ItemCompletionPlan, ItemCompletionSnapshot, ItemCompletionState, plan_item_completion,
    },
    items::{Item, ItemRepositoryError, ItemStatus},
};

#[derive(Clone, Debug)]
pub(crate) struct CompletionForestSnapshot {
    pub items: Vec<Item>,
    pub states: BTreeMap<Uuid, ItemCompletionState>,
    pub execution: ItemCompletionExecutionEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionDeliveryMode {
    Committed,
    Preview,
}

#[derive(Clone, Debug)]
pub(crate) struct ItemCompletionFinalization {
    pub effects: Vec<ItemCompletionEffect>,
    pub evaluated_item_ids: BTreeSet<Uuid>,
}

pub(super) async fn initialize(
    pool: &PgPool,
    scope: DatabaseScope,
) -> Result<(), ItemCompletionError> {
    let mut tx = pool.begin().await.map_err(storage)?;
    admit(&mut tx, scope).await?;
    item_repository::lock_execution_item_batch_tx(&mut tx, scope.workspace_id)
        .await
        .map_err(item_error)?;
    let before = capture_completion_before_tx(&mut tx, scope).await?;
    let now = database_now(&mut tx).await?;
    finalize_item_completion_tx(
        &mut tx,
        scope,
        &before,
        now,
        CompletionDeliveryMode::Committed,
    )
    .await?;
    tx.commit().await.map_err(storage)
}

pub(super) async fn get(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
) -> Result<ItemCompletionSnapshot, ItemCompletionError> {
    let mut tx = pool.begin().await.map_err(storage)?;
    admit(&mut tx, scope).await?;
    item_repository::lock_execution_item_batch_tx(&mut tx, scope.workspace_id)
        .await
        .map_err(item_error)?;
    let current = capture_completion_before_tx(&mut tx, scope).await?;
    let now = database_now(&mut tx).await?;
    let snapshot = read_snapshot(&current, item_id, now)?;
    tx.commit().await.map_err(storage)?;
    Ok(snapshot)
}

pub(super) async fn put(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    command: ItemCompletionCommand,
    actor_session_id: Option<Uuid>,
) -> Result<ItemCompletionMutation, ItemCompletionError> {
    let mut tx = pool.begin().await.map_err(storage)?;
    admit(&mut tx, scope).await?;
    item_repository::lock_execution_item_batch_tx(&mut tx, scope.workspace_id)
        .await
        .map_err(item_error)?;
    let request = json(&command)?;
    if let Some(row) = sqlx::query("SELECT item_id,request_json,result_json FROM item_completion_operations WHERE workspace_id=$1 AND operation_id=$2")
        .bind(scope.workspace_id).bind(command.operation_id).fetch_optional(&mut *tx).await.map_err(storage)?
    {
        let stored_id: Uuid = row.try_get("item_id").map_err(storage)?;
        let stored_request: Value = row.try_get("request_json").map_err(storage)?;
        if stored_id != item_id || stored_request != request { return Err(ItemCompletionError::OperationReused); }
        let completion = serde_json::from_value(row.try_get("result_json").map_err(storage)?)
            .map_err(|_| ItemCompletionError::Unavailable)?;
        tx.commit().await.map_err(storage)?;
        return Ok(ItemCompletionMutation { operation_id: command.operation_id, replayed: true, completion });
    }
    command.validate(item_id)?;
    let current = capture_completion_before_tx(&mut tx, scope).await?;
    let states = current.states.values().cloned().collect::<Vec<_>>();
    let now = database_now(&mut tx).await?;
    let plan = plan_item_completion(
        &current.items,
        &states,
        &current.execution,
        Some((item_id, &command)),
        now,
    )?;
    let evaluation_id = persist_completion_plan_tx(
        &mut tx,
        scope,
        &plan,
        current.execution.revision,
        now,
        CompletionDeliveryMode::Committed,
        Some(command.operation_id),
    )
    .await?;
    let after = capture_completion_before_tx(&mut tx, scope).await?;
    let completion = read_snapshot(&after, item_id, now)?;
    sqlx::query("INSERT INTO item_completion_operations (workspace_id,operation_id,item_id,actor_user_id,actor_session_id,evaluation_id,request_json,result_json,recorded_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(scope.workspace_id).bind(command.operation_id).bind(item_id).bind(scope.user_id)
        .bind(actor_session_id).bind(evaluation_id.ok_or(ItemCompletionError::Unavailable)?)
        .bind(request).bind(json(&completion)?).bind(now).execute(&mut *tx).await.map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(ItemCompletionMutation {
        operation_id: command.operation_id,
        replayed: false,
        completion,
    })
}

fn read_snapshot(
    current: &CompletionForestSnapshot,
    item_id: Uuid,
    now: DateTime<Utc>,
) -> Result<ItemCompletionSnapshot, ItemCompletionError> {
    if !current.items.iter().any(|item| item.id == item_id) {
        return Err(ItemCompletionError::ItemMissing);
    }
    plan_item_completion(
        &current.items,
        &current.states.values().cloned().collect::<Vec<_>>(),
        &current.execution,
        None,
        now,
    )?
    .snapshots
    .remove(&item_id)
    .ok_or(ItemCompletionError::ItemMissing)
}

pub(crate) async fn load_completion_states_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_ids: &[Uuid],
) -> Result<BTreeMap<Uuid, ItemCompletionState>, ItemCompletionError> {
    let mut states = item_ids
        .iter()
        .map(|id| (*id, ItemCompletionState::empty(*id)))
        .collect::<BTreeMap<_, _>>();
    for chunk in item_ids.chunks(1_000) {
        let rows = sqlx::query("SELECT item_id,revision,state_json,updated_at FROM item_completion_state WHERE workspace_id=$1 AND item_id=ANY($2) ORDER BY item_id")
            .bind(scope.workspace_id).bind(chunk).fetch_all(&mut **tx).await.map_err(storage)?;
        for row in rows {
            let id: Uuid = row.try_get("item_id").map_err(storage)?;
            let revision = unsigned(row.try_get("revision").map_err(storage)?)?;
            let updated_at: DateTime<Utc> = row.try_get("updated_at").map_err(storage)?;
            let state: ItemCompletionState =
                serde_json::from_value(row.try_get("state_json").map_err(storage)?)
                    .map_err(|_| ItemCompletionError::Unavailable)?;
            state
                .validate()
                .map_err(|_| ItemCompletionError::Unavailable)?;
            if state.item_id != id
                || state.revision != revision
                || state.updated_at != Some(updated_at)
            {
                return Err(ItemCompletionError::Unavailable);
            }
            states.insert(id, state);
        }
    }
    Ok(states)
}

pub(crate) fn has_qualified_completion(item: &Item, state: Option<&ItemCompletionState>) -> bool {
    item.status == ItemStatus::Completed
        && state.is_some_and(|state| {
            state.item_id == item.id && state.validate().is_ok() && state.provenance.is_some()
        })
}

pub(crate) async fn has_qualified_completion_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<bool, ItemCompletionError> {
    let state: Option<Value> = sqlx::query_scalar(
        "SELECT state_json FROM item_completion_state WHERE workspace_id=$1 AND item_id=$2",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    let Some(state) = state else {
        return Ok(false);
    };
    let state: ItemCompletionState =
        serde_json::from_value(state).map_err(|_| ItemCompletionError::Unavailable)?;
    state
        .validate()
        .map_err(|_| ItemCompletionError::Unavailable)?;
    Ok(state.item_id == item_id && state.provenance.is_some())
}

pub(crate) async fn ensure_legacy_completion_transition_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    current: &Item,
    replacement: &Item,
) -> Result<(), ItemCompletionError> {
    if current.status == replacement.status {
        return Ok(());
    }
    let state: Option<Value> = sqlx::query_scalar(
        "SELECT state_json FROM item_completion_state WHERE workspace_id=$1 AND item_id=$2",
    )
    .bind(workspace_id)
    .bind(current.id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    if let Some(state) = state {
        let state: ItemCompletionState =
            serde_json::from_value(state).map_err(|_| ItemCompletionError::Unavailable)?;
        state
            .validate()
            .map_err(|_| ItemCompletionError::Unavailable)?;
        if state.mode != ItemCompletionMode::Automatic || state.provenance.is_some() {
            return Err(ItemCompletionError::ReopeningReviewRequired);
        }
    }
    Ok(())
}

pub(crate) async fn capture_completion_before_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<CompletionForestSnapshot, ItemCompletionError> {
    let items = item_repository::list_active_completion_items_tx(tx, scope.workspace_id)
        .await
        .map_err(item_error)?;
    let ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
    let states = load_completion_states_tx(tx, scope, &ids).await?;
    let row =
        sqlx::query("SELECT revision,active_session_id FROM execution_state WHERE workspace_id=$1")
            .bind(scope.workspace_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(storage)?;
    let revision = unsigned(row.try_get("revision").map_err(storage)?)?;
    let active: Option<Uuid> = row.try_get("active_session_id").map_err(storage)?;
    let mut live_item_ids = BTreeSet::new();
    if let Some(id) = active {
        let session = sqlx::query(
            "SELECT item_id,state FROM execution_sessions WHERE workspace_id=$1 AND id=$2",
        )
        .bind(scope.workspace_id)
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)?;
        let state: String = session.try_get("state").map_err(storage)?;
        if !matches!(state.as_str(), "active" | "paused") {
            return Err(ItemCompletionError::Unavailable);
        }
        live_item_ids.insert(session.try_get("item_id").map_err(storage)?);
    }
    Ok(CompletionForestSnapshot {
        items,
        states,
        execution: ItemCompletionExecutionEvidence {
            revision,
            live_item_ids,
        },
    })
}

pub(crate) async fn finalize_item_completion_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    _before: &CompletionForestSnapshot,
    now: DateTime<Utc>,
    delivery: CompletionDeliveryMode,
) -> Result<ItemCompletionFinalization, ItemCompletionError> {
    let now = completion_storage_instant(now);
    require_closed_primary_group(tx).await?;
    let current = capture_completion_before_tx(tx, scope).await?;
    let states = current.states.values().cloned().collect::<Vec<_>>();
    let plan = plan_item_completion(&current.items, &states, &current.execution, None, now)?;
    persist_completion_plan_tx(
        tx,
        scope,
        &plan,
        current.execution.revision,
        now,
        delivery,
        None,
    )
    .await?;
    Ok(ItemCompletionFinalization {
        effects: plan.effects,
        evaluated_item_ids: plan.evaluated_item_ids,
    })
}

async fn require_closed_primary_group(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), ItemCompletionError> {
    let group: Option<String> = sqlx::query_scalar(
        "SELECT NULLIF(current_setting('dayweave.item_change_group_id',true),'')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    if group.is_some() {
        Err(ItemCompletionError::Unavailable)
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_completion_plan_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    plan: &ItemCompletionPlan,
    execution_revision: u64,
    now: DateTime<Utc>,
    delivery: CompletionDeliveryMode,
    operation_id: Option<Uuid>,
) -> Result<Option<Uuid>, ItemCompletionError> {
    require_closed_primary_group(tx).await?;
    if plan.effects.is_empty() {
        return Ok(None);
    }
    let evidence_hash = &plan
        .snapshots
        .values()
        .next()
        .ok_or(ItemCompletionError::Unavailable)?
        .evidence_hash;
    let evaluation_id = insert_evaluation(
        tx,
        scope,
        if operation_id.is_some() {
            "policy_command"
        } else {
            "canonical_write"
        },
        operation_id,
        evidence_hash,
        execution_revision,
        plan.effects.len(),
        now,
    )
    .await?;
    let mut group = None;
    let mut group_count = 0_usize;
    let mut group_bytes = 0_usize;
    for chunk in plan
        .effects
        .chunks(crate::items::MAX_ITEM_CHANGE_GROUP_SIZE)
    {
        // Ask PostgreSQL for its actual jsonb text size, matching delivery
        // guards; payloads remain bounded by the admitted complete forest.
        let payloads = chunk
            .iter()
            .map(|effect| json(&effect.after_item))
            .collect::<Result<Vec<_>, _>>()?;
        let sizes: Vec<i64> = sqlx::query_scalar("SELECT octet_length(value::text)::bigint FROM jsonb_array_elements($1::jsonb) WITH ORDINALITY ORDER BY ordinality")
            .bind(Value::Array(payloads)).fetch_all(&mut **tx).await.map_err(storage)?;
        if sizes.len() != chunk.len() {
            return Err(ItemCompletionError::Unavailable);
        }
        for (effect, bytes) in chunk.iter().zip(sizes) {
            let bytes = usize::try_from(bytes)
                .map_err(|_| ItemCompletionError::Unavailable)?
                .checked_add(if delivery == CompletionDeliveryMode::Preview {
                    1_024
                } else {
                    0
                })
                .ok_or(ItemCompletionError::TooLarge)?;
            if bytes > crate::items::MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES {
                return Err(ItemCompletionError::TooLarge);
            }
            if group_count == crate::items::MAX_ITEM_CHANGE_GROUP_SIZE
                || group_bytes
                    .checked_add(bytes)
                    .ok_or(ItemCompletionError::TooLarge)?
                    > crate::items::MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES
            {
                close_group(
                    tx,
                    scope,
                    group.take().ok_or(ItemCompletionError::Unavailable)?,
                    delivery,
                )
                .await?;
                group_count = 0;
                group_bytes = 0;
            }
            if group.is_none() {
                group = Some(
                    item_repository::start_item_change_group_tx(tx)
                        .await
                        .map_err(item_error)?,
                );
            }
            item_repository::update_item(tx, scope.workspace_id, &effect.after_item)
                .await
                .map_err(item_error)?;
            item_repository::record_mutation(
                tx,
                scope,
                &effect.after_item,
                "item.completion_changed",
                Some(effect.before_item.revision),
                item_repository::ChangeKind::Upsert,
            )
            .await
            .map_err(item_error)?;
            persist_state_effect(tx, scope, evaluation_id, effect, now).await?;
            group_count += 1;
            group_bytes += bytes;
        }
    }
    if let Some(group) = group {
        close_group(tx, scope, group, delivery).await?;
    }
    Ok(Some(evaluation_id))
}

async fn close_group(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    group: Uuid,
    delivery: CompletionDeliveryMode,
) -> Result<(), ItemCompletionError> {
    match delivery {
        CompletionDeliveryMode::Committed => {
            item_repository::validate_item_change_group_tx(tx, scope.workspace_id, group).await
        }
        CompletionDeliveryMode::Preview => {
            item_repository::validate_preview_item_change_group_tx(tx, scope.workspace_id, group)
                .await
        }
    }
    .map_err(item_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_evaluation(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    cause_kind: &str,
    cause_id: Option<Uuid>,
    evidence_hash: &str,
    execution_revision: u64,
    effect_count: usize,
    now: DateTime<Utc>,
) -> Result<Uuid, ItemCompletionError> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO item_completion_evaluations (workspace_id,evaluation_id,cause_kind,cause_id,evidence_hash,execution_revision,effect_count,recorded_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(scope.workspace_id).bind(id).bind(cause_kind).bind(cause_id).bind(evidence_hash)
        .bind(revision(execution_revision)?).bind(i32::try_from(effect_count).map_err(|_| ItemCompletionError::TooLarge)?)
        .bind(now).execute(&mut **tx).await.map_err(storage)?;
    Ok(id)
}

async fn persist_state_effect(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    evaluation_id: Uuid,
    effect: &ItemCompletionEffect,
    now: DateTime<Utc>,
) -> Result<(), ItemCompletionError> {
    let updated = if effect.before_state.revision == 0 {
        sqlx::query("INSERT INTO item_completion_state (workspace_id,item_id,revision,state_json,updated_at) VALUES ($1,$2,$3,$4,$5)")
            .bind(scope.workspace_id).bind(effect.after_item.id).bind(revision(effect.after_state.revision)?)
            .bind(json(&effect.after_state)?).bind(now).execute(&mut **tx).await.map_err(storage)?.rows_affected()
    } else {
        sqlx::query("UPDATE item_completion_state SET revision=$3,state_json=$4,updated_at=$5 WHERE workspace_id=$1 AND item_id=$2 AND revision=$6 AND state_json=$7")
            .bind(scope.workspace_id).bind(effect.after_item.id).bind(revision(effect.after_state.revision)?)
            .bind(json(&effect.after_state)?).bind(now).bind(revision(effect.before_state.revision)?)
            .bind(json(&effect.before_state)?).execute(&mut **tx).await.map_err(storage)?.rows_affected()
    };
    if updated != 1 {
        return Err(ItemCompletionError::Unavailable);
    }
    sqlx::query("INSERT INTO item_completion_effects (workspace_id,evaluation_id,item_id,before_item_revision,after_item_revision,completion_revision,before_state_json,after_state_json,reason,recorded_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(scope.workspace_id).bind(evaluation_id).bind(effect.after_item.id)
        .bind(revision(effect.before_item.revision)?).bind(revision(effect.after_item.revision)?)
        .bind(revision(effect.after_state.revision)?).bind(json(&effect.before_state)?)
        .bind(json(&effect.after_state)?).bind(effect.reason).bind(now)
        .execute(&mut **tx).await.map_err(storage)?;
    Ok(())
}

/// Restores only a trusted proposal inverse's semantic state. Its canonical
/// inverse has already advanced and emitted its own primary-group delta; this
/// companion never changes that receipt, resets revisions, or owns a group.
pub(crate) async fn restore_completion_semantics_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    before_item: &Item,
    after_item: &Item,
    snapshot: &ItemCompletionState,
    now: DateTime<Utc>,
) -> Result<(), ItemCompletionError> {
    let now = completion_storage_instant(now);
    snapshot.validate()?;
    if snapshot.item_id != after_item.id
        || before_item.id != after_item.id
        || before_item.revision.checked_add(1) != Some(after_item.revision)
    {
        return Err(ItemCompletionError::Invalid);
    }
    let before_state = load_completion_states_tx(tx, scope, &[after_item.id])
        .await?
        .remove(&after_item.id)
        .ok_or(ItemCompletionError::Unavailable)?;
    let mut after_state = snapshot.clone();
    after_state.revision = before_state.revision;
    after_state.updated_at = before_state.updated_at;
    if after_state == before_state {
        return Ok(());
    }
    if after_state.provenance.is_some() && after_item.status != ItemStatus::Completed {
        return Err(ItemCompletionError::ReopeningReviewRequired);
    }
    after_state.revision = before_state
        .revision
        .checked_add(1)
        .ok_or(ItemCompletionError::Unavailable)?;
    after_state.updated_at = Some(now);
    after_state.validate()?;
    let execution_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM execution_state WHERE workspace_id=$1")
            .bind(scope.workspace_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(storage)?;
    let evidence = serde_json::to_vec(&(
        "dayweave.completion.snapshot-restore.v1",
        before_item,
        after_item,
        &before_state,
        &after_state,
    ))
    .map_err(|_| ItemCompletionError::Unavailable)?;
    let hash = Sha256::digest(evidence);
    let id = insert_evaluation(
        tx,
        scope,
        "snapshot_restore",
        None,
        &format!("sha256:{hash:x}"),
        unsigned(execution_revision)?,
        1,
        now,
    )
    .await?;
    let effect = ItemCompletionEffect {
        before_item: before_item.clone(),
        after_item: after_item.clone(),
        before_state,
        after_state,
        reason: "snapshot_restored",
    };
    persist_state_effect(tx, scope, id, &effect, now).await
}

fn completion_storage_instant(value: DateTime<Utc>) -> DateTime<Utc> {
    value
        .with_nanosecond(value.nanosecond() / 1_000 * 1_000)
        .unwrap_or(value)
}

async fn admit(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), ItemCompletionError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1',0))")
        .execute(&mut **tx).await.map_err(storage)?;
    let permitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_members member JOIN workspaces workspace ON workspace.id=member.workspace_id JOIN users owner ON owner.id=workspace.owner_user_id WHERE member.workspace_id=$1 AND member.user_id=$2 AND workspace.owner_user_id=$2 AND member.role='owner' AND member.removed_at IS NULL AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL) AND NOT EXISTS(SELECT 1 FROM account_deletion_fences WHERE workspace_id=$1 OR user_id=$2)")
        .bind(scope.workspace_id).bind(scope.user_id).fetch_one(&mut **tx).await.map_err(storage)?;
    if permitted {
        Ok(())
    } else {
        Err(ItemCompletionError::Unavailable)
    }
}

async fn database_now(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<DateTime<Utc>, ItemCompletionError> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)
}

fn json(value: &impl serde::Serialize) -> Result<Value, ItemCompletionError> {
    serde_json::to_value(value).map_err(|_| ItemCompletionError::Unavailable)
}

pub(crate) fn completion_error(error: ItemCompletionError) -> ItemRepositoryError {
    match error {
        ItemCompletionError::TooLarge => ItemRepositoryError::DeltaGroupTooLarge,
        ItemCompletionError::ReopeningReviewRequired
        | ItemCompletionError::ParentRequired
        | ItemCompletionError::OccurrenceEvidenceRequired => {
            ItemRepositoryError::InvalidParentState
        }
        _ => ItemRepositoryError::Internal,
    }
}

fn item_error(error: ItemRepositoryError) -> ItemCompletionError {
    let mapped = match &error {
        ItemRepositoryError::DeltaGroupTooLarge => ItemCompletionError::TooLarge,
        _ => ItemCompletionError::Unavailable,
    };
    drop(error);
    mapped
}
fn storage(_: sqlx::Error) -> ItemCompletionError {
    ItemCompletionError::Unavailable
}
fn revision(value: u64) -> Result<i64, ItemCompletionError> {
    i64::try_from(value).map_err(|_| ItemCompletionError::Unavailable)
}
fn unsigned(value: i64) -> Result<u64, ItemCompletionError> {
    u64::try_from(value).map_err(|_| ItemCompletionError::Unavailable)
}
