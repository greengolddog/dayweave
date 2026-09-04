use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dayweave_core::{ConstraintStrength, Dependency, DependencyRelation};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::items::{
    BlockedReasonKind, DeadlineKind, DeadlineStrength, DeltaChange, DurationKind, DurationSource,
    IdempotencyContext, Item, ItemDeltaPage, ItemKind, ItemMutation, ItemQuery, ItemRepository,
    ItemRepositoryError, ItemStatus, ItemTombstone, MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES,
    MAX_ITEM_CHANGE_GROUP_SIZE, NewItem, ReplaceItem, SplitPolicy,
    delivery_bounded_delta_prefix_len, max_expanded_delta_page_size,
};

use super::{DatabaseScope, database::lock_canonical_item_space};

const ITEM_SELECT: &str = "SELECT item.id, item.is_sensitive, item.kind, item.status, item.title, item.notes, item.timezone_name, \
     item.duration_kind, item.duration_seconds, item.duration_min_seconds, item.duration_max_seconds, \
     item.duration_source, item.deadline_kind, item.deadline_date, item.deadline_at, \
     item.deadline_strength, item.deadline_soft_weight, item.earliest_start_at, item.recurrence, \
     item.scheduling_constraints, item.has_own_effort, item.split_allowed, item.minimum_chunk_seconds, \
     item.maximum_chunk_seconds, item.importance, item.urgency, item.revision, \
     item.created_at, item.updated_at, item.completed_at, item.trashed_at, \
     item.blocked_reason_kind, item.blocked_by_item_id, item.blocked_reason, \
     hierarchy.parent_item_id, \
     CASE WHEN hierarchy.child_item_id IS NULL THEN item.sibling_order ELSE hierarchy.position END \
         AS effective_sibling_order, \
     EXISTS (SELECT 1 FROM item_hierarchy AS child_edge \
         JOIN items AS child ON child.workspace_id = child_edge.workspace_id \
             AND child.id = child_edge.child_item_id \
         WHERE child_edge.workspace_id = item.workspace_id \
             AND child_edge.parent_item_id = item.id AND child.trashed_at IS NULL) AS has_children, \
     COALESCE((SELECT jsonb_agg(jsonb_build_object( \
             'item_id', dependency.predecessor_item_id, \
             'relation', dependency.dependency_kind, \
             'minimum_lag', dependency.lag_seconds / 60, \
             'strength', CASE WHEN dependency.dependency_strength = 'hard' \
                 THEN jsonb_build_object('level', 'hard') \
                 ELSE jsonb_build_object('level', 'soft', 'weight', dependency.dependency_soft_weight) END \
         ) ORDER BY dependency.projection_ordinal, dependency.predecessor_item_id) \
         FROM item_dependencies AS dependency \
         WHERE dependency.workspace_id = item.workspace_id \
           AND dependency.successor_item_id = item.id), '[]'::jsonb) AS authoritative_dependencies \
     FROM items AS item LEFT JOIN item_hierarchy AS hierarchy \
       ON hierarchy.workspace_id = item.workspace_id AND hierarchy.child_item_id = item.id";

const ITEM_CHANGE_GROUP_SETTING: &str = "dayweave.item_change_group_id";
// Preview/apply state, command content, UUIDs, revisions, row count, and row
// order are hash- or fence-bound. Only generated mutation timestamps can differ
// at a later apply/undo. Deadline, earliest-start, and recurrence times are
// command-bound; the four mutation-owned RFC 3339 fields can each gain at most
// seven bytes from a canonical microsecond fraction. One KiB per emitted row
// therefore leaves more than 36x the maximum dynamic growth.
const PREVIEW_ITEM_CHANGE_ROW_RESERVE_BYTES: i64 = 1024;

#[derive(Clone, Debug)]
pub struct PostgresItemRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

/// A canonical item command executed as part of a caller-owned `PostgreSQL`
/// transaction. Proposal application uses this boundary so a group of changes
/// can commit or roll back as one unit without nesting per-item transactions.
#[derive(Clone, Debug)]
pub(crate) enum TransactionalItemCommand {
    Create(NewItem),
    Replace {
        item_id: Uuid,
        expected_revision: u64,
        replacement: ReplaceItem,
    },
    Trash {
        item_id: Uuid,
        expected_revision: u64,
    },
    Restore {
        item_id: Uuid,
        expected_revision: u64,
    },
    /// Restores a trusted database snapshot while still advancing the
    /// canonical revision. This is intentionally unavailable to API callers;
    /// proposal undo uses it to recover fields such as the original
    /// `completed_at` and tombstone timestamp that `ReplaceItem` cannot carry.
    RestoreSnapshot {
        item_id: Uuid,
        expected_revision: u64,
        snapshot: Box<Item>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct TransactionalItemEffect {
    pub before: Option<Item>,
    pub after: Item,
    /// Zero-based order in which this command actually mutated the batch.
    /// Proposal review remains in submitted order, while undo reverses this
    /// execution order so parent/child staging is always unwound safely.
    pub execution_ordinal: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransactionalGraphMode {
    Immediate,
    Deferred,
    DeferredWithStagedCreates,
}

impl TransactionalGraphMode {
    const fn validates_immediately(self) -> bool {
        matches!(self, Self::Immediate)
    }

    const fn uses_staged_creates(self) -> bool {
        matches!(self, Self::DeferredWithStagedCreates)
    }
}

impl PostgresItemRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }
}

#[async_trait]
impl ItemRepository for PostgresItemRepository {
    fn cursor_scope(&self) -> Uuid {
        self.scope.workspace_id
    }

    #[allow(clippy::too_many_lines)] // Keeps the full locked create, grouped delta, and idempotency envelope visible.
    async fn create(
        &self,
        item: Item,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if let Reservation::Replay(item) =
            reserve_idempotency(&mut transaction, self.scope, &idempotency).await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(ItemMutation {
                item: *item,
                replayed: true,
            });
        }
        // Execution Start takes the execution-state lock before the canonical
        // item-space lock. Adding a child must take the same order because it
        // makes the parent non-executable.
        let active_execution = if item.parent_id.is_some() {
            lock_active_execution(&mut transaction, self.scope.workspace_id).await?
        } else {
            None
        };
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        validate_parent(
            &mut transaction,
            self.scope.workspace_id,
            item.id,
            item.parent_id,
        )
        .await?;
        if let Some(parent_id) = item.parent_id
            && let Some((session_id, active_item_id)) = active_execution
            && active_item_id == parent_id
        {
            return Err(ItemRepositoryError::ActiveExecutionConflict {
                item_id: parent_id,
                session_id,
            });
        }
        validate_blocked_by(
            &mut transaction,
            self.scope.workspace_id,
            item.blocked_by_item_id,
        )
        .await?;
        insert_item(&mut transaction, self.scope, &item).await?;
        persist_item_dependency_edges(&mut transaction, self.scope.workspace_id, &item).await?;
        replace_hierarchy_edge(
            &mut transaction,
            self.scope.workspace_id,
            item.id,
            item.parent_id,
            item.sibling_order,
        )
        .await?;
        validate_dependency_graph_tx(&mut transaction, self.scope.workspace_id).await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, item.id, false)
                .await?;
        let change_group_id = start_item_change_group_tx(&mut transaction).await?;
        record_mutation(
            &mut transaction,
            self.scope,
            &item,
            "item.created",
            None,
            ChangeKind::Upsert,
        )
        .await?;
        refresh_parents(
            &mut transaction,
            self.scope,
            [item.parent_id],
            item.updated_at,
        )
        .await?;
        validate_item_change_group_tx(&mut transaction, self.scope.workspace_id, change_group_id)
            .await?;
        complete_idempotency(&mut transaction, self.scope, &idempotency, &item).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ItemMutation {
            item,
            replayed: false,
        })
    }

    async fn get(&self, id: Uuid, include_deleted: bool) -> Result<Item, ItemRepositoryError> {
        let mut builder = QueryBuilder::<Postgres>::new(ITEM_SELECT);
        builder
            .push(" WHERE item.workspace_id = ")
            .push_bind(self.scope.workspace_id)
            .push(" AND item.id = ")
            .push_bind(id);
        if !include_deleted {
            builder.push(" AND item.trashed_at IS NULL");
        }
        let row = builder
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or(ItemRepositoryError::NotFound(id))?;
        item_from_row(&row)
    }

    async fn list(&self, query: ItemQuery) -> Result<Vec<Item>, ItemRepositoryError> {
        let mut builder = QueryBuilder::<Postgres>::new(ITEM_SELECT);
        builder
            .push(" WHERE item.workspace_id = ")
            .push_bind(self.scope.workspace_id);
        if !query.include_deleted {
            builder.push(" AND item.trashed_at IS NULL");
        }
        if let Some(parent_id) = query.parent_id {
            builder
                .push(" AND hierarchy.parent_item_id = ")
                .push_bind(parent_id);
        }
        let limit = i64::try_from(query.limit).map_err(|_| ItemRepositoryError::Internal)?;
        builder
            .push(" ORDER BY hierarchy.parent_item_id NULLS FIRST, ")
            .push("effective_sibling_order, item.created_at, item.id LIMIT ")
            .push_bind(limit);
        builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?
            .iter()
            .map(item_from_row)
            .collect()
    }

    #[allow(clippy::too_many_lines)] // Keeps the locked replace, hierarchy refreshes, and grouped delta envelope visible.
    async fn replace(
        &self,
        id: Uuid,
        expected_revision: u64,
        replacement: ReplaceItem,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if let Reservation::Replay(item) =
            reserve_idempotency(&mut transaction, self.scope, &idempotency).await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(ItemMutation {
                item: *item,
                replayed: true,
            });
        }
        // Execution Start locks `execution_state` before canonical items. A state that prevents
        // execution must take the same order so either Start observes that state or this
        // replacement observes the open lease, without an item/state deadlock. Exact idempotency
        // replays intentionally return before taking this current-state guard.
        let active_execution = if replacement.status.prevents_execution()
            || replacement.may_remove_executable_component()
            || replacement.parent_id.is_some()
        {
            lock_active_execution(&mut transaction, self.scope.workspace_id).await?
        } else {
            None
        };
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        let current =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
        ensure_revision(&current, expected_revision)?;
        let previous_parent_id = current.parent_id;
        let previous_sibling_order = current.sibling_order;
        let item = current.replaced(replacement, now)?;
        if ((!current.status.prevents_execution() && item.status.prevents_execution())
            || (current.is_executable && !item.execution_is_allowed(false)))
            && let Some((session_id, active_item_id)) = active_execution
            && active_item_id == id
        {
            return Err(ItemRepositoryError::ActiveExecutionConflict {
                item_id: id,
                session_id,
            });
        }
        validate_parent(
            &mut transaction,
            self.scope.workspace_id,
            id,
            item.parent_id,
        )
        .await?;
        if previous_parent_id != item.parent_id
            && let Some(parent_id) = item.parent_id
            && let Some((session_id, active_item_id)) = active_execution
            && active_item_id == parent_id
        {
            return Err(ItemRepositoryError::ActiveExecutionConflict {
                item_id: parent_id,
                session_id,
            });
        }
        validate_blocked_by(
            &mut transaction,
            self.scope.workspace_id,
            item.blocked_by_item_id,
        )
        .await?;
        if has_active_children(&mut transaction, self.scope.workspace_id, id).await?
            && item.status.is_executing_state()
        {
            return Err(ItemRepositoryError::NonLeafExecutable);
        }
        update_item(&mut transaction, self.scope.workspace_id, &item).await?;
        persist_item_dependency_edges(&mut transaction, self.scope.workspace_id, &item).await?;
        replace_hierarchy_edge(
            &mut transaction,
            self.scope.workspace_id,
            id,
            item.parent_id,
            item.sibling_order,
        )
        .await?;
        validate_dependency_graph_tx(&mut transaction, self.scope.workspace_id).await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
        let change_group_id = start_item_change_group_tx(&mut transaction).await?;
        record_mutation(
            &mut transaction,
            self.scope,
            &item,
            "item.updated",
            Some(expected_revision),
            ChangeKind::Upsert,
        )
        .await?;
        if previous_parent_id != item.parent_id || previous_sibling_order != item.sibling_order {
            refresh_parents(
                &mut transaction,
                self.scope,
                [previous_parent_id, item.parent_id],
                item.updated_at,
            )
            .await?;
        }
        validate_item_change_group_tx(&mut transaction, self.scope.workspace_id, change_group_id)
            .await?;
        complete_idempotency(&mut transaction, self.scope, &idempotency, &item).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ItemMutation {
            item,
            replayed: false,
        })
    }

    async fn trash(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if let Reservation::Replay(item) =
            reserve_idempotency(&mut transaction, self.scope, &idempotency).await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(ItemMutation {
                item: *item,
                replayed: true,
            });
        }
        let active_execution =
            lock_active_execution(&mut transaction, self.scope.workspace_id).await?;
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        let current =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
        ensure_revision(&current, expected_revision)?;
        if let Some((session_id, active_item_id)) = active_execution
            && active_item_id == id
        {
            return Err(ItemRepositoryError::ActiveExecutionConflict {
                item_id: id,
                session_id,
            });
        }
        if has_active_children(&mut transaction, self.scope.workspace_id, id).await? {
            return Err(ItemRepositoryError::HasChildren);
        }
        let item = current.trashed(now)?;
        update_item(&mut transaction, self.scope.workspace_id, &item).await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, true).await?;
        let change_group_id = start_item_change_group_tx(&mut transaction).await?;
        record_mutation(
            &mut transaction,
            self.scope,
            &item,
            "item.trashed",
            Some(expected_revision),
            ChangeKind::Tombstone,
        )
        .await?;
        refresh_parents(
            &mut transaction,
            self.scope,
            [item.parent_id],
            item.updated_at,
        )
        .await?;
        validate_item_change_group_tx(&mut transaction, self.scope.workspace_id, change_group_id)
            .await?;
        complete_idempotency(&mut transaction, self.scope, &idempotency, &item).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ItemMutation {
            item,
            replayed: false,
        })
    }

    async fn restore(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if let Reservation::Replay(item) =
            reserve_idempotency(&mut transaction, self.scope, &idempotency).await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(ItemMutation {
                item: *item,
                replayed: true,
            });
        }
        let active_execution =
            lock_active_execution(&mut transaction, self.scope.workspace_id).await?;
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        let current =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, true).await?;
        if current.deleted_at.is_none() {
            return Err(ItemRepositoryError::NotFound(id));
        }
        ensure_revision(&current, expected_revision)?;
        validate_parent(
            &mut transaction,
            self.scope.workspace_id,
            id,
            current.parent_id,
        )
        .await
        .map_err(|error| match error {
            ItemRepositoryError::ParentNotFound(_) => ItemRepositoryError::DeletedParent,
            other => other,
        })?;
        if let Some(parent_id) = current.parent_id
            && let Some((session_id, active_item_id)) = active_execution
            && active_item_id == parent_id
        {
            return Err(ItemRepositoryError::ActiveExecutionConflict {
                item_id: parent_id,
                session_id,
            });
        }
        let item = current.restored(now)?;
        if has_active_children(&mut transaction, self.scope.workspace_id, id).await?
            && item.status.is_executing_state()
        {
            return Err(ItemRepositoryError::NonLeafExecutable);
        }
        update_item(&mut transaction, self.scope.workspace_id, &item).await?;
        validate_dependency_graph_tx(&mut transaction, self.scope.workspace_id).await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
        let change_group_id = start_item_change_group_tx(&mut transaction).await?;
        record_mutation(
            &mut transaction,
            self.scope,
            &item,
            "item.restored",
            Some(expected_revision),
            ChangeKind::Upsert,
        )
        .await?;
        refresh_parents(
            &mut transaction,
            self.scope,
            [item.parent_id],
            item.updated_at,
        )
        .await?;
        validate_item_change_group_tx(&mut transaction, self.scope.workspace_id, change_group_id)
            .await?;
        complete_idempotency(&mut transaction, self.scope, &idempotency, &item).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ItemMutation {
            item,
            replayed: false,
        })
    }

    async fn delta_head(&self) -> Result<u64, ItemRepositoryError> {
        let maximum: i64 = sqlx::query_scalar(
            "SELECT COALESCE(max(sequence), 0) FROM item_changes WHERE workspace_id = $1",
        )
        .bind(self.scope.workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        u64::try_from(maximum).map_err(|_| ItemRepositoryError::Internal)
    }

    #[allow(clippy::too_many_lines)] // Keeps cursor validation, atomic expansion, and payload decoding in one ordered read.
    async fn delta(&self, after: u64, limit: usize) -> Result<ItemDeltaPage, ItemRepositoryError> {
        let after = i64::try_from(after).map_err(|_| ItemRepositoryError::Internal)?;
        let maximum =
            i64::try_from(self.delta_head().await?).map_err(|_| ItemRepositoryError::Internal)?;
        if after > maximum {
            return Err(ItemRepositoryError::InvalidCursor);
        }
        let fetch_limit = i64::try_from(
            max_expanded_delta_page_size(limit)?
                .checked_add(1)
                .ok_or(ItemRepositoryError::Internal)?,
        )
        .map_err(|_| ItemRepositoryError::Internal)?;
        let rows = sqlx::query(
            "SELECT sequence, change_kind, payload, change_group_id FROM item_changes \
             WHERE workspace_id = $1 AND sequence > $2 ORDER BY sequence LIMIT $3",
        )
        .bind(self.scope.workspace_id)
        .bind(after)
        .bind(fetch_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        if let Some(first_group_id) = rows
            .first()
            .map(|row| row.try_get::<Option<Uuid>, _>("change_group_id"))
            .transpose()
            .map_err(internal)?
            .flatten()
        {
            let preceding_group_id: Option<Uuid> = sqlx::query_scalar(
                "SELECT change_group_id FROM item_changes \
                 WHERE workspace_id = $1 AND sequence <= $2 \
                 ORDER BY sequence DESC LIMIT 1",
            )
            .bind(self.scope.workspace_id)
            .bind(after)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .flatten();
            if preceding_group_id == Some(first_group_id) {
                return Err(ItemRepositoryError::InvalidCursor);
            }
        }
        let group_ids = rows
            .iter()
            .map(|row| row.try_get::<Option<Uuid>, _>("change_group_id"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(internal)?;
        let payload_sizes = rows
            .iter()
            .map(|row| {
                let payload: Value = row.try_get("payload").map_err(internal)?;
                serde_json::to_vec(&payload)
                    .map(|serialized| serialized.len())
                    .map_err(|_| ItemRepositoryError::Internal)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let selected_count = delivery_bounded_delta_prefix_len(&group_ids, &payload_sizes, limit)?;
        validate_persisted_delta_groups(
            &self.pool,
            self.scope.workspace_id,
            &rows[..selected_count],
        )
        .await?;
        let has_more = rows.len() > selected_count;
        let mut watermark = u64::try_from(after).map_err(|_| ItemRepositoryError::Internal)?;
        let mut changes = Vec::with_capacity(selected_count);
        for row in rows.iter().take(selected_count) {
            let sequence: i64 = row.try_get("sequence").map_err(internal)?;
            watermark = u64::try_from(sequence).map_err(|_| ItemRepositoryError::Internal)?;
            let kind: String = row.try_get("change_kind").map_err(internal)?;
            let payload: Value = row.try_get("payload").map_err(internal)?;
            changes.push(match kind.as_str() {
                "upsert" => DeltaChange::Upsert {
                    item: Box::new(
                        serde_json::from_value(payload)
                            .map_err(|_| ItemRepositoryError::Internal)?,
                    ),
                },
                "tombstone" => DeltaChange::Tombstone {
                    tombstone: serde_json::from_value(payload)
                        .map_err(|_| ItemRepositoryError::Internal)?,
                },
                _ => return Err(ItemRepositoryError::Internal),
            });
        }
        Ok(ItemDeltaPage {
            changes,
            watermark,
            has_more,
        })
    }
}

pub(crate) async fn lock_item_batch_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    lock_workspace_items(transaction, workspace_id).await
}

/// Starts one bounded transaction-local delta group. `record_mutation` reads
/// this local setting, so direct item effects and every implicit parent refresh
/// produced by the same transaction receive the same identifier. The setting
/// disappears automatically at commit or rollback.
pub(crate) async fn start_item_change_group_tx(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<Uuid, ItemRepositoryError> {
    let group_id = Uuid::new_v4();
    let configured: String = sqlx::query_scalar("SELECT set_config($1, $2, true)")
        .bind(ITEM_CHANGE_GROUP_SETTING)
        .bind(group_id.to_string())
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
    if configured == group_id.to_string() {
        Ok(group_id)
    } else {
        Err(ItemRepositoryError::Internal)
    }
}

/// Verifies the write-side bound before a transaction can publish its group,
/// then closes the transaction-local group context.
/// Readers independently enforce the same bound and therefore fail closed if
/// data was inserted outside this repository contract.
pub(crate) async fn validate_item_change_group_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    group_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    validate_item_change_group_with_reserve_tx(transaction, workspace_id, group_id, 0).await
}

/// Applies the ordinary group bounds plus conservative headroom for fields that
/// can be regenerated between preview and a later apply or undo.
pub(crate) async fn validate_preview_item_change_group_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    group_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    validate_item_change_group_with_reserve_tx(
        transaction,
        workspace_id,
        group_id,
        PREVIEW_ITEM_CHANGE_ROW_RESERVE_BYTES,
    )
    .await
}

async fn validate_item_change_group_with_reserve_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    group_id: Uuid,
    reserved_bytes_per_row: i64,
) -> Result<(), ItemRepositoryError> {
    let (count, payload_bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*), COALESCE(sum(octet_length(payload::text)), 0)::bigint \
         FROM item_changes \
         WHERE workspace_id = $1 AND change_group_id = $2",
    )
    .bind(workspace_id)
    .bind(group_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    let bounded_payload_bytes = count
        .checked_mul(reserved_bytes_per_row)
        .and_then(|reserved| payload_bytes.checked_add(reserved));
    let validation = if count <= 0 || payload_bytes < 0 || reserved_bytes_per_row < 0 {
        Err(ItemRepositoryError::Internal)
    } else if count > i64::try_from(MAX_ITEM_CHANGE_GROUP_SIZE).expect("group bound fits i64")
        || bounded_payload_bytes.is_some_and(|bytes| {
            bytes
                > i64::try_from(MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES)
                    .expect("payload bound fits i64")
        })
    {
        Err(ItemRepositoryError::DeltaGroupTooLarge)
    } else if bounded_payload_bytes.is_none() {
        Err(ItemRepositoryError::Internal)
    } else {
        Ok(())
    };
    // Provider import transactions can process another independent item after
    // this check. Resetting prevents any later writer from extending a group
    // whose delivery bounds were already validated. `record_mutation` maps the
    // empty setting to NULL, which the post-cutover database guard rejects.
    let cleared: String = sqlx::query_scalar("SELECT set_config($1, '', true)")
        .bind(ITEM_CHANGE_GROUP_SETTING)
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
    if !cleared.is_empty() {
        return Err(ItemRepositoryError::Internal);
    }
    validation
}

async fn validate_persisted_delta_groups(
    pool: &PgPool,
    workspace_id: Uuid,
    selected_rows: &[PgRow],
) -> Result<(), ItemRepositoryError> {
    let mut local_counts = HashMap::<Uuid, usize>::new();
    for row in selected_rows {
        if let Some(group_id) = row
            .try_get::<Option<Uuid>, _>("change_group_id")
            .map_err(internal)?
        {
            *local_counts.entry(group_id).or_default() += 1;
        }
    }
    if local_counts.is_empty() {
        return Ok(());
    }
    let group_ids = local_counts.keys().copied().collect::<Vec<_>>();
    let stats = sqlx::query(
        "WITH group_stats AS ( \
             SELECT change_group_id, count(*)::bigint AS row_count, \
                    min(sequence) AS minimum_sequence, max(sequence) AS maximum_sequence, \
                    COALESCE(sum(octet_length(payload::text)), 0)::bigint AS payload_bytes \
             FROM item_changes \
             WHERE workspace_id = $1 AND change_group_id = ANY($2) \
             GROUP BY change_group_id \
         ) \
         SELECT stats.change_group_id, stats.row_count, stats.payload_bytes, \
                (SELECT count(*)::bigint FROM item_changes AS spanned \
                 WHERE spanned.workspace_id = $1 \
                   AND spanned.sequence BETWEEN stats.minimum_sequence AND stats.maximum_sequence) \
                    AS span_row_count \
         FROM group_stats AS stats",
    )
    .bind(workspace_id)
    .bind(&group_ids)
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    if stats.len() != group_ids.len() {
        return Err(ItemRepositoryError::Internal);
    }
    for row in stats {
        let group_id: Uuid = row.try_get("change_group_id").map_err(internal)?;
        let row_count: i64 = row.try_get("row_count").map_err(internal)?;
        let payload_bytes: i64 = row.try_get("payload_bytes").map_err(internal)?;
        let span_row_count: i64 = row.try_get("span_row_count").map_err(internal)?;
        let count = usize::try_from(row_count).map_err(|_| ItemRepositoryError::Internal)?;
        if count > MAX_ITEM_CHANGE_GROUP_SIZE
            || payload_bytes
                > i64::try_from(MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES)
                    .expect("payload bound fits i64")
        {
            return Err(ItemRepositoryError::DeltaGroupTooLarge);
        }
        if local_counts.get(&group_id) != Some(&count) || span_row_count != row_count {
            return Err(ItemRepositoryError::Internal);
        }
    }
    Ok(())
}

/// Locks execution state before the canonical item space. Proposal preview,
/// apply, and undo retain this order for every command in their transaction so
/// execution-preventing writes serialize with execution Start without a lock cycle.
pub(crate) async fn lock_execution_item_batch_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    lock_execution_state(transaction, workspace_id).await?;
    lock_workspace_items(transaction, workspace_id).await
}

pub(crate) async fn fetch_item_batch_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
    include_deleted: bool,
) -> Result<Item, ItemRepositoryError> {
    fetch_item_transaction(transaction, workspace_id, item_id, include_deleted).await
}

pub(crate) async fn list_item_batch_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<Vec<Item>, ItemRepositoryError> {
    let mut builder = QueryBuilder::<Postgres>::new(ITEM_SELECT);
    builder
        .push(" WHERE item.workspace_id = ")
        .push_bind(workspace_id)
        .push(" ORDER BY item.id");
    builder
        .build()
        .fetch_all(&mut **transaction)
        .await
        .map_err(internal)?
        .iter()
        .map(item_from_row)
        .collect()
}

/// Constructs the exact neutral identity persisted for a staged create.
/// Projection and SQL staging share this normalization so neither can observe
/// hierarchy, dependency, completion, or blocker state before finalization.
pub(crate) fn staged_item_shell(
    input: &NewItem,
    now: DateTime<Utc>,
) -> Result<Item, ItemRepositoryError> {
    let mut shell = Item::new(input.clone(), now)?;
    shell.status = ItemStatus::Planned;
    shell.completed_at = None;
    shell.parent_id = None;
    shell.blocked_reason_kind = None;
    shell.blocked_by_item_id = None;
    shell.blocked_reason = None;
    shell.project_dependencies(&[])?;
    shell.is_executable = shell.execution_is_allowed(false);
    Ok(shell)
}

/// Inserts a transaction-local shell before proposal batch execution. The row
/// makes its UUID available to foreign keys without publishing a revision,
/// audit record, or delta, and is finalized in the same transaction.
pub(crate) async fn stage_item_create_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    input: &NewItem,
    now: DateTime<Utc>,
) -> Result<(), ItemRepositoryError> {
    let shell = staged_item_shell(input, now)?;
    insert_item(transaction, scope, &shell).await
}

/// Removes the old incoming edge sets for every successor that a transactional
/// batch will replace. The final edge sets can then be inserted in any command
/// order without transient cycles; the caller must validate the complete graph
/// before committing.
pub(crate) async fn clear_dependency_edges_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    successor_item_ids: &[Uuid],
) -> Result<(), ItemRepositoryError> {
    let mut successor_item_ids = successor_item_ids.to_vec();
    successor_item_ids.sort_unstable();
    successor_item_ids.dedup();
    if successor_item_ids.is_empty() {
        return Ok(());
    }
    authorize_dependency_writes(transaction).await?;
    sqlx::query(
        "DELETE FROM item_dependencies WHERE workspace_id = $1 \
         AND successor_item_id = ANY($2)",
    )
    .bind(workspace_id)
    .bind(&successor_item_ids)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

pub(crate) async fn validate_dependency_graph_batch_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    validate_dependency_graph_tx(transaction, workspace_id).await
}

/// Executes one item command inside a transaction that already owns execution
/// state followed by the canonical workspace item lock. `record` is false for
/// rolled-back previews and true for committed application/undo transactions.
#[allow(clippy::too_many_lines)] // Mirrors all four ordinary item mutation invariants in one atomic boundary.
pub(crate) async fn apply_item_command_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    command: TransactionalItemCommand,
    now: DateTime<Utc>,
    record: bool,
    graph_mode: TransactionalGraphMode,
) -> Result<TransactionalItemEffect, ItemRepositoryError> {
    match command {
        TransactionalItemCommand::Create(input) => {
            let item = Item::new(input, now)?;
            validate_parent(transaction, scope.workspace_id, item.id, item.parent_id).await?;
            reject_parent_for_active_execution(transaction, scope.workspace_id, item.parent_id)
                .await?;
            validate_blocked_by(transaction, scope.workspace_id, item.blocked_by_item_id).await?;
            if graph_mode.uses_staged_creates() {
                update_item(transaction, scope.workspace_id, &item).await?;
            } else {
                insert_item(transaction, scope, &item).await?;
            }
            persist_item_dependency_edges(transaction, scope.workspace_id, &item).await?;
            replace_hierarchy_edge(
                transaction,
                scope.workspace_id,
                item.id,
                item.parent_id,
                item.sibling_order,
            )
            .await?;
            if graph_mode.validates_immediately() {
                validate_dependency_graph_tx(transaction, scope.workspace_id).await?;
            }
            let item =
                fetch_item_transaction(transaction, scope.workspace_id, item.id, false).await?;
            if record {
                record_mutation(
                    transaction,
                    scope,
                    &item,
                    "item.created",
                    None,
                    ChangeKind::Upsert,
                )
                .await?;
            }
            refresh_parents_with_mode(
                transaction,
                scope,
                [item.parent_id],
                item.updated_at,
                record,
            )
            .await?;
            Ok(TransactionalItemEffect {
                before: None,
                after: item,
                execution_ordinal: 0,
            })
        }
        TransactionalItemCommand::Replace {
            item_id,
            expected_revision,
            replacement,
        } => {
            let current =
                fetch_item_transaction(transaction, scope.workspace_id, item_id, false).await?;
            ensure_revision(&current, expected_revision)?;
            let previous_parent_id = current.parent_id;
            let previous_sibling_order = current.sibling_order;
            let item = current.replaced(replacement, now)?;
            reject_closing_transition_for_active_execution(
                transaction,
                scope.workspace_id,
                &current,
                &item,
            )
            .await?;
            validate_parent(transaction, scope.workspace_id, item_id, item.parent_id).await?;
            if previous_parent_id != item.parent_id {
                reject_parent_for_active_execution(transaction, scope.workspace_id, item.parent_id)
                    .await?;
            }
            validate_blocked_by(transaction, scope.workspace_id, item.blocked_by_item_id).await?;
            if has_active_children(transaction, scope.workspace_id, item_id).await?
                && item.status.is_executing_state()
            {
                return Err(ItemRepositoryError::NonLeafExecutable);
            }
            update_item(transaction, scope.workspace_id, &item).await?;
            persist_item_dependency_edges(transaction, scope.workspace_id, &item).await?;
            replace_hierarchy_edge(
                transaction,
                scope.workspace_id,
                item_id,
                item.parent_id,
                item.sibling_order,
            )
            .await?;
            if graph_mode.validates_immediately() {
                validate_dependency_graph_tx(transaction, scope.workspace_id).await?;
            }
            let item =
                fetch_item_transaction(transaction, scope.workspace_id, item_id, false).await?;
            if record {
                record_mutation(
                    transaction,
                    scope,
                    &item,
                    "item.updated",
                    Some(expected_revision),
                    ChangeKind::Upsert,
                )
                .await?;
            }
            if previous_parent_id != item.parent_id || previous_sibling_order != item.sibling_order
            {
                refresh_parents_with_mode(
                    transaction,
                    scope,
                    [previous_parent_id, item.parent_id],
                    item.updated_at,
                    record,
                )
                .await?;
            }
            Ok(TransactionalItemEffect {
                before: Some(current),
                after: item,
                execution_ordinal: 0,
            })
        }
        TransactionalItemCommand::Trash {
            item_id,
            expected_revision,
        } => {
            let current =
                fetch_item_transaction(transaction, scope.workspace_id, item_id, false).await?;
            ensure_revision(&current, expected_revision)?;
            if has_active_children(transaction, scope.workspace_id, item_id).await? {
                return Err(ItemRepositoryError::HasChildren);
            }
            let item = current.trashed(now)?;
            reject_closing_transition_for_active_execution(
                transaction,
                scope.workspace_id,
                &current,
                &item,
            )
            .await?;
            update_item(transaction, scope.workspace_id, &item).await?;
            let item =
                fetch_item_transaction(transaction, scope.workspace_id, item_id, true).await?;
            if record {
                record_mutation(
                    transaction,
                    scope,
                    &item,
                    "item.trashed",
                    Some(expected_revision),
                    ChangeKind::Tombstone,
                )
                .await?;
            }
            refresh_parents_with_mode(
                transaction,
                scope,
                [item.parent_id],
                item.updated_at,
                record,
            )
            .await?;
            Ok(TransactionalItemEffect {
                before: Some(current),
                after: item,
                execution_ordinal: 0,
            })
        }
        TransactionalItemCommand::Restore {
            item_id,
            expected_revision,
        } => {
            let current =
                fetch_item_transaction(transaction, scope.workspace_id, item_id, true).await?;
            if current.deleted_at.is_none() {
                return Err(ItemRepositoryError::NotFound(item_id));
            }
            ensure_revision(&current, expected_revision)?;
            validate_parent(transaction, scope.workspace_id, item_id, current.parent_id)
                .await
                .map_err(|error| match error {
                    ItemRepositoryError::ParentNotFound(_) => ItemRepositoryError::DeletedParent,
                    other => other,
                })?;
            reject_parent_for_active_execution(transaction, scope.workspace_id, current.parent_id)
                .await?;
            let item = current.restored(now)?;
            if has_active_children(transaction, scope.workspace_id, item_id).await?
                && item.status.is_executing_state()
            {
                return Err(ItemRepositoryError::NonLeafExecutable);
            }
            update_item(transaction, scope.workspace_id, &item).await?;
            if graph_mode.validates_immediately() {
                validate_dependency_graph_tx(transaction, scope.workspace_id).await?;
            }
            let item =
                fetch_item_transaction(transaction, scope.workspace_id, item_id, false).await?;
            if record {
                record_mutation(
                    transaction,
                    scope,
                    &item,
                    "item.restored",
                    Some(expected_revision),
                    ChangeKind::Upsert,
                )
                .await?;
            }
            refresh_parents_with_mode(
                transaction,
                scope,
                [item.parent_id],
                item.updated_at,
                record,
            )
            .await?;
            Ok(TransactionalItemEffect {
                before: Some(current),
                after: item,
                execution_ordinal: 0,
            })
        }
        TransactionalItemCommand::RestoreSnapshot {
            item_id,
            expected_revision,
            snapshot,
        } => {
            restore_item_snapshot_tx(
                transaction,
                scope,
                item_id,
                expected_revision,
                *snapshot,
                now,
                record,
                graph_mode,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn restore_item_snapshot_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_id: Uuid,
    expected_revision: u64,
    mut snapshot: Item,
    now: DateTime<Utc>,
    record: bool,
    graph_mode: TransactionalGraphMode,
) -> Result<TransactionalItemEffect, ItemRepositoryError> {
    let current = fetch_item_transaction(transaction, scope.workspace_id, item_id, true).await?;
    ensure_revision(&current, expected_revision)?;
    if snapshot.id != item_id || snapshot.created_at != current.created_at {
        return Err(ItemRepositoryError::Internal);
    }
    reject_closing_transition_for_active_execution(
        transaction,
        scope.workspace_id,
        &current,
        &snapshot,
    )
    .await?;
    if snapshot.deleted_at.is_none() {
        validate_parent(transaction, scope.workspace_id, item_id, snapshot.parent_id).await?;
        if current.deleted_at.is_some() || current.parent_id != snapshot.parent_id {
            reject_parent_for_active_execution(transaction, scope.workspace_id, snapshot.parent_id)
                .await?;
        }
        validate_blocked_by(transaction, scope.workspace_id, snapshot.blocked_by_item_id).await?;
        if has_active_children(transaction, scope.workspace_id, item_id).await?
            && snapshot.status.is_executing_state()
        {
            return Err(ItemRepositoryError::NonLeafExecutable);
        }
    }

    snapshot.revision = current
        .revision
        .checked_add(1)
        .ok_or(ItemRepositoryError::Internal)?;
    snapshot.updated_at = now;
    update_item(transaction, scope.workspace_id, &snapshot).await?;
    persist_item_dependency_edges(transaction, scope.workspace_id, &snapshot).await?;
    replace_hierarchy_edge(
        transaction,
        scope.workspace_id,
        item_id,
        snapshot.parent_id,
        snapshot.sibling_order,
    )
    .await?;
    if graph_mode.validates_immediately() {
        validate_dependency_graph_tx(transaction, scope.workspace_id).await?;
    }
    let restored = fetch_item_transaction(transaction, scope.workspace_id, item_id, true).await?;
    if record {
        let (operation, change_kind) =
            match (current.deleted_at.is_some(), restored.deleted_at.is_some()) {
                (_, true) => ("item.trashed", ChangeKind::Tombstone),
                (true, false) => ("item.restored", ChangeKind::Upsert),
                (false, false) => ("item.updated", ChangeKind::Upsert),
            };
        record_mutation(
            transaction,
            scope,
            &restored,
            operation,
            Some(expected_revision),
            change_kind,
        )
        .await?;
    }
    if current.parent_id != restored.parent_id
        || current.sibling_order != restored.sibling_order
        || current.deleted_at.is_some() != restored.deleted_at.is_some()
    {
        refresh_parents_with_mode(
            transaction,
            scope,
            [current.parent_id, restored.parent_id],
            now,
            record,
        )
        .await?;
    }
    Ok(TransactionalItemEffect {
        before: Some(current),
        after: restored,
        execution_ordinal: 0,
    })
}

async fn insert_item(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item: &Item,
) -> Result<(), ItemRepositoryError> {
    let (split_allowed, minimum_chunk, maximum_chunk) = split_columns(&item.split_policy);
    let stored_constraints = item.constraints_without_dependencies()?;
    let result = sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, is_sensitive, kind, status, title, notes, \
         timezone_name, duration_kind, duration_seconds, duration_min_seconds, duration_max_seconds, \
         duration_source, deadline_kind, deadline_date, deadline_at, deadline_strength, \
         deadline_soft_weight, earliest_start_at, recurrence, scheduling_constraints, has_own_effort, \
         split_allowed, minimum_chunk_seconds, maximum_chunk_seconds, importance, urgency, revision, \
         created_at, updated_at, completed_at, trashed_at, tombstoned_at, sibling_order, \
         blocked_reason_kind, blocked_by_item_id, blocked_reason) VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
         $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, \
         $33, $33, $34, $35, $36, $37)",
    )
    .bind(item.id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(item.is_sensitive)
    .bind(kind_name(item.kind))
    .bind(status_name(item.status))
    .bind(&item.title)
    .bind(&item.notes)
    .bind(&item.timezone_name)
    .bind(duration_kind_name(item.duration_kind))
    .bind(
        item.duration_seconds
            .map(|value| i32::try_from(value).expect("validated duration")),
    )
    .bind(
        item.duration_min_seconds
            .map(|value| i32::try_from(value).expect("validated duration minimum")),
    )
    .bind(
        item.duration_max_seconds
            .map(|value| i32::try_from(value).expect("validated duration maximum")),
    )
    .bind(item.duration_source.map(duration_source_name))
    .bind(deadline_kind_name(item.deadline_kind))
    .bind(item.deadline_date)
    .bind(item.deadline_at)
    .bind(item.deadline_strength.map(deadline_strength_name))
    .bind(
        item.deadline_soft_weight
            .map(|value| i32::try_from(value).expect("validated deadline soft weight")),
    )
    .bind(item.earliest_start_at)
    .bind(&item.recurrence)
    .bind(&stored_constraints)
    .bind(item.has_own_effort)
    .bind(split_allowed)
    .bind(minimum_chunk)
    .bind(maximum_chunk)
    .bind(i16::from(item.importance))
    .bind(i16::from(item.urgency))
    .bind(revision_to_i64(item.revision)?)
    .bind(item.created_at)
    .bind(item.updated_at)
    .bind(item.completed_at)
    .bind(item.deleted_at)
    .bind(i32::try_from(item.sibling_order).expect("validated sibling order"))
    .bind(item.blocked_reason_kind.map(blocked_reason_kind_name))
    .bind(item.blocked_by_item_id)
    .bind(&item.blocked_reason)
    .execute(&mut **transaction)
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => Err(ItemRepositoryError::Duplicate(item.id)),
        Err(error) => Err(internal(error)),
    }
}

async fn update_item(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item: &Item,
) -> Result<(), ItemRepositoryError> {
    let (split_allowed, minimum_chunk, maximum_chunk) = split_columns(&item.split_policy);
    let stored_constraints = item.constraints_without_dependencies()?;
    sqlx::query(
        "UPDATE items SET is_sensitive = $3, kind = $4, status = $5, title = $6, notes = $7, timezone_name = $8, \
         duration_kind = $9, duration_seconds = $10, duration_min_seconds = $11, \
         duration_max_seconds = $12, duration_source = $13, deadline_kind = $14, \
         deadline_date = $15, deadline_at = $16, deadline_strength = $17, \
         deadline_soft_weight = $18, earliest_start_at = $19, recurrence = $20, \
         scheduling_constraints = $21, has_own_effort = $22, split_allowed = $23, \
         minimum_chunk_seconds = $24, maximum_chunk_seconds = $25, importance = $26, \
         urgency = $27, revision = $28, updated_at = $29, completed_at = $30, \
         trashed_at = $31, tombstoned_at = $31, sibling_order = $32, blocked_reason_kind = $33, \
         blocked_by_item_id = $34, blocked_reason = $35 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(item.id)
    .bind(item.is_sensitive)
    .bind(kind_name(item.kind))
    .bind(status_name(item.status))
    .bind(&item.title)
    .bind(&item.notes)
    .bind(&item.timezone_name)
    .bind(duration_kind_name(item.duration_kind))
    .bind(
        item.duration_seconds
            .map(|value| i32::try_from(value).expect("validated duration")),
    )
    .bind(
        item.duration_min_seconds
            .map(|value| i32::try_from(value).expect("validated duration minimum")),
    )
    .bind(
        item.duration_max_seconds
            .map(|value| i32::try_from(value).expect("validated duration maximum")),
    )
    .bind(item.duration_source.map(duration_source_name))
    .bind(deadline_kind_name(item.deadline_kind))
    .bind(item.deadline_date)
    .bind(item.deadline_at)
    .bind(item.deadline_strength.map(deadline_strength_name))
    .bind(
        item.deadline_soft_weight
            .map(|value| i32::try_from(value).expect("validated deadline soft weight")),
    )
    .bind(item.earliest_start_at)
    .bind(&item.recurrence)
    .bind(&stored_constraints)
    .bind(item.has_own_effort)
    .bind(split_allowed)
    .bind(minimum_chunk)
    .bind(maximum_chunk)
    .bind(i16::from(item.importance))
    .bind(i16::from(item.urgency))
    .bind(revision_to_i64(item.revision)?)
    .bind(item.updated_at)
    .bind(item.completed_at)
    .bind(item.deleted_at)
    .bind(i32::try_from(item.sibling_order).expect("validated sibling order"))
    .bind(item.blocked_reason_kind.map(blocked_reason_kind_name))
    .bind(item.blocked_by_item_id)
    .bind(&item.blocked_reason)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn fetch_item_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    id: Uuid,
    include_deleted: bool,
) -> Result<Item, ItemRepositoryError> {
    let mut builder = QueryBuilder::<Postgres>::new(ITEM_SELECT);
    builder
        .push(" WHERE item.workspace_id = ")
        .push_bind(workspace_id)
        .push(" AND item.id = ")
        .push_bind(id);
    if !include_deleted {
        builder.push(" AND item.trashed_at IS NULL");
    }
    let row = builder
        .build()
        .fetch_optional(&mut **transaction)
        .await
        .map_err(internal)?
        .ok_or(ItemRepositoryError::NotFound(id))?;
    item_from_row(&row)
}

async fn lock_workspace_items(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    lock_canonical_item_space(transaction, workspace_id)
        .await
        .map_err(internal)?;
    sqlx::query("SELECT id FROM items WHERE workspace_id = $1 ORDER BY id FOR UPDATE")
        .bind(workspace_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(internal)?;
    Ok(())
}

async fn lock_active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, ItemRepositoryError> {
    lock_execution_state(transaction, workspace_id).await?;
    active_execution(transaction, workspace_id).await
}

async fn lock_execution_state(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    // `execution_state` is lazy, so materialize its workspace mutex before locking it. A
    // concurrent execution command performs the same insert-and-lock sequence.
    sqlx::query("INSERT INTO execution_state (workspace_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(workspace_id)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    let _: Uuid = sqlx::query_scalar(
        "SELECT workspace_id FROM execution_state WHERE workspace_id = $1 FOR UPDATE",
    )
    .bind(workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, ItemRepositoryError> {
    let active_session_id: Option<Uuid> =
        sqlx::query_scalar("SELECT active_session_id FROM execution_state WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(internal)?;
    let Some(session_id) = active_session_id else {
        return Ok(None);
    };
    let row = sqlx::query(
        "SELECT item_id, state FROM execution_sessions WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ItemRepositoryError::Internal)?;
    let state: String = row.try_get("state").map_err(internal)?;
    if !matches!(state.as_str(), "active" | "paused") {
        return Err(ItemRepositoryError::Internal);
    }
    Ok(Some((
        session_id,
        row.try_get("item_id").map_err(internal)?,
    )))
}

async fn reject_closing_transition_for_active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    current: &Item,
    replacement: &Item,
) -> Result<(), ItemRepositoryError> {
    let prevents_execution = (!current.status.prevents_execution()
        && replacement.status.prevents_execution())
        || (current.is_executable && !replacement.execution_is_allowed(false));
    let becomes_trashed = current.deleted_at.is_none() && replacement.deleted_at.is_some();
    if !prevents_execution && !becomes_trashed {
        return Ok(());
    }
    if let Some((session_id, active_item_id)) = active_execution(transaction, workspace_id).await?
        && active_item_id == current.id
    {
        return Err(ItemRepositoryError::ActiveExecutionConflict {
            item_id: current.id,
            session_id,
        });
    }
    Ok(())
}

async fn reject_parent_for_active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if let Some((session_id, active_item_id)) = active_execution(transaction, workspace_id).await?
        && active_item_id == parent_id
    {
        return Err(ItemRepositoryError::ActiveExecutionConflict {
            item_id: parent_id,
            session_id,
        });
    }
    Ok(())
}

pub(super) async fn validate_parent(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if parent_id == item_id {
        return Err(ItemRepositoryError::SelfParent);
    }
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM items WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL",
    )
    .bind(workspace_id)
    .bind(parent_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(status) = status else {
        return Err(ItemRepositoryError::ParentNotFound(parent_id));
    };
    if parse_status(&status)?.is_executing_state() {
        return Err(ItemRepositoryError::InvalidParentState);
    }
    let cycle: bool = sqlx::query_scalar(
        "WITH RECURSIVE ancestors(id) AS ( \
             SELECT parent_item_id FROM item_hierarchy \
              WHERE workspace_id = $1 AND child_item_id = $2 \
             UNION \
             SELECT edge.parent_item_id FROM item_hierarchy AS edge \
              JOIN ancestors ON ancestors.id = edge.child_item_id \
              WHERE edge.workspace_id = $1 \
         ) SELECT EXISTS (SELECT 1 FROM ancestors WHERE id = $3)",
    )
    .bind(workspace_id)
    .bind(parent_id)
    .bind(item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if cycle {
        Err(ItemRepositoryError::HierarchyCycle)
    } else {
        Ok(())
    }
}

/// Soft deletion intentionally does not invalidate a blocker identity. This
/// mirrors the composite `PostgreSQL` foreign key and keeps historical causes
/// visible through include-deleted reads.
async fn validate_blocked_by(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    blocked_by_item_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    let Some(blocked_by_item_id) = blocked_by_item_id else {
        return Ok(());
    };
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id = $1 AND id = $2)",
    )
    .bind(workspace_id)
    .bind(blocked_by_item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if exists {
        Ok(())
    } else {
        Err(ItemRepositoryError::BlockedByItemNotFound(
            blocked_by_item_id,
        ))
    }
}

async fn persist_item_dependency_edges(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item: &Item,
) -> Result<(), ItemRepositoryError> {
    let dependencies = item.dependencies()?;
    replace_dependency_edges(transaction, workspace_id, item.id, &dependencies).await
}

async fn replace_dependency_edges(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    successor_item_id: Uuid,
    dependencies: &[Dependency],
) -> Result<(), ItemRepositoryError> {
    authorize_dependency_writes(transaction).await?;
    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by_key(|dependency| dependency.item_id);
    for dependency in &dependencies {
        let predecessor_item_id = dependency.item_id.0;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id = $1 AND id = $2)",
        )
        .bind(workspace_id)
        .bind(predecessor_item_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
        if !exists {
            return Err(ItemRepositoryError::DependencyNotFound(predecessor_item_id));
        }
    }

    sqlx::query("DELETE FROM item_dependencies WHERE workspace_id = $1 AND successor_item_id = $2")
        .bind(workspace_id)
        .bind(successor_item_id)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;

    for (projection_ordinal, dependency) in dependencies.into_iter().enumerate() {
        let (strength, weight) = match dependency.strength {
            ConstraintStrength::Hard => ("hard", None),
            ConstraintStrength::Soft { weight } => (
                "soft",
                Some(i32::try_from(weight).map_err(|_| ItemRepositoryError::Internal)?),
            ),
        };
        let lag_seconds = dependency
            .minimum_lag
            .get()
            .checked_mul(60)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(ItemRepositoryError::Internal)?;
        sqlx::query(
            "INSERT INTO item_dependencies (workspace_id, predecessor_item_id, \
             successor_item_id, dependency_kind, lag_seconds, dependency_strength, \
             dependency_soft_weight, projection_ordinal) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(workspace_id)
        .bind(dependency.item_id.0)
        .bind(successor_item_id)
        .bind(dependency_relation_name(dependency.relation))
        .bind(lag_seconds)
        .bind(strength)
        .bind(weight)
        .bind(i32::try_from(projection_ordinal).map_err(|_| ItemRepositoryError::Internal)?)
        .execute(&mut **transaction)
        .await
        .map_err(map_dependency_write_error)?;
    }
    Ok(())
}

async fn authorize_dependency_writes(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), ItemRepositoryError> {
    let _: String = sqlx::query_scalar(
        "SELECT set_config('dayweave.item_dependency_write', 'aggregate-v1', true)",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn validate_dependency_graph_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ItemRepositoryError> {
    let items = list_item_batch_tx(transaction, workspace_id).await?;
    crate::items::validate_dependency_graph(
        &items.into_iter().map(|item| (item.id, item)).collect(),
    )
}

fn map_dependency_write_error(error: sqlx::Error) -> ItemRepositoryError {
    if error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::constraint)
        == Some("item_dependencies_acyclic")
    {
        ItemRepositoryError::DependencyCycle
    } else {
        internal(error)
    }
}

async fn has_active_children(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<bool, ItemRepositoryError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM item_hierarchy AS edge \
         JOIN items AS child ON child.workspace_id = edge.workspace_id \
             AND child.id = edge.child_item_id \
         WHERE edge.workspace_id = $1 AND edge.parent_item_id = $2 \
             AND child.trashed_at IS NULL)",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)
}

async fn replace_hierarchy_edge(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    child_id: Uuid,
    parent_id: Option<Uuid>,
    sibling_order: u32,
) -> Result<(), ItemRepositoryError> {
    if let Some(parent_id) = parent_id {
        sqlx::query(
            "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id, position) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (workspace_id, child_item_id) DO UPDATE \
             SET parent_item_id = EXCLUDED.parent_item_id, position = EXCLUDED.position, \
                 updated_at = clock_timestamp()",
        )
        .bind(workspace_id)
        .bind(parent_id)
        .bind(child_id)
        .bind(i32::try_from(sibling_order).expect("validated sibling order"))
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    } else {
        sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id = $2")
            .bind(workspace_id)
            .bind(child_id)
            .execute(&mut **transaction)
            .await
            .map_err(internal)?;
    }
    Ok(())
}

pub(super) async fn refresh_parents(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    parent_ids: impl IntoIterator<Item = Option<Uuid>>,
    now: DateTime<Utc>,
) -> Result<(), ItemRepositoryError> {
    refresh_parents_with_mode(transaction, scope, parent_ids, now, true).await
}

async fn refresh_parents_with_mode(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    parent_ids: impl IntoIterator<Item = Option<Uuid>>,
    now: DateTime<Utc>,
    record: bool,
) -> Result<(), ItemRepositoryError> {
    let mut parent_ids: Vec<_> = parent_ids.into_iter().flatten().collect();
    parent_ids.sort_unstable();
    parent_ids.dedup();
    for parent_id in parent_ids {
        let current =
            fetch_item_transaction(transaction, scope.workspace_id, parent_id, false).await?;
        let has_children = has_active_children(transaction, scope.workspace_id, parent_id).await?;
        let parent = current.refreshed_execution(has_children, now)?;
        update_item(transaction, scope.workspace_id, &parent).await?;
        let parent =
            fetch_item_transaction(transaction, scope.workspace_id, parent_id, false).await?;
        if record {
            record_mutation(
                transaction,
                scope,
                &parent,
                "item.hierarchy_changed",
                Some(current.revision),
                ChangeKind::Upsert,
            )
            .await?;
        }
    }
    Ok(())
}

enum Reservation {
    Acquired,
    Replay(Box<Item>),
}

async fn reserve_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    context: &IdempotencyContext,
) -> Result<Reservation, ItemRepositoryError> {
    let key_hash: [u8; 32] = Sha256::digest(context.key.as_bytes()).into();
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
         AND key_hash = $3 AND expires_at <= clock_timestamp()",
    )
    .bind(scope.workspace_id)
    .bind(context.namespace)
    .bind(key_hash.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    let inserted = sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         expires_at) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (workspace_id, namespace, key_hash) DO NOTHING",
    )
    .bind(scope.workspace_id)
    .bind(context.namespace)
    .bind(key_hash.as_slice())
    .bind(context.fingerprint.as_slice())
    .bind(context.expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if inserted == 1 {
        return Ok(Reservation::Acquired);
    }
    let row = sqlx::query(
        "SELECT request_fingerprint, state, response_json FROM idempotency_keys \
         WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(context.namespace)
    .bind(key_hash.as_slice())
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    let fingerprint: Vec<u8> = row.try_get("request_fingerprint").map_err(internal)?;
    if fingerprint != context.fingerprint {
        return Err(ItemRepositoryError::IdempotencyConflict);
    }
    let state: String = row.try_get("state").map_err(internal)?;
    if state != "completed" {
        return Err(ItemRepositoryError::IdempotencyInProgress);
    }
    let response: Value = row
        .try_get::<Option<Value>, _>("response_json")
        .map_err(internal)?
        .ok_or(ItemRepositoryError::Internal)?;
    let item = serde_json::from_value(response).map_err(|_| ItemRepositoryError::Internal)?;
    Ok(Reservation::Replay(Box::new(item)))
}

async fn complete_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    context: &IdempotencyContext,
    item: &Item,
) -> Result<(), ItemRepositoryError> {
    let key_hash: [u8; 32] = Sha256::digest(context.key.as_bytes()).into();
    let response = serde_json::to_value(item).map_err(|_| ItemRepositoryError::Internal)?;
    let updated = sqlx::query(
        "UPDATE idempotency_keys SET state = 'completed', resource_type = 'item', \
         resource_id = $4, response_json = $5, updated_at = clock_timestamp() \
         WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 \
         AND request_fingerprint = $6 AND state = 'in_progress'",
    )
    .bind(scope.workspace_id)
    .bind(context.namespace)
    .bind(key_hash.as_slice())
    .bind(item.id)
    .bind(response)
    .bind(context.fingerprint.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(ItemRepositoryError::Internal)
    }
}

#[derive(Clone, Copy)]
enum ChangeKind {
    Upsert,
    Tombstone,
}

async fn record_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item: &Item,
    operation: &str,
    base_revision: Option<u64>,
    change_kind: ChangeKind,
) -> Result<(), ItemRepositoryError> {
    let payload = match change_kind {
        ChangeKind::Upsert => serde_json::to_value(item),
        ChangeKind::Tombstone => serde_json::to_value(ItemTombstone {
            id: item.id,
            revision: item.revision,
            deleted_at: item.deleted_at.ok_or(ItemRepositoryError::Internal)?,
            parent_id: item.parent_id,
        }),
    }
    .map_err(|_| ItemRepositoryError::Internal)?;
    let change_name = match change_kind {
        ChangeKind::Upsert => "upsert",
        ChangeKind::Tombstone => "tombstone",
    };
    let revision = revision_to_i64(item.revision)?;
    sqlx::query(
        "INSERT INTO item_changes (workspace_id, item_id, item_revision, change_kind, payload, \
         changed_at, change_group_id) VALUES ($1, $2, $3, $4, $5, $6, \
         NULLIF(current_setting($7, true), '')::uuid)",
    )
    .bind(scope.workspace_id)
    .bind(item.id)
    .bind(revision)
    .bind(change_name)
    .bind(&payload)
    .bind(item.updated_at)
    .bind(ITEM_CHANGE_GROUP_SETTING)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, \
         aggregate_revision, event_type, deduplication_key, payload) \
         VALUES ($1, $2, 'item', $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item.id)
    .bind(revision)
    .bind(operation)
    .bind(format!("{operation}:{}:{}", item.id, item.revision))
    .bind(json!({
        "item_id": item.id,
        "revision": item.revision,
        "change": change_name,
    }))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, base_revision, result_revision, outcome) \
         VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, 'succeeded')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(operation)
    .bind(item.id)
    .bind(base_revision.map(revision_to_i64).transpose()?)
    .bind(revision)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

#[allow(clippy::too_many_lines)] // Decodes the complete flat canonical item row without hidden defaults.
fn item_from_row(row: &PgRow) -> Result<Item, ItemRepositoryError> {
    let revision: i64 = row.try_get("revision").map_err(internal)?;
    let duration_kind = parse_duration_kind(
        &row.try_get::<String, _>("duration_kind")
            .map_err(internal)?,
    )?;
    let duration: Option<i32> = row.try_get("duration_seconds").map_err(internal)?;
    let duration_minimum: Option<i32> = row.try_get("duration_min_seconds").map_err(internal)?;
    let duration_maximum: Option<i32> = row.try_get("duration_max_seconds").map_err(internal)?;
    let importance: i16 = row.try_get("importance").map_err(internal)?;
    let urgency: i16 = row.try_get("urgency").map_err(internal)?;
    let split_allowed: bool = row.try_get("split_allowed").map_err(internal)?;
    let split_policy = if split_allowed {
        let minimum: i32 = row.try_get("minimum_chunk_seconds").map_err(internal)?;
        let maximum: i32 = row.try_get("maximum_chunk_seconds").map_err(internal)?;
        SplitPolicy::Splittable {
            minimum_chunk_seconds: u32::try_from(minimum)
                .map_err(|_| ItemRepositoryError::Internal)?,
            maximum_chunk_seconds: u32::try_from(maximum)
                .map_err(|_| ItemRepositoryError::Internal)?,
        }
    } else {
        SplitPolicy::Indivisible
    };
    let has_children: bool = row.try_get("has_children").map_err(internal)?;
    let deleted_at: Option<DateTime<Utc>> = row.try_get("trashed_at").map_err(internal)?;
    let kind = parse_kind(&row.try_get::<String, _>("kind").map_err(internal)?)?;
    let has_own_effort: bool = row.try_get("has_own_effort").map_err(internal)?;
    let authoritative_dependencies: Vec<Dependency> = serde_json::from_value(
        row.try_get::<Value, _>("authoritative_dependencies")
            .map_err(internal)?,
    )
    .map_err(|_| ItemRepositoryError::Internal)?;
    let mut item = Item {
        id: row.try_get("id").map_err(internal)?,
        is_sensitive: row.try_get("is_sensitive").map_err(internal)?,
        kind,
        status: parse_status(&row.try_get::<String, _>("status").map_err(internal)?)?,
        title: row.try_get("title").map_err(internal)?,
        notes: row.try_get("notes").map_err(internal)?,
        timezone_name: row.try_get("timezone_name").map_err(internal)?,
        duration_kind,
        duration_seconds: duration
            .map(u32::try_from)
            .transpose()
            .map_err(|_| ItemRepositoryError::Internal)?,
        duration_min_seconds: duration_minimum
            .map(u32::try_from)
            .transpose()
            .map_err(|_| ItemRepositoryError::Internal)?,
        duration_max_seconds: duration_maximum
            .map(u32::try_from)
            .transpose()
            .map_err(|_| ItemRepositoryError::Internal)?,
        duration_source: row
            .try_get::<Option<String>, _>("duration_source")
            .map_err(internal)?
            .as_deref()
            .map(parse_duration_source)
            .transpose()?,
        deadline_kind: parse_deadline_kind(
            &row.try_get::<String, _>("deadline_kind")
                .map_err(internal)?,
        )?,
        deadline_date: row.try_get("deadline_date").map_err(internal)?,
        deadline_at: row.try_get("deadline_at").map_err(internal)?,
        deadline_strength: row
            .try_get::<Option<String>, _>("deadline_strength")
            .map_err(internal)?
            .as_deref()
            .map(parse_deadline_strength)
            .transpose()?,
        deadline_soft_weight: row
            .try_get::<Option<i32>, _>("deadline_soft_weight")
            .map_err(internal)?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| ItemRepositoryError::Internal)?,
        earliest_start_at: row.try_get("earliest_start_at").map_err(internal)?,
        recurrence: row.try_get("recurrence").map_err(internal)?,
        flexible_constraints: row.try_get("scheduling_constraints").map_err(internal)?,
        has_own_effort,
        split_policy,
        importance: u8::try_from(importance).map_err(|_| ItemRepositoryError::Internal)?,
        urgency: u8::try_from(urgency).map_err(|_| ItemRepositoryError::Internal)?,
        parent_id: row.try_get("parent_item_id").map_err(internal)?,
        sibling_order: u32::try_from(
            row.try_get::<i32, _>("effective_sibling_order")
                .map_err(internal)?,
        )
        .map_err(|_| ItemRepositoryError::Internal)?,
        is_executable: deleted_at.is_none()
            && !has_children
            && kind.has_executable_component(has_own_effort),
        revision: u64::try_from(revision).map_err(|_| ItemRepositoryError::Internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
        completed_at: row.try_get("completed_at").map_err(internal)?,
        deleted_at,
        blocked_reason_kind: row
            .try_get::<Option<String>, _>("blocked_reason_kind")
            .map_err(internal)?
            .as_deref()
            .map(parse_blocked_reason_kind)
            .transpose()?,
        blocked_by_item_id: row.try_get("blocked_by_item_id").map_err(internal)?,
        blocked_reason: row.try_get("blocked_reason").map_err(internal)?,
    };
    item.project_dependencies(&authoritative_dependencies)
        .map_err(|_| ItemRepositoryError::Internal)?;
    Ok(item)
}

fn split_columns(policy: &SplitPolicy) -> (bool, Option<i32>, Option<i32>) {
    match policy {
        SplitPolicy::Indivisible => (false, None, None),
        SplitPolicy::Splittable {
            minimum_chunk_seconds,
            maximum_chunk_seconds,
        } => (
            true,
            Some(i32::try_from(*minimum_chunk_seconds).expect("validated minimum chunk")),
            Some(i32::try_from(*maximum_chunk_seconds).expect("validated maximum chunk")),
        ),
    }
}

const fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Event => "event",
        ItemKind::Task => "task",
        ItemKind::Habit => "habit",
        ItemKind::Routine => "routine",
        ItemKind::Goal => "goal",
        ItemKind::Project => "project",
        ItemKind::Break => "break",
    }
}

const fn dependency_relation_name(relation: DependencyRelation) -> &'static str {
    match relation {
        DependencyRelation::FinishToStart => "finish_to_start",
        DependencyRelation::StartToStart => "start_to_start",
        DependencyRelation::FinishToFinish => "finish_to_finish",
        DependencyRelation::StartToFinish => "start_to_finish",
    }
}

fn parse_kind(value: &str) -> Result<ItemKind, ItemRepositoryError> {
    match value {
        "event" => Ok(ItemKind::Event),
        "task" => Ok(ItemKind::Task),
        "habit" => Ok(ItemKind::Habit),
        "routine" => Ok(ItemKind::Routine),
        "goal" => Ok(ItemKind::Goal),
        "project" => Ok(ItemKind::Project),
        "break" => Ok(ItemKind::Break),
        _ => Err(ItemRepositoryError::Internal),
    }
}

const fn status_name(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Inbox => "inbox",
        ItemStatus::Planned => "planned",
        ItemStatus::Scheduled => "scheduled",
        ItemStatus::InProgress => "in_progress",
        ItemStatus::Paused => "paused",
        ItemStatus::Completed => "completed",
        ItemStatus::Skipped => "skipped",
        ItemStatus::Cancelled => "cancelled",
        ItemStatus::Blocked => "blocked",
    }
}

fn parse_status(value: &str) -> Result<ItemStatus, ItemRepositoryError> {
    match value {
        "inbox" => Ok(ItemStatus::Inbox),
        "planned" => Ok(ItemStatus::Planned),
        "scheduled" => Ok(ItemStatus::Scheduled),
        "in_progress" => Ok(ItemStatus::InProgress),
        "paused" => Ok(ItemStatus::Paused),
        "completed" => Ok(ItemStatus::Completed),
        "skipped" => Ok(ItemStatus::Skipped),
        "cancelled" => Ok(ItemStatus::Cancelled),
        "blocked" => Ok(ItemStatus::Blocked),
        _ => Err(ItemRepositoryError::Internal),
    }
}

const fn duration_kind_name(kind: DurationKind) -> &'static str {
    match kind {
        DurationKind::Unknown => "unknown",
        DurationKind::Exact => "exact",
        DurationKind::Range => "range",
    }
}

fn parse_duration_kind(value: &str) -> Result<DurationKind, ItemRepositoryError> {
    match value {
        "unknown" => Ok(DurationKind::Unknown),
        "exact" => Ok(DurationKind::Exact),
        "range" => Ok(DurationKind::Range),
        _ => Err(ItemRepositoryError::Internal),
    }
}

const fn duration_source_name(source: DurationSource) -> &'static str {
    match source {
        DurationSource::User => "user",
        DurationSource::Assistant => "assistant",
        DurationSource::Learned => "learned",
        DurationSource::Imported => "imported",
    }
}

fn parse_duration_source(value: &str) -> Result<DurationSource, ItemRepositoryError> {
    match value {
        "user" => Ok(DurationSource::User),
        "assistant" => Ok(DurationSource::Assistant),
        "learned" => Ok(DurationSource::Learned),
        "imported" => Ok(DurationSource::Imported),
        _ => Err(ItemRepositoryError::Internal),
    }
}

const fn deadline_kind_name(kind: DeadlineKind) -> &'static str {
    match kind {
        DeadlineKind::None => "none",
        DeadlineKind::Date => "date",
        DeadlineKind::DateTime => "date_time",
    }
}

fn parse_deadline_kind(value: &str) -> Result<DeadlineKind, ItemRepositoryError> {
    match value {
        "none" => Ok(DeadlineKind::None),
        "date" => Ok(DeadlineKind::Date),
        "date_time" => Ok(DeadlineKind::DateTime),
        _ => Err(ItemRepositoryError::Internal),
    }
}

const fn deadline_strength_name(strength: DeadlineStrength) -> &'static str {
    match strength {
        DeadlineStrength::Hard => "hard",
        DeadlineStrength::Soft => "soft",
    }
}

fn parse_deadline_strength(value: &str) -> Result<DeadlineStrength, ItemRepositoryError> {
    match value {
        "hard" => Ok(DeadlineStrength::Hard),
        "soft" => Ok(DeadlineStrength::Soft),
        _ => Err(ItemRepositoryError::Internal),
    }
}

const fn blocked_reason_kind_name(kind: BlockedReasonKind) -> &'static str {
    match kind {
        BlockedReasonKind::Dependency => "dependency",
        BlockedReasonKind::Manual => "manual",
        BlockedReasonKind::External => "external",
    }
}

fn parse_blocked_reason_kind(value: &str) -> Result<BlockedReasonKind, ItemRepositoryError> {
    match value {
        "dependency" => Ok(BlockedReasonKind::Dependency),
        "manual" => Ok(BlockedReasonKind::Manual),
        "external" => Ok(BlockedReasonKind::External),
        _ => Err(ItemRepositoryError::Internal),
    }
}

fn ensure_revision(item: &Item, expected: u64) -> Result<(), ItemRepositoryError> {
    if item.revision == expected {
        Ok(())
    } else {
        Err(ItemRepositoryError::RevisionConflict {
            expected,
            actual: item.revision,
        })
    }
}

fn revision_to_i64(revision: u64) -> Result<i64, ItemRepositoryError> {
    i64::try_from(revision).map_err(|_| ItemRepositoryError::Internal)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

fn internal<T>(_error: T) -> ItemRepositoryError {
    ItemRepositoryError::Internal
}
