use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::execution::ExecutionRepository;

use super::{Item, ItemDomainError, ReplaceItem};

#[derive(Clone, Debug)]
pub struct IdempotencyContext {
    pub namespace: &'static str,
    pub key: String,
    pub fingerprint: [u8; 32],
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemMutation {
    pub item: Item,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ItemQuery {
    pub parent_id: Option<Uuid>,
    pub include_deleted: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct ItemTombstone {
    pub id: Uuid,
    pub revision: u64,
    pub deleted_at: DateTime<Utc>,
    pub parent_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeltaChange {
    Upsert { item: Box<Item> },
    Tombstone { tombstone: ItemTombstone },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemDeltaPage {
    pub changes: Vec<DeltaChange>,
    pub watermark: u64,
    pub has_more: bool,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ItemRepositoryError {
    #[error("item {0} was not found")]
    NotFound(Uuid),
    #[error("item {0} already exists")]
    Duplicate(Uuid),
    #[error("revision conflict: expected {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("parent item {0} was not found")]
    ParentNotFound(Uuid),
    #[error("blocking item {0} was not found")]
    BlockedByItemNotFound(Uuid),
    #[error("item cannot be its own parent")]
    SelfParent,
    #[error("item hierarchy would contain a cycle")]
    HierarchyCycle,
    #[error("an executing or terminal item cannot become a parent")]
    InvalidParentState,
    #[error("only leaf items can enter an executable state")]
    NonLeafExecutable,
    #[error("item {item_id} is targeted by active execution session {session_id}")]
    ActiveExecutionConflict { item_id: Uuid, session_id: Uuid },
    #[error("an item with active children cannot be deleted")]
    HasChildren,
    #[error("deleted item's parent must be restored first")]
    DeletedParent,
    #[error("idempotency key was used for different request content")]
    IdempotencyConflict,
    #[error("matching idempotency request is still in progress")]
    IdempotencyInProgress,
    #[error("delta cursor does not belong to the available item stream")]
    InvalidCursor,
    #[error(transparent)]
    InvalidItem(#[from] ItemDomainError),
    #[error("repository operation failed")]
    Internal,
}

#[async_trait]
pub trait ItemRepository: Send + Sync {
    fn cursor_scope(&self) -> Uuid;

    async fn create(
        &self,
        item: Item,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError>;

    async fn get(&self, id: Uuid, include_deleted: bool) -> Result<Item, ItemRepositoryError>;

    async fn list(&self, query: ItemQuery) -> Result<Vec<Item>, ItemRepositoryError>;

    async fn replace(
        &self,
        id: Uuid,
        expected_revision: u64,
        replacement: ReplaceItem,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError>;

    async fn trash(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError>;

    async fn restore(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError>;

    /// Returns the authoritative high-water sequence of the durable item
    /// change log without loading any item content.
    async fn delta_head(&self) -> Result<u64, ItemRepositoryError>;

    async fn delta(&self, after: u64, limit: usize) -> Result<ItemDeltaPage, ItemRepositoryError>;
}

#[derive(Clone)]
pub struct InMemoryItemRepository {
    state: Arc<Mutex<MemoryState>>,
    cursor_scope: Uuid,
    execution_guard: Option<MemoryExecutionGuard>,
}

impl std::fmt::Debug for InMemoryItemRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InMemoryItemRepository")
            .field("cursor_scope", &self.cursor_scope)
            .field("execution_guard", &self.execution_guard.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for InMemoryItemRepository {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState::default())),
            cursor_scope: Uuid::new_v4(),
            execution_guard: None,
        }
    }
}

impl InMemoryItemRepository {
    pub(crate) fn with_execution_guard(
        execution: Arc<dyn ExecutionRepository>,
        operation_gate: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            execution_guard: Some(MemoryExecutionGuard {
                execution,
                operation_gate,
            }),
            ..Self::default()
        }
    }
}

#[derive(Clone)]
struct MemoryExecutionGuard {
    execution: Arc<dyn ExecutionRepository>,
    operation_gate: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryState {
    items: HashMap<Uuid, Item>,
    idempotency: HashMap<(String, String), MemoryIdempotency>,
    changes: Vec<MemoryChange>,
    next_sequence: u64,
}

#[derive(Clone, Debug)]
struct MemoryIdempotency {
    fingerprint: [u8; 32],
    expires_at: DateTime<Utc>,
    item: Item,
}

#[derive(Clone, Debug)]
struct MemoryChange {
    sequence: u64,
    change: DeltaChange,
}

#[async_trait]
impl ItemRepository for InMemoryItemRepository {
    fn cursor_scope(&self) -> Uuid {
        self.cursor_scope
    }

    async fn create(
        &self,
        item: Item,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        if let Some(parent_id) = item.parent_id
            && let Some(execution_guard) = &self.execution_guard
        {
            let _operation = execution_guard.operation_gate.lock().await;
            {
                let mut state = self.state.lock().await;
                if let Some(replay) = replay(&mut state, &idempotency) {
                    return replay;
                }
            }
            let snapshot = execution_guard
                .execution
                .snapshot()
                .await
                .map_err(|_| ItemRepositoryError::Internal)?;
            if let Some(session) = snapshot
                .active_session
                .filter(|session| session.item_id == parent_id)
            {
                return Err(ItemRepositoryError::ActiveExecutionConflict {
                    item_id: parent_id,
                    session_id: session.id,
                });
            }
            let mut state = self.state.lock().await;
            return create_memory(&mut state, item, idempotency);
        }

        let mut state = self.state.lock().await;
        create_memory(&mut state, item, idempotency)
    }

    async fn get(&self, id: Uuid, include_deleted: bool) -> Result<Item, ItemRepositoryError> {
        let state = self.state.lock().await;
        let item = state
            .items
            .get(&id)
            .filter(|item| include_deleted || item.deleted_at.is_none())
            .ok_or(ItemRepositoryError::NotFound(id))?;
        Ok(decorate(item.clone(), &state.items))
    }

    async fn list(&self, query: ItemQuery) -> Result<Vec<Item>, ItemRepositoryError> {
        let state = self.state.lock().await;
        let mut items: Vec<_> = state
            .items
            .values()
            .filter(|item| query.include_deleted || item.deleted_at.is_none())
            .filter(|item| {
                query
                    .parent_id
                    .is_none_or(|parent| item.parent_id == Some(parent))
            })
            .map(|item| decorate(item.clone(), &state.items))
            .collect();
        items.sort_by(|left, right| {
            left.parent_id
                .cmp(&right.parent_id)
                .then_with(|| left.sibling_order.cmp(&right.sibling_order))
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        items.truncate(query.limit);
        Ok(items)
    }

    async fn replace(
        &self,
        id: Uuid,
        expected_revision: u64,
        replacement: ReplaceItem,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        let may_close_self = replacement.status.prevents_execution()
            || replacement.may_remove_executable_component();
        if (may_close_self || replacement.parent_id.is_some())
            && let Some(execution_guard) = &self.execution_guard
        {
            let _operation = execution_guard.operation_gate.lock().await;
            let previous_parent_id = {
                let mut state = self.state.lock().await;
                if let Some(replay) = replay(&mut state, &idempotency) {
                    return replay;
                }
                let current = state
                    .items
                    .get(&id)
                    .filter(|item| item.deleted_at.is_none())
                    .ok_or(ItemRepositoryError::NotFound(id))?;
                ensure_revision(current, expected_revision)?;
                current.parent_id
            };

            let snapshot = execution_guard
                .execution
                .snapshot()
                .await
                .map_err(|_| ItemRepositoryError::Internal)?;
            if let Some(session) = snapshot.active_session {
                let conflicted_item = if may_close_self && session.item_id == id {
                    Some(id)
                } else if replacement.parent_id != previous_parent_id
                    && replacement.parent_id == Some(session.item_id)
                {
                    Some(session.item_id)
                } else {
                    None
                };
                if let Some(item_id) = conflicted_item {
                    return Err(ItemRepositoryError::ActiveExecutionConflict {
                        item_id,
                        session_id: session.id,
                    });
                }
            }

            let mut state = self.state.lock().await;
            return replace_memory(
                &mut state,
                id,
                expected_revision,
                replacement,
                now,
                idempotency,
            );
        }

        let mut guard = self.state.lock().await;
        replace_memory(
            &mut guard,
            id,
            expected_revision,
            replacement,
            now,
            idempotency,
        )
    }

    async fn trash(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        if let Some(execution_guard) = &self.execution_guard {
            let _operation = execution_guard.operation_gate.lock().await;
            {
                let mut state = self.state.lock().await;
                if let Some(replay) = replay(&mut state, &idempotency) {
                    return replay;
                }
                let current = state
                    .items
                    .get(&id)
                    .filter(|item| item.deleted_at.is_none())
                    .ok_or(ItemRepositoryError::NotFound(id))?;
                ensure_revision(current, expected_revision)?;
            }

            let snapshot = execution_guard
                .execution
                .snapshot()
                .await
                .map_err(|_| ItemRepositoryError::Internal)?;
            if let Some(session) = snapshot
                .active_session
                .filter(|session| session.item_id == id)
            {
                return Err(ItemRepositoryError::ActiveExecutionConflict {
                    item_id: id,
                    session_id: session.id,
                });
            }

            let mut state = self.state.lock().await;
            return trash_memory(&mut state, id, expected_revision, now, idempotency);
        }

        let mut guard = self.state.lock().await;
        trash_memory(&mut guard, id, expected_revision, now, idempotency)
    }

    async fn restore(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        if let Some(execution_guard) = &self.execution_guard {
            let _operation = execution_guard.operation_gate.lock().await;
            let parent_id = {
                let mut state = self.state.lock().await;
                if let Some(replay) = replay(&mut state, &idempotency) {
                    return replay;
                }
                let current = state
                    .items
                    .get(&id)
                    .filter(|item| item.deleted_at.is_some())
                    .ok_or(ItemRepositoryError::NotFound(id))?;
                ensure_revision(current, expected_revision)?;
                current.parent_id
            };
            if let Some(parent_id) = parent_id {
                let snapshot = execution_guard
                    .execution
                    .snapshot()
                    .await
                    .map_err(|_| ItemRepositoryError::Internal)?;
                if let Some(session) = snapshot
                    .active_session
                    .filter(|session| session.item_id == parent_id)
                {
                    return Err(ItemRepositoryError::ActiveExecutionConflict {
                        item_id: parent_id,
                        session_id: session.id,
                    });
                }
            }
            let mut state = self.state.lock().await;
            return restore_memory(&mut state, id, expected_revision, now, idempotency);
        }

        let mut state = self.state.lock().await;
        restore_memory(&mut state, id, expected_revision, now, idempotency)
    }

    async fn delta(&self, after: u64, limit: usize) -> Result<ItemDeltaPage, ItemRepositoryError> {
        let state = self.state.lock().await;
        if after > state.next_sequence {
            return Err(ItemRepositoryError::InvalidCursor);
        }
        let fetch_limit = limit.checked_add(1).ok_or(ItemRepositoryError::Internal)?;
        let mut matching = state
            .changes
            .iter()
            .filter(|change| change.sequence > after);
        let selected: Vec<_> = matching.by_ref().take(fetch_limit).cloned().collect();
        let has_more = selected.len() > limit;
        let changes: Vec<_> = selected
            .into_iter()
            .take(limit)
            .map(|change| change.change)
            .collect();
        let watermark = state
            .changes
            .iter()
            .filter(|change| change.sequence > after)
            .take(limit)
            .last()
            .map_or(after, |change| change.sequence);
        Ok(ItemDeltaPage {
            changes,
            watermark,
            has_more,
        })
    }

    async fn delta_head(&self) -> Result<u64, ItemRepositoryError> {
        Ok(self.state.lock().await.next_sequence)
    }
}

fn create_memory(
    state: &mut MemoryState,
    mut item: Item,
    idempotency: IdempotencyContext,
) -> Result<ItemMutation, ItemRepositoryError> {
    if let Some(replay) = replay(state, &idempotency) {
        return replay;
    }
    let mut next = state.clone();
    if next.items.contains_key(&item.id) {
        return Err(ItemRepositoryError::Duplicate(item.id));
    }
    validate_parent(&next.items, item.id, item.parent_id)?;
    validate_blocked_by(&next.items, item.blocked_by_item_id)?;
    // The repository owns the topology-aware projection even when an internal
    // adapter supplies a stale pre-structural value.
    item.is_executable = item.execution_is_allowed(false);
    next.items.insert(item.id, item.clone());
    append_change(
        &mut next,
        DeltaChange::Upsert {
            item: Box::new(item.clone()),
        },
    )?;
    refresh_parents(&mut next, [item.parent_id], item.updated_at)?;
    remember(&mut next, idempotency, item.clone());
    *state = next;
    Ok(ItemMutation {
        item,
        replayed: false,
    })
}

fn replace_memory(
    state: &mut MemoryState,
    id: Uuid,
    expected_revision: u64,
    replacement: ReplaceItem,
    now: DateTime<Utc>,
    idempotency: IdempotencyContext,
) -> Result<ItemMutation, ItemRepositoryError> {
    if let Some(replay) = replay(state, &idempotency) {
        return replay;
    }
    let mut next = state.clone();
    let current = next
        .items
        .get(&id)
        .filter(|item| item.deleted_at.is_none())
        .cloned()
        .ok_or(ItemRepositoryError::NotFound(id))?;
    ensure_revision(&current, expected_revision)?;
    let previous_parent_id = current.parent_id;
    let previous_sibling_order = current.sibling_order;
    let mut item = current.replaced(replacement, now)?;
    validate_parent(&next.items, id, item.parent_id)?;
    validate_blocked_by(&next.items, item.blocked_by_item_id)?;
    let has_children = has_active_children(id, &next.items);
    if has_children && item.status.is_executing_state() {
        return Err(ItemRepositoryError::NonLeafExecutable);
    }
    item.is_executable = item.execution_is_allowed(has_children);
    next.items.insert(id, item.clone());
    append_change(
        &mut next,
        DeltaChange::Upsert {
            item: Box::new(item.clone()),
        },
    )?;
    if previous_parent_id != item.parent_id || previous_sibling_order != item.sibling_order {
        refresh_parents(
            &mut next,
            [previous_parent_id, item.parent_id],
            item.updated_at,
        )?;
    }
    remember(&mut next, idempotency, item.clone());
    *state = next;
    Ok(ItemMutation {
        item,
        replayed: false,
    })
}

fn restore_memory(
    state: &mut MemoryState,
    id: Uuid,
    expected_revision: u64,
    now: DateTime<Utc>,
    idempotency: IdempotencyContext,
) -> Result<ItemMutation, ItemRepositoryError> {
    if let Some(replay) = replay(state, &idempotency) {
        return replay;
    }
    let mut next = state.clone();
    let current = next
        .items
        .get(&id)
        .filter(|item| item.deleted_at.is_some())
        .cloned()
        .ok_or(ItemRepositoryError::NotFound(id))?;
    ensure_revision(&current, expected_revision)?;
    if current.parent_id.is_some_and(|parent_id| {
        next.items
            .get(&parent_id)
            .is_none_or(|parent| parent.deleted_at.is_some())
    }) {
        return Err(ItemRepositoryError::DeletedParent);
    }
    let mut item = current.restored(now)?;
    let has_children = has_active_children(id, &next.items);
    item.is_executable = item.execution_is_allowed(has_children);
    if has_children && item.status.is_executing_state() {
        return Err(ItemRepositoryError::NonLeafExecutable);
    }
    next.items.insert(id, item.clone());
    append_change(
        &mut next,
        DeltaChange::Upsert {
            item: Box::new(item.clone()),
        },
    )?;
    refresh_parents(&mut next, [item.parent_id], item.updated_at)?;
    remember(&mut next, idempotency, item.clone());
    *state = next;
    Ok(ItemMutation {
        item,
        replayed: false,
    })
}

fn trash_memory(
    state: &mut MemoryState,
    id: Uuid,
    expected_revision: u64,
    now: DateTime<Utc>,
    idempotency: IdempotencyContext,
) -> Result<ItemMutation, ItemRepositoryError> {
    if let Some(replay) = replay(state, &idempotency) {
        return replay;
    }
    let mut next = state.clone();
    let current = next
        .items
        .get(&id)
        .filter(|item| item.deleted_at.is_none())
        .cloned()
        .ok_or(ItemRepositoryError::NotFound(id))?;
    ensure_revision(&current, expected_revision)?;
    if has_active_children(id, &next.items) {
        return Err(ItemRepositoryError::HasChildren);
    }
    let item = current.trashed(now)?;
    let deleted_at = item.deleted_at.ok_or(ItemRepositoryError::Internal)?;
    next.items.insert(id, item.clone());
    append_change(
        &mut next,
        DeltaChange::Tombstone {
            tombstone: ItemTombstone {
                id,
                revision: item.revision,
                deleted_at,
                parent_id: item.parent_id,
            },
        },
    )?;
    refresh_parents(&mut next, [item.parent_id], item.updated_at)?;
    remember(&mut next, idempotency, item.clone());
    *state = next;
    Ok(ItemMutation {
        item,
        replayed: false,
    })
}

fn validate_parent(
    items: &HashMap<Uuid, Item>,
    item_id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if parent_id == item_id {
        return Err(ItemRepositoryError::SelfParent);
    }
    let parent = items
        .get(&parent_id)
        .filter(|item| item.deleted_at.is_none())
        .ok_or(ItemRepositoryError::ParentNotFound(parent_id))?;
    if parent.status.is_executing_state() {
        return Err(ItemRepositoryError::InvalidParentState);
    }
    let mut visited = HashSet::new();
    let mut ancestor = Some(parent_id);
    while let Some(ancestor_id) = ancestor {
        if ancestor_id == item_id {
            return Err(ItemRepositoryError::HierarchyCycle);
        }
        if !visited.insert(ancestor_id) {
            return Err(ItemRepositoryError::HierarchyCycle);
        }
        ancestor = items.get(&ancestor_id).and_then(|item| item.parent_id);
    }
    Ok(())
}

/// A soft-deleted blocker remains a valid historical identity and can still
/// explain why another item was blocked. Repositories never physically remove
/// canonical rows, and `PostgreSQL`'s matching foreign key follows this rule.
fn validate_blocked_by(
    items: &HashMap<Uuid, Item>,
    blocked_by_item_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    if let Some(blocked_by_item_id) = blocked_by_item_id
        && !items.contains_key(&blocked_by_item_id)
    {
        return Err(ItemRepositoryError::BlockedByItemNotFound(
            blocked_by_item_id,
        ));
    }
    Ok(())
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

fn has_active_children(item_id: Uuid, items: &HashMap<Uuid, Item>) -> bool {
    items
        .values()
        .any(|item| item.parent_id == Some(item_id) && item.deleted_at.is_none())
}

fn decorate(mut item: Item, items: &HashMap<Uuid, Item>) -> Item {
    item.is_executable = item.execution_is_allowed(has_active_children(item.id, items));
    item
}

fn refresh_parents(
    state: &mut MemoryState,
    parent_ids: impl IntoIterator<Item = Option<Uuid>>,
    now: DateTime<Utc>,
) -> Result<(), ItemRepositoryError> {
    let mut parent_ids: Vec<_> = parent_ids.into_iter().flatten().collect();
    parent_ids.sort_unstable();
    parent_ids.dedup();
    for parent_id in parent_ids {
        let parent = state
            .items
            .get(&parent_id)
            .filter(|item| item.deleted_at.is_none())
            .cloned()
            .ok_or(ItemRepositoryError::ParentNotFound(parent_id))?;
        let parent =
            parent.refreshed_execution(has_active_children(parent_id, &state.items), now)?;
        state.items.insert(parent_id, parent.clone());
        append_change(
            state,
            DeltaChange::Upsert {
                item: Box::new(parent),
            },
        )?;
    }
    Ok(())
}

fn replay(
    state: &mut MemoryState,
    context: &IdempotencyContext,
) -> Option<Result<ItemMutation, ItemRepositoryError>> {
    let key = (context.namespace.to_owned(), context.key.clone());
    if state
        .idempotency
        .get(&key)
        .is_some_and(|entry| entry.expires_at <= Utc::now())
    {
        state.idempotency.remove(&key);
        return None;
    }
    state.idempotency.get(&key).map(|entry| {
        if entry.fingerprint == context.fingerprint {
            Ok(ItemMutation {
                item: entry.item.clone(),
                replayed: true,
            })
        } else {
            Err(ItemRepositoryError::IdempotencyConflict)
        }
    })
}

fn remember(state: &mut MemoryState, context: IdempotencyContext, item: Item) {
    state.idempotency.insert(
        (context.namespace.to_owned(), context.key),
        MemoryIdempotency {
            fingerprint: context.fingerprint,
            expires_at: context.expires_at,
            item,
        },
    );
}

fn append_change(state: &mut MemoryState, change: DeltaChange) -> Result<(), ItemRepositoryError> {
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .ok_or(ItemRepositoryError::Internal)?;
    state.changes.push(MemoryChange {
        sequence: state.next_sequence,
        change,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use super::*;
    use crate::items::{BlockedReasonKind, ItemKind, ItemStatus, NewItem, SplitPolicy};

    fn new_item(id: Uuid, title: &str, parent_id: Option<Uuid>, order: u32) -> Item {
        Item::new(
            NewItem {
                id,
                is_sensitive: false,
                kind: ItemKind::Task,
                status: ItemStatus::Planned,
                title: title.to_owned(),
                notes: None,
                timezone_name: "Europe/Madrid".to_owned(),
                duration_kind: None,
                duration_seconds: Some(1800),
                duration_min_seconds: None,
                duration_max_seconds: None,
                duration_source: None,
                deadline_kind: None,
                deadline_date: None,
                deadline_at: None,
                deadline_strength: None,
                deadline_soft_weight: None,
                earliest_start_at: None,
                recurrence: None,
                flexible_constraints: json!({}),
                has_own_effort: None,
                split_policy: SplitPolicy::Indivisible,
                importance: 50,
                urgency: 40,
                parent_id,
                sibling_order: order,
                blocked_reason_kind: None,
                blocked_by_item_id: None,
                blocked_reason: None,
            },
            Utc::now(),
        )
        .unwrap()
    }

    fn idempotency(namespace: &'static str, key: &str, marker: u8) -> IdempotencyContext {
        IdempotencyContext {
            namespace,
            key: key.to_owned(),
            fingerprint: [marker; 32],
            expires_at: Utc::now() + Duration::hours(1),
        }
    }

    fn replacement(item: &Item, parent_id: Option<Uuid>, status: ItemStatus) -> ReplaceItem {
        ReplaceItem {
            is_sensitive: item.is_sensitive,
            kind: item.kind,
            status,
            title: item.title.clone(),
            notes: item.notes.clone(),
            timezone_name: item.timezone_name.clone(),
            duration_kind: Some(item.duration_kind),
            duration_seconds: item.duration_seconds,
            duration_min_seconds: item.duration_min_seconds,
            duration_max_seconds: item.duration_max_seconds,
            duration_source: item.duration_source,
            deadline_kind: Some(item.deadline_kind),
            deadline_date: item.deadline_date,
            deadline_at: item.deadline_at,
            deadline_strength: item.deadline_strength,
            deadline_soft_weight: item.deadline_soft_weight,
            earliest_start_at: item.earliest_start_at,
            recurrence: item.recurrence.clone(),
            flexible_constraints: item.flexible_constraints.clone(),
            has_own_effort: Some(item.has_own_effort),
            split_policy: item.split_policy.clone(),
            importance: item.importance,
            urgency: item.urgency,
            parent_id,
            sibling_order: item.sibling_order,
            blocked_reason_kind: item.blocked_reason_kind,
            blocked_by_item_id: item.blocked_by_item_id,
            blocked_reason: item.blocked_reason.clone(),
        }
    }

    #[tokio::test]
    async fn hierarchy_is_unbounded_ordered_and_cycle_safe() {
        let repository = InMemoryItemRepository::default();
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        let leaf_id = Uuid::new_v4();
        let root = new_item(root_id, "Root", None, 0);
        repository
            .create(root.clone(), idempotency("items.create", "root-key", 1))
            .await
            .unwrap();
        let child = new_item(child_id, "Child", Some(root_id), 2);
        repository
            .create(child, idempotency("items.create", "child-key", 2))
            .await
            .unwrap();
        let leaf = new_item(leaf_id, "Leaf", Some(child_id), 1);
        repository
            .create(leaf, idempotency("items.create", "leaf-key", 3))
            .await
            .unwrap();

        assert!(!repository.get(root_id, false).await.unwrap().is_executable);
        let error = repository
            .replace(
                root_id,
                2,
                replacement(&root, Some(leaf_id), ItemStatus::Planned),
                Utc::now(),
                idempotency("items.replace", "cycle-key", 4),
            )
            .await
            .unwrap_err();
        assert_eq!(error, ItemRepositoryError::HierarchyCycle);

        let siblings = repository
            .list(ItemQuery {
                parent_id: Some(root_id),
                include_deleted: false,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(siblings[0].id, child_id);

        let delta = repository.delta(0, 10).await.unwrap();
        assert_eq!(delta.changes.len(), 5);
        assert!(delta.changes.iter().any(|change| {
            matches!(
                change,
                DeltaChange::Upsert { item }
                    if item.id == root_id && item.revision == 2 && !item.is_executable
            )
        }));
        assert!(delta.changes.iter().any(|change| {
            matches!(
                change,
                DeltaChange::Upsert { item }
                    if item.id == child_id && item.revision == 2 && !item.is_executable
            )
        }));
    }

    #[tokio::test]
    async fn semantic_container_projection_requires_explicit_own_effort() {
        let repository = InMemoryItemRepository::default();
        for (marker, kind) in [
            (11, ItemKind::Project),
            (12, ItemKind::Goal),
            (13, ItemKind::Routine),
        ] {
            // Simulate a direct adapter caller carrying the pre-structural
            // hierarchy-only projection. The repository remains authoritative.
            let mut item = new_item(
                Uuid::from_u128(u128::from(marker)),
                "Semantic container",
                None,
                0,
            );
            item.kind = kind;
            item.has_own_effort = false;
            item.is_executable = true;
            let created = repository
                .create(
                    item,
                    idempotency("items.create", &format!("container-{marker}"), marker),
                )
                .await
                .expect("create semantic container");
            assert!(!created.item.is_executable, "{kind:?} is not executable");
            assert!(
                !repository
                    .get(created.item.id, false)
                    .await
                    .expect("read semantic container")
                    .is_executable
            );
        }
    }

    #[tokio::test]
    async fn non_leaf_cannot_execute_and_revisions_are_optimistic() {
        let repository = InMemoryItemRepository::default();
        let root_id = Uuid::new_v4();
        let root = new_item(root_id, "Root", None, 0);
        repository
            .create(root.clone(), idempotency("items.create", "root-key", 1))
            .await
            .unwrap();
        repository
            .create(
                new_item(Uuid::new_v4(), "Child", Some(root_id), 0),
                idempotency("items.create", "child-key", 2),
            )
            .await
            .unwrap();

        let error = repository
            .replace(
                root_id,
                2,
                replacement(&root, None, ItemStatus::InProgress),
                Utc::now(),
                idempotency("items.replace", "execute-key", 3),
            )
            .await
            .unwrap_err();
        assert_eq!(error, ItemRepositoryError::NonLeafExecutable);
    }

    #[tokio::test]
    async fn idempotency_replays_and_delta_retains_tombstones() {
        let repository = InMemoryItemRepository::default();
        let id = Uuid::new_v4();
        let item = new_item(id, "Offline item", None, 0);
        let context = idempotency("items.create", "offline-key", 1);
        let first = repository
            .create(item.clone(), context.clone())
            .await
            .unwrap();
        let replay = repository.create(item, context).await.unwrap();
        assert!(!first.replayed);
        assert!(replay.replayed);

        let deleted = repository
            .trash(
                id,
                1,
                Utc::now(),
                idempotency("items.delete", "delete-key", 2),
            )
            .await
            .unwrap();
        assert_eq!(deleted.item.revision, 2);
        let delta = repository.delta(0, 10).await.unwrap();
        assert_eq!(delta.changes.len(), 2);
        assert!(matches!(delta.changes[1], DeltaChange::Tombstone { .. }));

        let restored = repository
            .restore(
                id,
                2,
                Utc::now(),
                idempotency("items.restore", "restore-key", 3),
            )
            .await
            .unwrap();
        assert_eq!(restored.item.revision, 3);
        let tail = repository.delta(delta.watermark, 10).await.unwrap();
        assert_eq!(tail.changes.len(), 1);
        assert!(matches!(tail.changes[0], DeltaChange::Upsert { .. }));
    }

    #[tokio::test]
    async fn dependency_blockers_require_an_existing_identity_and_retain_trashed_history() {
        let repository = InMemoryItemRepository::default();
        let missing_id = Uuid::new_v4();
        let mut missing_blocker = new_item(Uuid::new_v4(), "Blocked", None, 0);
        missing_blocker.status = ItemStatus::Blocked;
        missing_blocker.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
        missing_blocker.blocked_by_item_id = Some(missing_id);
        let error = repository
            .create(
                missing_blocker,
                idempotency("items.create", "missing-blocker", 1),
            )
            .await
            .expect_err("missing blocker identity must fail");
        assert_eq!(
            error,
            ItemRepositoryError::BlockedByItemNotFound(missing_id)
        );

        let replace_id = Uuid::new_v4();
        let replace_current = new_item(replace_id, "Replace target", None, 0);
        repository
            .create(
                replace_current.clone(),
                idempotency("items.create", "replace-target", 2),
            )
            .await
            .unwrap();
        let mut replace = replacement(&replace_current, None, ItemStatus::Blocked);
        replace.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
        replace.blocked_by_item_id = Some(missing_id);
        let error = repository
            .replace(
                replace_id,
                1,
                replace,
                Utc::now(),
                idempotency("items.replace", "missing-replacement-blocker", 3),
            )
            .await
            .expect_err("replacement must validate blocker identity");
        assert_eq!(
            error,
            ItemRepositoryError::BlockedByItemNotFound(missing_id)
        );

        let blocker_id = Uuid::new_v4();
        let prerequisite = new_item(blocker_id, "Prerequisite", None, 0);
        repository
            .create(prerequisite, idempotency("items.create", "blocker-item", 4))
            .await
            .unwrap();
        let mut dependent = new_item(Uuid::new_v4(), "Dependent", None, 0);
        dependent.status = ItemStatus::Blocked;
        dependent.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
        dependent.blocked_by_item_id = Some(blocker_id);
        repository
            .create(dependent, idempotency("items.create", "valid-blocker", 5))
            .await
            .expect("existing blocker identity");
        repository
            .trash(
                blocker_id,
                1,
                Utc::now(),
                idempotency("items.delete", "trash-blocker", 6),
            )
            .await
            .expect("soft-delete preserves canonical identity");

        let mut historical = new_item(Uuid::new_v4(), "Historical blocker", None, 0);
        historical.status = ItemStatus::Blocked;
        historical.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
        historical.blocked_by_item_id = Some(blocker_id);
        repository
            .create(
                historical,
                idempotency("items.create", "trashed-blocker", 7),
            )
            .await
            .expect("trashed blocker remains a resolvable historical identity");
    }
}
