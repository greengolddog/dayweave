use std::{sync::Arc, time::Duration as StdDuration};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    items::{ItemService, ItemServiceError},
    proposals::Clock,
    scheduling::truncate_to_postgres_timestamp_precision,
};

use super::{
    DeferAssessment, DeferAssessmentRequest, ExecutionCommand, ExecutionDomainError,
    ExecutionIdempotency, ExecutionMutation, ExecutionRepository, ExecutionRepositoryError,
    ExecutionSession, ExecutionSnapshot, StartExecution,
    invalidation::{
        ExecutionInvalidationConfig, ExecutionInvalidationHub, ExecutionInvalidationOpenError,
        ExecutionInvalidationStream,
    },
};

const IDEMPOTENCY_TTL: StdDuration = StdDuration::from_hours(24);
const MAX_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct ExecutionIdempotencyKey {
    pub key: String,
    pub fingerprint: [u8; 32],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionHistoryPage {
    pub sessions: Vec<ExecutionSession>,
    pub next_offset: Option<usize>,
}

pub struct ExecutionService {
    repository: Arc<dyn ExecutionRepository>,
    items: Arc<ItemService>,
    clock: Arc<dyn Clock>,
    start_operation_gate: Option<Arc<Mutex<()>>>,
    invalidations: ExecutionInvalidationHub,
}

impl std::fmt::Debug for ExecutionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionService")
            .finish_non_exhaustive()
    }
}

impl ExecutionService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn ExecutionRepository>,
        items: Arc<ItemService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            repository,
            items,
            clock,
            start_operation_gate: None,
            invalidations: ExecutionInvalidationHub::new(ExecutionInvalidationConfig::default()),
        }
    }

    pub(crate) fn with_start_operation_gate(mut self, gate: Arc<Mutex<()>>) -> Self {
        self.start_operation_gate = Some(gate);
        self
    }

    /// Replaces the bounded invalidation stream configuration while assembling
    /// a service. Primarily useful for deterministic embedded/HTTP tests.
    #[must_use]
    pub fn with_invalidation_config(mut self, config: ExecutionInvalidationConfig) -> Self {
        self.invalidations = ExecutionInvalidationHub::new(config);
        self
    }

    pub(crate) async fn invalidation_stream(
        &self,
        cursor: u64,
    ) -> Result<ExecutionInvalidationStream, ExecutionInvalidationOpenError> {
        self.invalidations
            .open(self.repository.clone(), cursor)
            .await
    }

    /// Returns the canonical cross-device execution lease.
    ///
    /// # Errors
    ///
    /// Returns a redacted repository error when state cannot be loaded.
    pub async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionServiceError> {
        Ok(self.repository.snapshot().await?)
    }

    /// Assesses an exact future replacement for the currently paused session.
    ///
    /// The durable repository derives duration, policy, schedule, Calendar,
    /// and execution evidence; callers choose only the target start and an
    /// optional corrected actual duration.
    ///
    /// # Errors
    ///
    /// Returns validation, stale-evidence, approval, or storage errors without
    /// mutating the execution lease.
    pub async fn assess_defer(
        &self,
        request: DeferAssessmentRequest,
    ) -> Result<DeferAssessment, ExecutionServiceError> {
        if request.expected_revision > i64::MAX as u64 {
            return Err(ExecutionServiceError::InvalidRevision);
        }
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        Ok(self.repository.assess_defer(request, now).await?)
    }

    /// Applies one idempotent execution command under optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Returns validation, item, concurrency, idempotency, or storage errors.
    pub async fn command(
        &self,
        expected_revision: u64,
        command: ExecutionCommand,
        key: ExecutionIdempotencyKey,
    ) -> Result<ExecutionMutation, ExecutionServiceError> {
        validate_idempotency_key(&key.key)?;
        if expected_revision > i64::MAX as u64 {
            return Err(ExecutionServiceError::InvalidRevision);
        }
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        let ttl = chrono::Duration::from_std(IDEMPOTENCY_TTL)
            .map_err(|_| ExecutionServiceError::Internal)?;
        let idempotency = ExecutionIdempotency {
            key_hash: Sha256::digest(key.key.as_bytes()).into(),
            fingerprint: key.fingerprint,
            expires_at: now + ttl,
        };

        // A retry must keep succeeding even if the referenced item changed after
        // the original transaction. The repository repeats this check inside
        // `apply` to close the concurrent first-request race.
        if let Some(mutation) = self.repository.replay(now, &idempotency).await? {
            self.invalidations.publish(mutation.revision);
            return Ok(mutation);
        }
        command.validate(now)?;

        if let ExecutionCommand::Start(input) = &command {
            if let Some(gate) = &self.start_operation_gate {
                let _operation = gate.lock().await;
                self.validate_start_item(input).await?;
                let mutation = self
                    .repository
                    .apply(expected_revision, command, now, idempotency)
                    .await?;
                self.invalidations.publish(mutation.revision);
                return Ok(mutation);
            }
            self.validate_start_item(input).await?;
        }

        let mutation = self
            .repository
            .apply(expected_revision, command, now, idempotency)
            .await?;
        self.invalidations.publish(mutation.revision);
        Ok(mutation)
    }

    async fn validate_start_item(
        &self,
        input: &StartExecution,
    ) -> Result<(), ExecutionServiceError> {
        let item = self.items.get(input.item_id).await?;
        if item.revision != input.item_revision {
            return Err(ExecutionServiceError::ItemRevisionConflict {
                expected: input.item_revision,
                actual: item.revision,
            });
        }
        if !item.is_executable || item.status.prevents_execution() {
            return Err(ExecutionServiceError::ItemNotExecutable);
        }
        Ok(())
    }

    /// Returns newest-first immutable execution history.
    ///
    /// # Errors
    ///
    /// Returns validation or storage errors.
    pub async fn history(
        &self,
        limit: usize,
    ) -> Result<Vec<super::ExecutionSession>, ExecutionServiceError> {
        Ok(self.history_page(limit, 0).await?.sessions)
    }

    /// Returns one newest-first history page with an exact continuation offset.
    ///
    /// # Errors
    ///
    /// Returns validation errors for an unsupported page shape and propagates storage failures.
    pub async fn history_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<ExecutionHistoryPage, ExecutionServiceError> {
        if !(1..=MAX_HISTORY_LIMIT).contains(&limit) {
            return Err(ExecutionServiceError::InvalidHistoryLimit);
        }
        if i64::try_from(offset).is_err() {
            return Err(ExecutionServiceError::InvalidHistoryOffset);
        }
        let probe_limit = limit
            .checked_add(1)
            .ok_or(ExecutionServiceError::Internal)?;
        let mut sessions = self.repository.history(probe_limit, offset).await?;
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let next_offset = if has_more {
            Some(
                offset
                    .checked_add(limit)
                    .ok_or(ExecutionServiceError::Internal)?,
            )
        } else {
            None
        };
        Ok(ExecutionHistoryPage {
            sessions,
            next_offset,
        })
    }
}

#[derive(Debug, Error)]
pub enum ExecutionServiceError {
    #[error(transparent)]
    Domain(#[from] ExecutionDomainError),
    #[error(transparent)]
    Repository(#[from] ExecutionRepositoryError),
    #[error(transparent)]
    Item(#[from] ItemServiceError),
    #[error("Idempotency-Key must be 8-128 URL-safe ASCII characters")]
    InvalidIdempotencyKey,
    #[error("expected_revision is outside the supported range")]
    InvalidRevision,
    #[error("history limit must be between 1 and 100")]
    InvalidHistoryLimit,
    #[error("history offset is outside the supported range")]
    InvalidHistoryOffset,
    #[error("item revision conflict: expected {expected}, found {actual}")]
    ItemRevisionConflict { expected: u64, actual: u64 },
    #[error("item is not an executable active leaf")]
    ItemNotExecutable,
    #[error("execution service operation failed")]
    Internal,
}

fn validate_idempotency_key(value: &str) -> Result<(), ExecutionServiceError> {
    if (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        Ok(())
    } else {
        Err(ExecutionServiceError::InvalidIdempotencyKey)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    use crate::{
        items::{
            BlockedReasonKind, IdempotencyKey, InMemoryItemRepository, ItemKind,
            ItemRepositoryError, ItemStatus, NewItem, ReplaceItem, SplitPolicy,
        },
        proposals::Clock,
    };

    use super::*;
    use crate::execution::{
        DeferExecution, ExecutionStatus, FinishExecution, InMemoryExecutionRepository,
        PauseExecution, StartExecution,
    };

    #[derive(Debug)]
    struct TestClock(RwLock<DateTime<Utc>>);

    impl TestClock {
        fn set(&self, now: DateTime<Utc>) {
            *self.0.write().unwrap() = now;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.read().unwrap()
        }
    }

    async fn fixture() -> (ExecutionService, Arc<TestClock>, Uuid) {
        fixture_with_status(ItemStatus::Planned).await
    }

    async fn fixture_with_status(status: ItemStatus) -> (ExecutionService, Arc<TestClock>, Uuid) {
        fixture_with_shape(status, ItemKind::Task, false).await
    }

    async fn fixture_with_shape(
        status: ItemStatus,
        kind: ItemKind,
        has_own_effort: bool,
    ) -> (ExecutionService, Arc<TestClock>, Uuid) {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let clock = Arc::new(TestClock(RwLock::new(now)));
        let items = Arc::new(ItemService::new(
            Arc::new(InMemoryItemRepository::default()),
            clock.clone(),
        ));
        let item_id = Uuid::from_u128(11);
        items
            .create(
                NewItem {
                    id: item_id,
                    is_sensitive: false,
                    kind,
                    status,
                    title: "Write".to_owned(),
                    notes: None,
                    timezone_name: "UTC".to_owned(),
                    duration_kind: None,
                    duration_seconds: Some(1_800),
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
                    flexible_constraints: if has_own_effort {
                        serde_json::json!({"has_own_effort": true})
                    } else {
                        serde_json::json!({})
                    },
                    has_own_effort: Some(has_own_effort),
                    split_policy: SplitPolicy::Indivisible,
                    importance: 50,
                    urgency: 50,
                    parent_id: None,
                    sibling_order: 0,
                    blocked_reason_kind: (status == ItemStatus::Blocked)
                        .then_some(BlockedReasonKind::Manual),
                    blocked_by_item_id: None,
                    blocked_reason: (status == ItemStatus::Blocked)
                        .then(|| "Waiting for input".to_owned()),
                },
                IdempotencyKey {
                    key: "create-item-11".to_owned(),
                    fingerprint: [1; 32],
                },
            )
            .await
            .unwrap();
        (
            ExecutionService::new(
                Arc::new(InMemoryExecutionRepository::default()),
                items,
                clock.clone(),
            ),
            clock,
            item_id,
        )
    }

    fn idempotency(byte: u8) -> ExecutionIdempotencyKey {
        ExecutionIdempotencyKey {
            key: format!("execution-key-{byte}"),
            fingerprint: [byte; 32],
        }
    }

    fn hierarchy_item(id: Uuid, title: &str, parent_id: Option<Uuid>) -> NewItem {
        NewItem {
            id,
            is_sensitive: false,
            kind: ItemKind::Task,
            status: ItemStatus::Planned,
            title: title.to_owned(),
            notes: None,
            timezone_name: "UTC".to_owned(),
            duration_kind: None,
            duration_seconds: Some(1_800),
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
            flexible_constraints: serde_json::json!({}),
            has_own_effort: Some(false),
            split_policy: SplitPolicy::Indivisible,
            importance: 50,
            urgency: 50,
            parent_id,
            sibling_order: 0,
            blocked_reason_kind: None,
            blocked_by_item_id: None,
            blocked_reason: None,
        }
    }

    fn hierarchy_replacement(item: &crate::items::Item, parent_id: Option<Uuid>) -> ReplaceItem {
        ReplaceItem {
            is_sensitive: item.is_sensitive,
            kind: item.kind,
            status: item.status,
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
    async fn blocked_item_cannot_start_execution() {
        let (service, _clock, item_id) = fixture_with_status(ItemStatus::Blocked).await;
        let result = service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id: Uuid::from_u128(12),
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(13),
                }),
                idempotency(23),
            )
            .await;
        assert!(matches!(
            result,
            Err(ExecutionServiceError::ItemNotExecutable)
        ));
    }

    #[tokio::test]
    async fn semantic_container_without_own_effort_cannot_start_execution() {
        let (service, _clock, item_id) =
            fixture_with_shape(ItemStatus::Planned, ItemKind::Project, false).await;
        let result = service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id: Uuid::from_u128(120),
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(130),
                }),
                idempotency(24),
            )
            .await;
        assert!(matches!(
            result,
            Err(ExecutionServiceError::ItemNotExecutable)
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One in-memory lifecycle proves the cross-repository execution fence.
    async fn active_task_cannot_be_retyped_as_a_container_without_own_effort() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let clock = Arc::new(TestClock(RwLock::new(now)));
        let execution_repository = Arc::new(InMemoryExecutionRepository::default());
        let operation_gate = Arc::new(Mutex::new(()));
        let item_repository = InMemoryItemRepository::with_execution_guard(
            execution_repository.clone(),
            operation_gate.clone(),
        );
        let items = Arc::new(ItemService::new(Arc::new(item_repository), clock.clone()));
        let item_id = Uuid::from_u128(140);
        items
            .create(
                NewItem {
                    id: item_id,
                    is_sensitive: false,
                    kind: ItemKind::Task,
                    status: ItemStatus::Planned,
                    title: "Active work".to_owned(),
                    notes: None,
                    timezone_name: "UTC".to_owned(),
                    duration_kind: None,
                    duration_seconds: Some(1_800),
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
                    flexible_constraints: serde_json::json!({}),
                    has_own_effort: Some(false),
                    split_policy: SplitPolicy::Indivisible,
                    importance: 50,
                    urgency: 50,
                    parent_id: None,
                    sibling_order: 0,
                    blocked_reason_kind: None,
                    blocked_by_item_id: None,
                    blocked_reason: None,
                },
                IdempotencyKey {
                    key: "create-active-task".to_owned(),
                    fingerprint: [31; 32],
                },
            )
            .await
            .expect("create active task fixture");
        let execution = ExecutionService::new(execution_repository, items.clone(), clock)
            .with_start_operation_gate(operation_gate);
        let session_id = Uuid::from_u128(141);
        execution
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(142),
                }),
                idempotency(32),
            )
            .await
            .expect("start task");
        let current = items.get(item_id).await.expect("load task");
        let conflict = items
            .replace(
                item_id,
                current.revision,
                ReplaceItem {
                    is_sensitive: current.is_sensitive,
                    kind: ItemKind::Project,
                    status: current.status,
                    title: current.title,
                    notes: current.notes,
                    timezone_name: current.timezone_name,
                    duration_kind: Some(current.duration_kind),
                    duration_seconds: current.duration_seconds,
                    duration_min_seconds: current.duration_min_seconds,
                    duration_max_seconds: current.duration_max_seconds,
                    duration_source: current.duration_source,
                    deadline_kind: Some(current.deadline_kind),
                    deadline_date: current.deadline_date,
                    deadline_at: current.deadline_at,
                    deadline_strength: current.deadline_strength,
                    deadline_soft_weight: current.deadline_soft_weight,
                    earliest_start_at: current.earliest_start_at,
                    recurrence: current.recurrence,
                    flexible_constraints: serde_json::json!({}),
                    has_own_effort: Some(false),
                    split_policy: current.split_policy,
                    importance: current.importance,
                    urgency: current.urgency,
                    parent_id: current.parent_id,
                    sibling_order: current.sibling_order,
                    blocked_reason_kind: current.blocked_reason_kind,
                    blocked_by_item_id: current.blocked_by_item_id,
                    blocked_reason: current.blocked_reason,
                },
                IdempotencyKey {
                    key: "retype-active-task".to_owned(),
                    fingerprint: [33; 32],
                },
            )
            .await
            .expect_err("active Task cannot lose its executable component");
        assert!(matches!(
            conflict,
            ItemServiceError::Repository(ItemRepositoryError::ActiveExecutionConflict {
                item_id: conflicted_item,
                session_id: conflicted_session,
            }) if conflicted_item == item_id && conflicted_session == session_id
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One scenario proves all three hierarchy mutations share the active-execution fence.
    async fn active_parent_rejects_child_create_reparent_and_restore() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let clock = Arc::new(TestClock(RwLock::new(now)));
        let execution_repository = Arc::new(InMemoryExecutionRepository::default());
        let operation_gate = Arc::new(Mutex::new(()));
        let item_repository = InMemoryItemRepository::with_execution_guard(
            execution_repository.clone(),
            operation_gate.clone(),
        );
        let items = Arc::new(ItemService::new(Arc::new(item_repository), clock.clone()));
        let parent_id = Uuid::from_u128(150);
        let movable_id = Uuid::from_u128(151);
        let restorable_id = Uuid::from_u128(152);
        for (item, key, marker) in [
            (
                hierarchy_item(parent_id, "Executing parent", None),
                "create-parent",
                41,
            ),
            (
                hierarchy_item(movable_id, "Movable child", None),
                "create-movable",
                42,
            ),
            (
                hierarchy_item(restorable_id, "Restorable child", Some(parent_id)),
                "create-restorable",
                43,
            ),
        ] {
            items
                .create(
                    item,
                    IdempotencyKey {
                        key: key.to_owned(),
                        fingerprint: [marker; 32],
                    },
                )
                .await
                .expect("create hierarchy fixture");
        }
        let restorable = items.get(restorable_id).await.expect("load restorable");
        items
            .trash(
                restorable_id,
                restorable.revision,
                IdempotencyKey {
                    key: "trash-restorable".to_owned(),
                    fingerprint: [44; 32],
                },
            )
            .await
            .expect("trash restores parent leaf state");
        let parent = items.get(parent_id).await.expect("load refreshed parent");
        assert!(parent.is_executable);

        let execution = ExecutionService::new(execution_repository, items.clone(), clock)
            .with_start_operation_gate(operation_gate);
        let session_id = Uuid::from_u128(153);
        execution
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id,
                    item_id: parent_id,
                    item_revision: parent.revision,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(154),
                }),
                idempotency(45),
            )
            .await
            .expect("start parent");

        let create_error = items
            .create(
                hierarchy_item(Uuid::from_u128(155), "New child", Some(parent_id)),
                IdempotencyKey {
                    key: "child-under-active-parent".to_owned(),
                    fingerprint: [46; 32],
                },
            )
            .await
            .expect_err("create cannot close an active parent's executable component");
        let movable = items.get(movable_id).await.expect("load movable");
        let reparent_error = items
            .replace(
                movable_id,
                movable.revision,
                hierarchy_replacement(&movable, Some(parent_id)),
                IdempotencyKey {
                    key: "reparent-under-active-parent".to_owned(),
                    fingerprint: [47; 32],
                },
            )
            .await
            .expect_err("reparent cannot close an active parent's executable component");
        let deleted = items
            .get_including_deleted(restorable_id)
            .await
            .expect("load trashed child");
        let restore_error = items
            .restore(
                restorable_id,
                deleted.revision,
                IdempotencyKey {
                    key: "restore-under-active-parent".to_owned(),
                    fingerprint: [48; 32],
                },
            )
            .await
            .expect_err("restore cannot close an active parent's executable component");

        for error in [create_error, reparent_error, restore_error] {
            assert!(matches!(
                error,
                ItemServiceError::Repository(ItemRepositoryError::ActiveExecutionConflict {
                    item_id: conflicted_item,
                    session_id: conflicted_session,
                }) if conflicted_item == parent_id && conflicted_session == session_id
            ));
        }
        assert!(items.get(parent_id).await.unwrap().is_executable);
        assert!(items.get(restorable_id).await.is_err());
        assert_eq!(items.get(movable_id).await.unwrap().parent_id, None);
    }

    #[tokio::test]
    async fn command_flow_is_server_authoritative_and_retryable() {
        let (service, clock, item_id) = fixture().await;
        let session_id = Uuid::from_u128(21);
        let start = ExecutionCommand::Start(StartExecution {
            session_id,
            item_id,
            item_revision: 1,
            occurrence_id: None,
            session_index: 0,
            planned_block_id: None,
            device_id: Uuid::from_u128(22),
        });
        let mutation = service
            .command(0, start.clone(), idempotency(2))
            .await
            .unwrap();
        assert_eq!(mutation.revision, 1);
        assert!(!mutation.replayed);
        let replay = service.command(0, start, idempotency(2)).await.unwrap();
        assert!(replay.replayed);

        clock.set(clock.now() + chrono::Duration::seconds(90));
        service
            .command(
                1,
                ExecutionCommand::Pause(PauseExecution {
                    session_id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: Some("break".to_owned()),
                }),
                idempotency(3),
            )
            .await
            .unwrap();
        clock.set(clock.now() + chrono::Duration::minutes(10));
        let complete = service
            .command(
                2,
                ExecutionCommand::Complete(FinishExecution {
                    session_id,
                    actual_seconds: None,
                }),
                idempotency(4),
            )
            .await
            .unwrap();
        assert_eq!(complete.changed_session.actual_seconds, Some(90));
        assert!(complete.active_session.is_none());
    }

    #[tokio::test]
    async fn exact_absolute_pause_retry_replays_after_its_deadline() {
        let (service, clock, item_id) = fixture().await;
        let session_id = Uuid::from_u128(31);
        service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(32),
                }),
                idempotency(5),
            )
            .await
            .unwrap();
        let pause_until = clock.now() + chrono::Duration::minutes(5);
        let pause = ExecutionCommand::Pause(PauseExecution {
            session_id,
            duration_seconds: None,
            pause_until: Some(pause_until),
            reason: None,
        });
        service
            .command(1, pause.clone(), idempotency(6))
            .await
            .unwrap();

        clock.set(pause_until + chrono::Duration::seconds(1));
        let replay = service.command(1, pause, idempotency(6)).await.unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.revision, 2);
    }

    #[tokio::test]
    async fn execution_protocol_clock_is_microsecond_exact_across_return_and_history() {
        let (service, clock, item_id) = fixture().await;
        let t0 = clock.now();
        clock.set(t0 + chrono::Duration::nanoseconds(999));
        let session_id = Uuid::from_u128(34);
        let started = service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(33),
                }),
                idempotency(16),
            )
            .await
            .unwrap();
        assert_eq!(started.changed_session.started_at, t0);
        assert_eq!(started.changed_session.running_since, Some(t0));
        assert_eq!(started.changed_session.created_at, t0);
        assert_eq!(started.changed_session.updated_at, t0);

        clock.set(t0 + chrono::Duration::seconds(5) + chrono::Duration::nanoseconds(999));
        let finished = service
            .command(
                1,
                ExecutionCommand::Complete(FinishExecution {
                    session_id,
                    actual_seconds: None,
                }),
                idempotency(17),
            )
            .await
            .unwrap();
        let terminal_at = t0 + chrono::Duration::seconds(5);
        assert_eq!(finished.changed_session.ended_at, Some(terminal_at));
        assert_eq!(finished.changed_session.updated_at, terminal_at);
        assert_eq!(finished.changed_session.accumulated_seconds, 5);

        let history = service.history(1).await.unwrap();
        assert_eq!(history, vec![finished.changed_session]);
        for instant in [
            history[0].started_at,
            history[0].ended_at.unwrap(),
            history[0].updated_at,
        ] {
            assert!(instant.timestamp_subsec_nanos().is_multiple_of(1_000));
        }
    }

    #[tokio::test]
    async fn execution_protocol_clock_is_strictly_monotonic_across_sessions() {
        let (service, clock, item_id) = fixture().await;
        let t0 = clock.now();
        let older_id = Uuid::from_u128(200);
        service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id: older_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(201),
                }),
                idempotency(18),
            )
            .await
            .unwrap();
        clock.set(t0 + chrono::Duration::seconds(10));
        let older = service
            .command(
                1,
                ExecutionCommand::Complete(FinishExecution {
                    session_id: older_id,
                    actual_seconds: None,
                }),
                idempotency(19),
            )
            .await
            .unwrap()
            .changed_session;

        // The later session deliberately has the lower UUID and observes a rolled-back clock.
        // Its persisted protocol instants, rather than the UUID tie-breaker, must establish cause.
        clock.set(t0 - chrono::Duration::hours(1));
        let newer_id = Uuid::from_u128(1);
        let newer = service
            .command(
                2,
                ExecutionCommand::Start(StartExecution {
                    session_id: newer_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 1,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(201),
                }),
                idempotency(20),
            )
            .await
            .unwrap()
            .changed_session;
        assert_eq!(
            newer.updated_at,
            older.updated_at + chrono::Duration::microseconds(1)
        );

        let paused = service
            .command(
                3,
                ExecutionCommand::Pause(PauseExecution {
                    session_id: newer_id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: None,
                }),
                idempotency(22),
            )
            .await
            .unwrap()
            .changed_session;
        assert_eq!(
            paused.updated_at,
            newer.updated_at + chrono::Duration::microseconds(1)
        );

        let deferred = service
            .command(
                4,
                ExecutionCommand::Defer(DeferExecution {
                    session_id: newer_id,
                    move_start: t0 + chrono::Duration::hours(1),
                    move_end: t0 + chrono::Duration::hours(2),
                    actual_seconds: None,
                    assessment_digest: None,
                    approved_assessment_digest: None,
                }),
                idempotency(21),
            )
            .await
            .unwrap()
            .changed_session;
        assert_eq!(
            deferred.updated_at,
            newer.updated_at + chrono::Duration::microseconds(2)
        );

        let history = service.history(2).await.unwrap();
        assert_eq!(history, vec![deferred, older]);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Keeps pause enforcement and replay in one atomic-flow test.
    async fn defer_requires_pause_then_closes_atomically_and_replays() {
        let (service, clock, item_id) = fixture().await;
        let first_id = Uuid::from_u128(35);
        service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id: first_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(36),
                }),
                idempotency(10),
            )
            .await
            .unwrap();

        clock.set(clock.now() + chrono::Duration::seconds(30));
        let first_move_start = clock.now() + chrono::Duration::hours(2);
        let first_move_end = first_move_start + chrono::Duration::hours(1);
        let defer_first = ExecutionCommand::Defer(DeferExecution {
            session_id: first_id,
            move_start: first_move_start,
            move_end: first_move_end,
            actual_seconds: None,
            assessment_digest: None,
            approved_assessment_digest: None,
        });
        let stale = service
            .command(0, defer_first.clone(), idempotency(11))
            .await
            .unwrap_err();
        assert!(matches!(
            stale,
            ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
                expected: 0,
                actual: 1
            })
        ));
        assert_eq!(service.snapshot().await.unwrap().revision, 1);

        let active = service
            .command(1, defer_first.clone(), idempotency(12))
            .await
            .unwrap_err();
        assert!(matches!(
            active,
            ExecutionServiceError::Repository(ExecutionRepositoryError::InvalidCommand(
                ExecutionDomainError::InvalidTransition
            ))
        ));
        assert_eq!(service.snapshot().await.unwrap().revision, 1);

        service
            .command(
                1,
                ExecutionCommand::Pause(PauseExecution {
                    session_id: first_id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: None,
                }),
                idempotency(16),
            )
            .await
            .unwrap();
        let deferred = service
            .command(2, defer_first.clone(), idempotency(17))
            .await
            .unwrap();
        assert_eq!(deferred.revision, 3);
        assert!(deferred.active_session.is_none());
        assert_eq!(deferred.changed_session.status, ExecutionStatus::Deferred);
        assert_eq!(deferred.changed_session.accumulated_seconds, 30);
        assert_eq!(deferred.changed_session.actual_seconds, Some(30));
        assert_eq!(deferred.changed_session.move_start, Some(first_move_start));
        assert_eq!(deferred.changed_session.move_end, Some(first_move_end));

        clock.set(first_move_end + chrono::Duration::seconds(1));
        let replay = service
            .command(2, defer_first, idempotency(17))
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.revision, 3);
        assert_eq!(replay.changed_session, deferred.changed_session);

        let second_id = Uuid::from_u128(37);
        service
            .command(
                3,
                ExecutionCommand::Start(StartExecution {
                    session_id: second_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 1,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(36),
                }),
                idempotency(13),
            )
            .await
            .unwrap();
        clock.set(clock.now() + chrono::Duration::seconds(20));
        service
            .command(
                4,
                ExecutionCommand::Pause(PauseExecution {
                    session_id: second_id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: Some("Interrupted".to_owned()),
                }),
                idempotency(14),
            )
            .await
            .unwrap();
        clock.set(clock.now() + chrono::Duration::minutes(10));
        let second_move_start = clock.now() + chrono::Duration::days(30);
        let second_move_end = second_move_start + chrono::Duration::minutes(45);
        let deferred = service
            .command(
                5,
                ExecutionCommand::Defer(DeferExecution {
                    session_id: second_id,
                    move_start: second_move_start,
                    move_end: second_move_end,
                    actual_seconds: Some(7),
                    assessment_digest: None,
                    approved_assessment_digest: None,
                }),
                idempotency(15),
            )
            .await
            .unwrap();
        assert_eq!(deferred.revision, 6);
        assert!(deferred.active_session.is_none());
        assert_eq!(deferred.changed_session.status, ExecutionStatus::Deferred);
        assert_eq!(deferred.changed_session.accumulated_seconds, 20);
        assert_eq!(deferred.changed_session.actual_seconds, Some(7));
        assert_eq!(deferred.changed_session.move_start, Some(second_move_start));
        assert_eq!(deferred.changed_session.move_end, Some(second_move_end));
    }

    #[tokio::test]
    async fn history_pages_have_exact_non_overlapping_continuations() {
        let (service, clock, item_id) = fixture().await;
        let first_id = Uuid::from_u128(41);
        service
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id: first_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 0,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(42),
                }),
                idempotency(7),
            )
            .await
            .unwrap();
        service
            .command(
                1,
                ExecutionCommand::Complete(FinishExecution {
                    session_id: first_id,
                    actual_seconds: None,
                }),
                idempotency(8),
            )
            .await
            .unwrap();
        clock.set(clock.now() + chrono::Duration::seconds(1));
        let second_id = Uuid::from_u128(43);
        service
            .command(
                2,
                ExecutionCommand::Start(StartExecution {
                    session_id: second_id,
                    item_id,
                    item_revision: 1,
                    occurrence_id: None,
                    session_index: 1,
                    planned_block_id: None,
                    device_id: Uuid::from_u128(42),
                }),
                idempotency(9),
            )
            .await
            .unwrap();

        let first = service.history_page(1, 0).await.unwrap();
        assert_eq!(first.sessions[0].id, second_id);
        assert_eq!(first.next_offset, Some(1));
        let second = service.history_page(1, 1).await.unwrap();
        assert_eq!(second.sessions[0].id, first_id);
        assert_eq!(second.next_offset, None);
    }
}
