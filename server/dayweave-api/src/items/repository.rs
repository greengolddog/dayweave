use std::{
    collections::{BTreeSet, HashMap, HashSet},
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

/// A proposal contains at most 100 commands, and one command can emit its
/// direct aggregate plus refreshes for its old and new parent. Keeping the
/// bound beside delta pagination makes response expansion explicit and keeps
/// corrupt or accidentally oversized writer groups fail-closed.
pub(crate) const MAX_ITEM_CHANGE_GROUP_SIZE: usize = 300;

/// Leaves ample room inside the native clients' 12 MiB/16 MiB response
/// envelopes for JSON keys, cursors, and decoder overhead.
pub(crate) const MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// One atomic group must fit wholly inside one payload-bounded page.
pub(crate) const MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES: usize = MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES;

/// Returns the largest valid response for a requested delta limit. A page may
/// contain `limit - 1` independent rows followed by one complete maximum-size
/// atomic group.
pub(crate) fn max_expanded_delta_page_size(
    requested_limit: usize,
) -> Result<usize, ItemRepositoryError> {
    if requested_limit == 0 {
        return Err(ItemRepositoryError::Internal);
    }
    requested_limit
        .checked_add(MAX_ITEM_CHANGE_GROUP_SIZE - 1)
        .ok_or(ItemRepositoryError::Internal)
}

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
    #[error("dependency predecessor item {0} was not found")]
    DependencyNotFound(Uuid),
    #[error(
        "dependency from item {successor_id} to item {predecessor_id} crosses a materialized recurring subtree boundary"
    )]
    CrossRecurringSubtreeDependency {
        successor_id: Uuid,
        predecessor_id: Uuid,
    },
    #[error("item dependency graph would contain a cycle")]
    DependencyCycle,
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
    #[error("atomic item delta group exceeds its safe delivery bound")]
    DeltaGroupTooLarge,
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
    change_group_id: Option<Uuid>,
    change: DeltaChange,
}

/// Chooses a delta prefix without cutting the atomic group that intersects the
/// requested boundary. Callers provide at most `limit + group_cap` rows, which
/// leaves one look-ahead row after the largest valid expanded page.
pub(crate) fn atomic_delta_prefix_len(
    change_group_ids: &[Option<Uuid>],
    requested_limit: usize,
) -> Result<usize, ItemRepositoryError> {
    let maximum_page = max_expanded_delta_page_size(requested_limit)?;
    if change_group_ids.len() <= requested_limit {
        return Ok(change_group_ids.len());
    }
    let Some(boundary_group_id) = change_group_ids[requested_limit - 1] else {
        return Ok(requested_limit);
    };

    let group_start = change_group_ids[..requested_limit]
        .iter()
        .rposition(|group_id| *group_id != Some(boundary_group_id))
        .map_or(0, |index| index + 1);
    let group_end = change_group_ids[requested_limit..]
        .iter()
        .position(|group_id| *group_id != Some(boundary_group_id))
        .map_or(change_group_ids.len(), |offset| requested_limit + offset);
    let group_size = group_end - group_start;
    if group_size > MAX_ITEM_CHANGE_GROUP_SIZE || group_end > maximum_page {
        return Err(ItemRepositoryError::DeltaGroupTooLarge);
    }
    Ok(group_end)
}

/// Revalidates every complete group selected for delivery. This is deliberately
/// independent from write-side checks so malformed rows inserted outside the
/// repository cannot force an unbounded or native-undecodable response.
pub(crate) fn validate_atomic_delta_groups(
    change_group_ids: &[Option<Uuid>],
    payload_sizes: &[usize],
) -> Result<(), ItemRepositoryError> {
    if change_group_ids.len() != payload_sizes.len() {
        return Err(ItemRepositoryError::Internal);
    }
    let mut completed = HashSet::new();
    let mut current_group_id = None;
    let mut current_count = 0_usize;
    let mut current_payload_bytes = 0_usize;

    let finish_group = |group_id: Option<Uuid>, count: usize, payload_bytes: usize| {
        if group_id.is_some()
            && (count > MAX_ITEM_CHANGE_GROUP_SIZE
                || payload_bytes > MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES)
        {
            Err(ItemRepositoryError::DeltaGroupTooLarge)
        } else {
            Ok(())
        }
    };

    for (group_id, payload_size) in change_group_ids.iter().zip(payload_sizes) {
        if *group_id != current_group_id {
            finish_group(current_group_id, current_count, current_payload_bytes)?;
            if let Some(group_id) = current_group_id {
                completed.insert(group_id);
            }
            if group_id.is_some_and(|group_id| completed.contains(&group_id)) {
                return Err(ItemRepositoryError::Internal);
            }
            current_group_id = *group_id;
            current_count = 0;
            current_payload_bytes = 0;
        }
        if group_id.is_some() {
            current_count = current_count
                .checked_add(1)
                .ok_or(ItemRepositoryError::Internal)?;
            current_payload_bytes = current_payload_bytes
                .checked_add(*payload_size)
                .ok_or(ItemRepositoryError::Internal)?;
        }
    }
    finish_group(current_group_id, current_count, current_payload_bytes)
}

/// Applies the serialized payload ceiling after count expansion, stopping only
/// between independent rows or complete transaction groups. A valid first unit
/// always makes progress; an individually undeliverable unit fails closed.
pub(crate) fn delivery_bounded_delta_prefix_len(
    change_group_ids: &[Option<Uuid>],
    payload_sizes: &[usize],
    requested_limit: usize,
) -> Result<usize, ItemRepositoryError> {
    if change_group_ids.len() != payload_sizes.len() {
        return Err(ItemRepositoryError::Internal);
    }
    let count_prefix = atomic_delta_prefix_len(change_group_ids, requested_limit)?;
    validate_atomic_delta_groups(
        &change_group_ids[..count_prefix],
        &payload_sizes[..count_prefix],
    )?;

    let mut selected = 0_usize;
    let mut selected_payload_bytes = 0_usize;
    while selected < count_prefix {
        let unit_end = match change_group_ids[selected] {
            Some(group_id) => change_group_ids[selected + 1..count_prefix]
                .iter()
                .position(|candidate| *candidate != Some(group_id))
                .map_or(count_prefix, |offset| selected + offset + 1),
            None => selected + 1,
        };
        let unit_payload_bytes = payload_sizes[selected..unit_end]
            .iter()
            .try_fold(0_usize, |total, size| total.checked_add(*size))
            .ok_or(ItemRepositoryError::Internal)?;
        if unit_payload_bytes > MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES {
            return Err(ItemRepositoryError::DeltaGroupTooLarge);
        }
        let next_payload_bytes = selected_payload_bytes
            .checked_add(unit_payload_bytes)
            .ok_or(ItemRepositoryError::Internal)?;
        if next_payload_bytes > MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES {
            break;
        }
        selected = unit_end;
        selected_payload_bytes = next_payload_bytes;
    }
    if count_prefix > 0 && selected == 0 {
        Err(ItemRepositoryError::DeltaGroupTooLarge)
    } else {
        Ok(selected)
    }
}

fn validate_memory_group_completeness(
    all_changes: &[MemoryChange],
    selected_changes: &[MemoryChange],
) -> Result<(), ItemRepositoryError> {
    let selected_counts =
        selected_changes
            .iter()
            .fold(HashMap::<Uuid, usize>::new(), |mut counts, change| {
                if let Some(group_id) = change.change_group_id {
                    *counts.entry(group_id).or_default() += 1;
                }
                counts
            });
    for (group_id, selected_count) in selected_counts {
        let matching = all_changes
            .iter()
            .enumerate()
            .filter(|(_, change)| change.change_group_id == Some(group_id))
            .collect::<Vec<_>>();
        let payload_bytes = matching.iter().try_fold(0_usize, |total, (_, change)| {
            let size = serde_json::to_vec(&change.change)
                .map_err(|_| ItemRepositoryError::Internal)?
                .len();
            total.checked_add(size).ok_or(ItemRepositoryError::Internal)
        })?;
        if matching.len() > MAX_ITEM_CHANGE_GROUP_SIZE
            || payload_bytes > MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES
        {
            return Err(ItemRepositoryError::DeltaGroupTooLarge);
        }
        let contiguous = matching.first().zip(matching.last()).is_some_and(
            |((first_index, _), (last_index, _))| last_index - first_index + 1 == matching.len(),
        );
        if matching.len() != selected_count || !contiguous {
            return Err(ItemRepositoryError::Internal);
        }
    }
    Ok(())
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
        let first_index = state
            .changes
            .partition_point(|change| change.sequence <= after);
        if first_index > 0
            && first_index < state.changes.len()
            && state.changes[first_index].change_group_id.is_some()
            && state.changes[first_index].change_group_id
                == state.changes[first_index - 1].change_group_id
        {
            return Err(ItemRepositoryError::InvalidCursor);
        }
        let fetch_limit = limit
            .checked_add(MAX_ITEM_CHANGE_GROUP_SIZE)
            .ok_or(ItemRepositoryError::Internal)?;
        let available = &state.changes[first_index..];
        let candidates = &available[..available.len().min(fetch_limit)];
        let group_ids = candidates
            .iter()
            .map(|change| change.change_group_id)
            .collect::<Vec<_>>();
        let payload_sizes = candidates
            .iter()
            .map(|change| {
                serde_json::to_vec(&change.change)
                    .map(|payload| payload.len())
                    .map_err(|_| ItemRepositoryError::Internal)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let selected_count = delivery_bounded_delta_prefix_len(&group_ids, &payload_sizes, limit)?;
        validate_memory_group_completeness(&state.changes, &candidates[..selected_count])?;
        let has_more = available.len() > selected_count;
        let watermark = candidates
            .get(selected_count.saturating_sub(1))
            .map_or(after, |change| change.sequence);
        let changes = candidates[..selected_count]
            .iter()
            .map(|change| change.change.clone())
            .collect();
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
    let change_group_id = Uuid::new_v4();
    if next.items.contains_key(&item.id) {
        return Err(ItemRepositoryError::Duplicate(item.id));
    }
    validate_parent(&next.items, item.id, item.parent_id)?;
    validate_blocked_by(&next.items, item.blocked_by_item_id)?;
    // The repository owns the topology-aware projection even when an internal
    // adapter supplies a stale pre-structural value.
    item.is_executable = item.execution_is_allowed(false);
    next.items.insert(item.id, item.clone());
    validate_dependency_graph(&next.items)?;
    append_change(
        &mut next,
        Some(change_group_id),
        DeltaChange::Upsert {
            item: Box::new(item.clone()),
        },
    )?;
    refresh_parents(
        &mut next,
        [item.parent_id],
        item.updated_at,
        change_group_id,
    )?;
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
    let change_group_id = Uuid::new_v4();
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
    validate_dependency_graph(&next.items)?;
    append_change(
        &mut next,
        Some(change_group_id),
        DeltaChange::Upsert {
            item: Box::new(item.clone()),
        },
    )?;
    if previous_parent_id != item.parent_id || previous_sibling_order != item.sibling_order {
        refresh_parents(
            &mut next,
            [previous_parent_id, item.parent_id],
            item.updated_at,
            change_group_id,
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
    let change_group_id = Uuid::new_v4();
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
    validate_dependency_graph(&next.items)?;
    append_change(
        &mut next,
        Some(change_group_id),
        DeltaChange::Upsert {
            item: Box::new(item.clone()),
        },
    )?;
    refresh_parents(
        &mut next,
        [item.parent_id],
        item.updated_at,
        change_group_id,
    )?;
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
    let change_group_id = Uuid::new_v4();
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
        Some(change_group_id),
        DeltaChange::Tombstone {
            tombstone: ItemTombstone {
                id,
                revision: item.revision,
                deleted_at,
                parent_id: item.parent_id,
            },
        },
    )?;
    refresh_parents(
        &mut next,
        [item.parent_id],
        item.updated_at,
        change_group_id,
    )?;
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

pub(crate) fn validate_dependency_graph(
    items: &HashMap<Uuid, Item>,
) -> Result<(), ItemRepositoryError> {
    let recurring_owners = recurring_subtree_owners(items)?;
    let mut adjacency: HashMap<Uuid, Vec<Uuid>> =
        items.keys().copied().map(|id| (id, Vec::new())).collect();
    let mut indegree: HashMap<Uuid, usize> = items.keys().copied().map(|id| (id, 0)).collect();

    for (successor_id, item) in items {
        for dependency in item.dependencies()? {
            let predecessor_id = dependency.item_id.0;
            if !items.contains_key(&predecessor_id) {
                return Err(ItemRepositoryError::DependencyNotFound(predecessor_id));
            }
            if let Some(predecessor_owner) = recurring_owners.get(&predecessor_id)
                && recurring_owners.get(successor_id) != Some(predecessor_owner)
            {
                return Err(ItemRepositoryError::CrossRecurringSubtreeDependency {
                    successor_id: *successor_id,
                    predecessor_id,
                });
            }
            add_dependency_edge(&mut adjacency, &mut indegree, predecessor_id, *successor_id);
        }
    }

    for routine in items.values().filter(|item| {
        item.deleted_at.is_none()
            && item.kind == super::ItemKind::Routine
            && item
                .flexible_constraints
                .get("routine_ordered")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }) {
        let mut children: Vec<_> = items
            .values()
            .filter(|item| item.deleted_at.is_none() && item.parent_id == Some(routine.id))
            .collect();
        children.sort_by_key(|item| (item.sibling_order, item.id));
        for pair in children.windows(2) {
            add_dependency_edge(&mut adjacency, &mut indegree, pair[0].id, pair[1].id);
        }
    }

    for successors in adjacency.values_mut() {
        successors.sort_unstable();
        successors.dedup();
    }
    // Recompute after de-duplicating explicit and derived ordered-routine edges.
    for value in indegree.values_mut() {
        *value = 0;
    }
    for successors in adjacency.values() {
        for successor in successors {
            *indegree
                .get_mut(successor)
                .ok_or(ItemRepositoryError::Internal)? += 1;
        }
    }

    let mut ready: BTreeSet<_> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect();
    let mut visited = 0_usize;
    while let Some(id) = ready.pop_first() {
        visited = visited
            .checked_add(1)
            .ok_or(ItemRepositoryError::Internal)?;
        for successor in adjacency.get(&id).into_iter().flatten() {
            let degree = indegree
                .get_mut(successor)
                .ok_or(ItemRepositoryError::Internal)?;
            *degree = degree.checked_sub(1).ok_or(ItemRepositoryError::Internal)?;
            if *degree == 0 {
                ready.insert(*successor);
            }
        }
    }
    if visited == items.len() {
        Ok(())
    } else {
        Err(ItemRepositoryError::DependencyCycle)
    }
}

fn recurring_subtree_owners(
    items: &HashMap<Uuid, Item>,
) -> Result<HashMap<Uuid, Uuid>, ItemRepositoryError> {
    let mut resolved = HashMap::<Uuid, Option<Uuid>>::new();
    for start in items.keys().copied() {
        if resolved.contains_key(&start) {
            continue;
        }
        let mut path = Vec::new();
        let mut visiting = HashSet::new();
        let mut current = Some(start);
        let mut owner = None;
        while let Some(item_id) = current {
            if let Some(cached) = resolved.get(&item_id) {
                owner = *cached;
                break;
            }
            if !visiting.insert(item_id) {
                return Err(ItemRepositoryError::HierarchyCycle);
            }
            let item = items
                .get(&item_id)
                .ok_or(ItemRepositoryError::ParentNotFound(item_id))?;
            path.push(item_id);
            current = item.parent_id;
        }
        for item_id in path.into_iter().rev() {
            if owner.is_none() && items[&item_id].recurrence.is_some() {
                owner = Some(item_id);
            }
            resolved.insert(item_id, owner);
        }
    }
    Ok(resolved
        .into_iter()
        .filter_map(|(item_id, owner)| owner.map(|owner| (item_id, owner)))
        .collect())
}

fn add_dependency_edge(
    adjacency: &mut HashMap<Uuid, Vec<Uuid>>,
    indegree: &mut HashMap<Uuid, usize>,
    predecessor_id: Uuid,
    successor_id: Uuid,
) {
    adjacency
        .entry(predecessor_id)
        .or_default()
        .push(successor_id);
    indegree.entry(successor_id).or_default();
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
    change_group_id: Uuid,
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
            Some(change_group_id),
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

fn append_change(
    state: &mut MemoryState,
    change_group_id: Option<Uuid>,
    change: DeltaChange,
) -> Result<(), ItemRepositoryError> {
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .ok_or(ItemRepositoryError::Internal)?;
    state.changes.push(MemoryChange {
        sequence: state.next_sequence,
        change_group_id,
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

    fn dependency_json(predecessor_id: Uuid, strength: &serde_json::Value) -> serde_json::Value {
        json!({
            "constraints": {
                "dependencies": [{
                    "item_id": predecessor_id,
                    "relation": "finish_to_start",
                    "minimum_lag": 15,
                    "strength": strength
                }]
            }
        })
    }

    #[test]
    fn atomic_delta_page_expansion_is_bounded() {
        assert_eq!(
            MAX_ITEM_CHANGE_GROUP_SIZE,
            crate::proposals::MAX_PROPOSAL_COMMANDS * 3,
            "the delivery bound tracks direct plus two-parent effects per proposal command"
        );
        let group_id = Uuid::new_v4();
        let mut group_ids = vec![None; 199];
        group_ids.extend(std::iter::repeat_n(
            Some(group_id),
            MAX_ITEM_CHANGE_GROUP_SIZE,
        ));
        group_ids.push(None);

        assert_eq!(
            max_expanded_delta_page_size(200).unwrap(),
            499,
            "a maximum request can expand only through one bounded group"
        );
        assert_eq!(atomic_delta_prefix_len(&group_ids, 200).unwrap(), 499);
        assert_eq!(
            atomic_delta_prefix_len(
                &std::iter::repeat_n(Some(group_id), MAX_ITEM_CHANGE_GROUP_SIZE + 1)
                    .collect::<Vec<_>>(),
                1,
            ),
            Err(ItemRepositoryError::DeltaGroupTooLarge),
            "an oversized writer group must fail closed instead of expanding without bound"
        );
        assert_eq!(
            validate_atomic_delta_groups(
                &[Some(group_id)],
                &[MAX_ITEM_CHANGE_GROUP_PAYLOAD_BYTES + 1],
            ),
            Err(ItemRepositoryError::DeltaGroupTooLarge),
            "a grouped response that cannot fit native decode limits must fail closed"
        );

        let mixed_group_ids = [None, Some(group_id), Some(group_id), None];
        let mixed_payload_sizes = [MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES - 10, 6, 6, 1];
        assert_eq!(
            delivery_bounded_delta_prefix_len(&mixed_group_ids, &mixed_payload_sizes, 2).unwrap(),
            1,
            "the page stops before a complete group when the group would cross its byte ceiling"
        );
        assert_eq!(
            delivery_bounded_delta_prefix_len(&mixed_group_ids[1..], &mixed_payload_sizes[1..], 1,)
                .unwrap(),
            2,
            "the next page delivers the complete group and makes progress"
        );

        let ungrouped_payload_sizes = vec![200 * 1024; 50];
        assert_eq!(
            delivery_bounded_delta_prefix_len(&vec![None; 50], &ungrouped_payload_sizes, 50)
                .unwrap(),
            MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES / (200 * 1024),
            "ordinary ungrouped bulk rows are also bounded by total serialized payload"
        );
    }

    #[tokio::test]
    async fn in_memory_delta_never_splits_direct_mutation_parent_refreshes() {
        let repository = InMemoryItemRepository::default();
        let parent_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        repository
            .create(
                new_item(parent_id, "Parent", None, 0),
                idempotency("items.create", "atomic-parent", 71),
            )
            .await
            .unwrap();
        let baseline = repository.delta(0, 1).await.unwrap();
        assert_eq!(baseline.changes.len(), 1);
        assert!(!baseline.has_more);

        repository
            .create(
                new_item(child_id, "Child", Some(parent_id), 0),
                idempotency("items.create", "atomic-child", 72),
            )
            .await
            .unwrap();
        let child_page = repository.delta(baseline.watermark, 1).await.unwrap();
        assert_eq!(child_page.changes.len(), 2);
        assert!(!child_page.has_more);
        assert!(matches!(
            &child_page.changes[0],
            DeltaChange::Upsert { item } if item.id == child_id
        ));
        assert!(matches!(
            &child_page.changes[1],
            DeltaChange::Upsert { item } if item.id == parent_id
        ));

        assert_eq!(
            repository.delta(baseline.watermark + 1, 1).await,
            Err(ItemRepositoryError::InvalidCursor),
            "a cursor inside a grouped mutation cannot silently lose part of that mutation"
        );

        let corrupt_repository = InMemoryItemRepository::default();
        let corrupt_group_id = Uuid::new_v4();
        {
            let mut state = corrupt_repository.state.lock().await;
            for ordinal in 0..302 {
                let group_id = (ordinal == 0 || ordinal == 301).then_some(corrupt_group_id);
                append_change(
                    &mut state,
                    group_id,
                    DeltaChange::Upsert {
                        item: Box::new(new_item(Uuid::new_v4(), "Synthetic", None, 0)),
                    },
                )
                .unwrap();
            }
        }
        assert_eq!(
            corrupt_repository.delta(0, 1).await,
            Err(ItemRepositoryError::Internal),
            "group reuse outside the fetch window is detected from authoritative memory state"
        );
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

    #[tokio::test]
    async fn dependency_graph_requires_existing_predecessors_and_rejects_all_cycles() {
        let repository = InMemoryItemRepository::default();
        let missing_id = Uuid::new_v4();
        let mut missing = new_item(Uuid::new_v4(), "Missing predecessor", None, 0);
        missing.flexible_constraints = dependency_json(missing_id, &json!({"level": "hard"}));
        assert_eq!(
            repository
                .create(
                    missing,
                    idempotency("items.create", "missing-dependency", 21),
                )
                .await
                .expect_err("unknown dependency must fail"),
            ItemRepositoryError::DependencyNotFound(missing_id)
        );

        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let first = new_item(first_id, "First", None, 0);
        repository
            .create(
                first.clone(),
                idempotency("items.create", "dependency-first", 22),
            )
            .await
            .unwrap();
        let mut second = new_item(second_id, "Second", None, 0);
        second.flexible_constraints =
            dependency_json(first_id, &json!({"level": "soft", "weight": 400}));
        repository
            .create(
                second.clone(),
                idempotency("items.create", "dependency-second", 23),
            )
            .await
            .unwrap();
        assert_eq!(
            repository
                .get(second_id, false)
                .await
                .unwrap()
                .dependencies()
                .unwrap()[0]
                .item_id
                .0,
            first_id
        );

        let mut cycle = replacement(&first, None, ItemStatus::Planned);
        cycle.flexible_constraints = dependency_json(second_id, &json!({"level": "hard"}));
        assert_eq!(
            repository
                .replace(
                    first_id,
                    1,
                    cycle,
                    Utc::now(),
                    idempotency("items.replace", "dependency-cycle", 24),
                )
                .await
                .expect_err("hard and soft edges share one acyclic graph"),
            ItemRepositoryError::DependencyCycle
        );
        assert!(
            repository
                .get(first_id, false)
                .await
                .unwrap()
                .dependencies()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn dependencies_must_stay_inside_a_materialized_recurring_subtree() {
        let repository = InMemoryItemRepository::default();
        let routine_id = Uuid::new_v4();
        let predecessor_id = Uuid::new_v4();
        let internal_successor_id = Uuid::new_v4();

        let mut routine = new_item(routine_id, "Recurring routine", None, 0);
        routine.kind = ItemKind::Routine;
        routine.has_own_effort = false;
        routine.recurrence = Some(json!({"type": "daily", "times_per_day": 1}));
        routine.flexible_constraints = json!({"routine_ordered": false});
        repository
            .create(
                routine,
                idempotency("items.create", "recurring-boundary-root", 41),
            )
            .await
            .unwrap();

        repository
            .create(
                new_item(predecessor_id, "Recurring step", Some(routine_id), 0),
                idempotency("items.create", "recurring-boundary-step", 42),
            )
            .await
            .unwrap();
        let mut internal_successor = new_item(
            internal_successor_id,
            "Internal successor",
            Some(routine_id),
            1,
        );
        internal_successor.flexible_constraints =
            dependency_json(predecessor_id, &json!({"level": "hard"}));
        repository
            .create(
                internal_successor,
                idempotency("items.create", "recurring-boundary-internal", 43),
            )
            .await
            .expect("an occurrence-local dependency is rewritten with its recurring subtree");

        let external_id = Uuid::new_v4();
        let mut external = new_item(external_id, "External successor", None, 0);
        external.flexible_constraints =
            dependency_json(predecessor_id, &json!({"level": "soft", "weight": 1}));
        assert_eq!(
            repository
                .create(
                    external,
                    idempotency("items.create", "recurring-boundary-external", 44),
                )
                .await
                .expect_err("an external dependency would retain a dangling series item id"),
            ItemRepositoryError::CrossRecurringSubtreeDependency {
                successor_id: external_id,
                predecessor_id,
            }
        );
    }

    #[tokio::test]
    async fn ordered_routine_edges_participate_in_cycle_prevention() {
        let repository = InMemoryItemRepository::default();
        let routine_id = Uuid::new_v4();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let mut routine = new_item(routine_id, "Morning", None, 0);
        routine.kind = ItemKind::Routine;
        routine.has_own_effort = false;
        routine.flexible_constraints = json!({"routine_ordered": true});
        repository
            .create(routine, idempotency("items.create", "ordered-routine", 31))
            .await
            .unwrap();
        let first = new_item(first_id, "First step", Some(routine_id), 0);
        repository
            .create(
                first.clone(),
                idempotency("items.create", "ordered-first", 32),
            )
            .await
            .unwrap();
        repository
            .create(
                new_item(second_id, "Second step", Some(routine_id), 1),
                idempotency("items.create", "ordered-second", 33),
            )
            .await
            .unwrap();

        let current = repository.get(first_id, false).await.unwrap();
        let mut replacement = replacement(&current, Some(routine_id), ItemStatus::Planned);
        replacement.flexible_constraints =
            dependency_json(second_id, &json!({"level": "soft", "weight": 1}));
        assert_eq!(
            repository
                .replace(
                    first_id,
                    current.revision,
                    replacement,
                    Utc::now(),
                    idempotency("items.replace", "ordered-cycle", 34),
                )
                .await
                .expect_err("explicit edge must not reverse an ordered routine edge"),
            ItemRepositoryError::DependencyCycle
        );
    }
}
