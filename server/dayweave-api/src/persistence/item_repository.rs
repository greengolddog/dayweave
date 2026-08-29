use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::items::{
    DeltaChange, IdempotencyContext, Item, ItemDeltaPage, ItemKind, ItemMutation, ItemQuery,
    ItemRepository, ItemRepositoryError, ItemStatus, ItemTombstone, NewItem, ReplaceItem,
    SplitPolicy,
};

use super::{DatabaseScope, database::lock_canonical_item_space};

const ITEM_SELECT: &str = "SELECT item.id, item.is_sensitive, item.kind, item.status, item.title, item.notes, item.timezone_name, \
     item.duration_seconds, item.deadline_at, item.earliest_start_at, item.recurrence, \
     item.scheduling_constraints, item.split_allowed, item.minimum_chunk_seconds, \
     item.maximum_chunk_seconds, item.importance, item.urgency, item.revision, \
     item.created_at, item.updated_at, item.completed_at, item.trashed_at, \
     hierarchy.parent_item_id, \
     CASE WHEN hierarchy.child_item_id IS NULL THEN item.sibling_order ELSE hierarchy.position END \
         AS effective_sibling_order, \
     EXISTS (SELECT 1 FROM item_hierarchy AS child_edge \
         JOIN items AS child ON child.workspace_id = child_edge.workspace_id \
             AND child.id = child_edge.child_item_id \
         WHERE child_edge.workspace_id = item.workspace_id \
             AND child_edge.parent_item_id = item.id AND child.trashed_at IS NULL) AS has_children \
     FROM items AS item LEFT JOIN item_hierarchy AS hierarchy \
       ON hierarchy.workspace_id = item.workspace_id AND hierarchy.child_item_id = item.id";

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
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        validate_parent(
            &mut transaction,
            self.scope.workspace_id,
            item.id,
            item.parent_id,
        )
        .await?;
        insert_item(&mut transaction, self.scope, &item).await?;
        replace_hierarchy_edge(
            &mut transaction,
            self.scope.workspace_id,
            item.id,
            item.parent_id,
            item.sibling_order,
        )
        .await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, item.id, false)
                .await?;
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
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        let current =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
        ensure_revision(&current, expected_revision)?;
        let previous_parent_id = current.parent_id;
        let previous_sibling_order = current.sibling_order;
        let item = current.replaced(replacement, now)?;
        validate_parent(
            &mut transaction,
            self.scope.workspace_id,
            id,
            item.parent_id,
        )
        .await?;
        if has_active_children(&mut transaction, self.scope.workspace_id, id).await?
            && item.status.is_executing_state()
        {
            return Err(ItemRepositoryError::NonLeafExecutable);
        }
        update_item(&mut transaction, self.scope.workspace_id, &item).await?;
        replace_hierarchy_edge(
            &mut transaction,
            self.scope.workspace_id,
            id,
            item.parent_id,
            item.sibling_order,
        )
        .await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
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
        lock_workspace_items(&mut transaction, self.scope.workspace_id).await?;
        let current =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
        ensure_revision(&current, expected_revision)?;
        if has_active_children(&mut transaction, self.scope.workspace_id, id).await? {
            return Err(ItemRepositoryError::HasChildren);
        }
        let item = current.trashed(now)?;
        update_item(&mut transaction, self.scope.workspace_id, &item).await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, true).await?;
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
        let item = current.restored(now)?;
        if has_active_children(&mut transaction, self.scope.workspace_id, id).await?
            && item.status.is_executing_state()
        {
            return Err(ItemRepositoryError::NonLeafExecutable);
        }
        update_item(&mut transaction, self.scope.workspace_id, &item).await?;
        let item =
            fetch_item_transaction(&mut transaction, self.scope.workspace_id, id, false).await?;
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
        complete_idempotency(&mut transaction, self.scope, &idempotency, &item).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ItemMutation {
            item,
            replayed: false,
        })
    }

    async fn delta(&self, after: u64, limit: usize) -> Result<ItemDeltaPage, ItemRepositoryError> {
        let after = i64::try_from(after).map_err(|_| ItemRepositoryError::Internal)?;
        let maximum: i64 = sqlx::query_scalar(
            "SELECT COALESCE(max(sequence), 0) FROM item_changes WHERE workspace_id = $1",
        )
        .bind(self.scope.workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        if after > maximum {
            return Err(ItemRepositoryError::InvalidCursor);
        }
        let fetch_limit = i64::try_from(limit.checked_add(1).ok_or(ItemRepositoryError::Internal)?)
            .map_err(|_| ItemRepositoryError::Internal)?;
        let rows = sqlx::query(
            "SELECT sequence, change_kind, payload FROM item_changes \
             WHERE workspace_id = $1 AND sequence > $2 ORDER BY sequence LIMIT $3",
        )
        .bind(self.scope.workspace_id)
        .bind(after)
        .bind(fetch_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        let has_more = rows.len() > limit;
        let mut watermark = u64::try_from(after).map_err(|_| ItemRepositoryError::Internal)?;
        let mut changes = Vec::with_capacity(rows.len().min(limit));
        for row in rows.iter().take(limit) {
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

/// Executes one item command inside a transaction that already owns the
/// canonical workspace lock. `record` is false for rolled-back previews and
/// true for committed application/undo transactions.
#[allow(clippy::too_many_lines)] // Mirrors all four ordinary item mutation invariants in one atomic boundary.
pub(crate) async fn apply_item_command_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    command: TransactionalItemCommand,
    now: DateTime<Utc>,
    record: bool,
) -> Result<TransactionalItemEffect, ItemRepositoryError> {
    match command {
        TransactionalItemCommand::Create(input) => {
            let item = Item::new(input, now)?;
            validate_parent(transaction, scope.workspace_id, item.id, item.parent_id).await?;
            insert_item(transaction, scope, &item).await?;
            replace_hierarchy_edge(
                transaction,
                scope.workspace_id,
                item.id,
                item.parent_id,
                item.sibling_order,
            )
            .await?;
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
            validate_parent(transaction, scope.workspace_id, item_id, item.parent_id).await?;
            if has_active_children(transaction, scope.workspace_id, item_id).await?
                && item.status.is_executing_state()
            {
                return Err(ItemRepositoryError::NonLeafExecutable);
            }
            update_item(transaction, scope.workspace_id, &item).await?;
            replace_hierarchy_edge(
                transaction,
                scope.workspace_id,
                item_id,
                item.parent_id,
                item.sibling_order,
            )
            .await?;
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
            let item = current.restored(now)?;
            if has_active_children(transaction, scope.workspace_id, item_id).await?
                && item.status.is_executing_state()
            {
                return Err(ItemRepositoryError::NonLeafExecutable);
            }
            update_item(transaction, scope.workspace_id, &item).await?;
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
) -> Result<TransactionalItemEffect, ItemRepositoryError> {
    let current = fetch_item_transaction(transaction, scope.workspace_id, item_id, true).await?;
    ensure_revision(&current, expected_revision)?;
    if snapshot.id != item_id || snapshot.created_at != current.created_at {
        return Err(ItemRepositoryError::Internal);
    }
    if snapshot.deleted_at.is_none() {
        validate_parent(transaction, scope.workspace_id, item_id, snapshot.parent_id).await?;
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
    replace_hierarchy_edge(
        transaction,
        scope.workspace_id,
        item_id,
        snapshot.parent_id,
        snapshot.sibling_order,
    )
    .await?;
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
    })
}

async fn insert_item(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item: &Item,
) -> Result<(), ItemRepositoryError> {
    let (split_allowed, minimum_chunk, maximum_chunk) = split_columns(&item.split_policy);
    let result = sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, is_sensitive, kind, status, title, notes, \
         timezone_name, duration_seconds, deadline_at, earliest_start_at, recurrence, \
         scheduling_constraints, split_allowed, minimum_chunk_seconds, maximum_chunk_seconds, \
         importance, urgency, revision, created_at, updated_at, completed_at, trashed_at, \
         tombstoned_at, sibling_order) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
         $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $24, $25)",
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
    .bind(
        item.duration_seconds
            .map(|value| i32::try_from(value).expect("validated duration")),
    )
    .bind(item.deadline_at)
    .bind(item.earliest_start_at)
    .bind(&item.recurrence)
    .bind(&item.flexible_constraints)
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
    sqlx::query(
        "UPDATE items SET is_sensitive = $3, kind = $4, status = $5, title = $6, notes = $7, timezone_name = $8, \
         duration_seconds = $9, deadline_at = $10, earliest_start_at = $11, recurrence = $12, \
         scheduling_constraints = $13, split_allowed = $14, minimum_chunk_seconds = $15, \
         maximum_chunk_seconds = $16, importance = $17, urgency = $18, revision = $19, \
         updated_at = $20, completed_at = $21, trashed_at = $22, tombstoned_at = $22, \
         sibling_order = $23 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(item.id)
    .bind(item.is_sensitive)
    .bind(kind_name(item.kind))
    .bind(status_name(item.status))
    .bind(&item.title)
    .bind(&item.notes)
    .bind(&item.timezone_name)
    .bind(
        item.duration_seconds
            .map(|value| i32::try_from(value).expect("validated duration")),
    )
    .bind(item.deadline_at)
    .bind(item.earliest_start_at)
    .bind(&item.recurrence)
    .bind(&item.flexible_constraints)
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

async fn validate_parent(
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

async fn refresh_parents(
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
        let is_executable =
            !has_active_children(transaction, scope.workspace_id, parent_id).await?;
        let parent = current.refreshed_execution(is_executable, now)?;
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
         changed_at) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(scope.workspace_id)
    .bind(item.id)
    .bind(revision)
    .bind(change_name)
    .bind(&payload)
    .bind(item.updated_at)
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

fn item_from_row(row: &PgRow) -> Result<Item, ItemRepositoryError> {
    let revision: i64 = row.try_get("revision").map_err(internal)?;
    let duration: Option<i32> = row.try_get("duration_seconds").map_err(internal)?;
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
    Ok(Item {
        id: row.try_get("id").map_err(internal)?,
        is_sensitive: row.try_get("is_sensitive").map_err(internal)?,
        kind: parse_kind(&row.try_get::<String, _>("kind").map_err(internal)?)?,
        status: parse_status(&row.try_get::<String, _>("status").map_err(internal)?)?,
        title: row.try_get("title").map_err(internal)?,
        notes: row.try_get("notes").map_err(internal)?,
        timezone_name: row.try_get("timezone_name").map_err(internal)?,
        duration_seconds: duration
            .map(u32::try_from)
            .transpose()
            .map_err(|_| ItemRepositoryError::Internal)?,
        deadline_at: row.try_get("deadline_at").map_err(internal)?,
        earliest_start_at: row.try_get("earliest_start_at").map_err(internal)?,
        recurrence: row.try_get("recurrence").map_err(internal)?,
        flexible_constraints: row.try_get("scheduling_constraints").map_err(internal)?,
        split_policy,
        importance: u8::try_from(importance).map_err(|_| ItemRepositoryError::Internal)?,
        urgency: u8::try_from(urgency).map_err(|_| ItemRepositoryError::Internal)?,
        parent_id: row.try_get("parent_item_id").map_err(internal)?,
        sibling_order: u32::try_from(
            row.try_get::<i32, _>("effective_sibling_order")
                .map_err(internal)?,
        )
        .map_err(|_| ItemRepositoryError::Internal)?,
        is_executable: deleted_at.is_none() && !has_children,
        revision: u64::try_from(revision).map_err(|_| ItemRepositoryError::Internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
        completed_at: row.try_get("completed_at").map_err(internal)?,
        deleted_at,
    })
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
        ItemKind::Break => "break",
    }
}

fn parse_kind(value: &str) -> Result<ItemKind, ItemRepositoryError> {
    match value {
        "event" => Ok(ItemKind::Event),
        "task" => Ok(ItemKind::Task),
        "habit" => Ok(ItemKind::Habit),
        "routine" => Ok(ItemKind::Routine),
        "goal" => Ok(ItemKind::Goal),
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
