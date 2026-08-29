use std::{sync::Arc, time::Duration as StdDuration};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    items::{ItemService, ItemServiceError, ItemStatus},
    proposals::Clock,
};

use super::{
    ExecutionCommand, ExecutionDomainError, ExecutionIdempotency, ExecutionMutation,
    ExecutionRepository, ExecutionRepositoryError, ExecutionSession, ExecutionSnapshot,
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
        }
    }

    /// Returns the canonical cross-device execution lease.
    ///
    /// # Errors
    ///
    /// Returns a redacted repository error when state cannot be loaded.
    pub async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionServiceError> {
        Ok(self.repository.snapshot().await?)
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
        let now = self.clock.now();
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
            return Ok(mutation);
        }
        command.validate(now)?;

        if let ExecutionCommand::Start(input) = &command {
            let item = self.items.get(input.item_id).await?;
            if item.revision != input.item_revision {
                return Err(ExecutionServiceError::ItemRevisionConflict {
                    expected: input.item_revision,
                    actual: item.revision,
                });
            }
            if !item.is_executable
                || matches!(
                    item.status,
                    ItemStatus::Completed | ItemStatus::Skipped | ItemStatus::Cancelled
                )
            {
                return Err(ExecutionServiceError::ItemNotExecutable);
            }
        }

        Ok(self
            .repository
            .apply(expected_revision, command, now, idempotency)
            .await?)
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
        items::{IdempotencyKey, InMemoryItemRepository, ItemKind, NewItem, SplitPolicy},
        proposals::Clock,
    };

    use super::*;
    use crate::execution::{
        FinishExecution, InMemoryExecutionRepository, PauseExecution, StartExecution,
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
                    kind: ItemKind::Task,
                    status: ItemStatus::Planned,
                    title: "Write".to_owned(),
                    notes: None,
                    timezone_name: "UTC".to_owned(),
                    duration_seconds: Some(1_800),
                    deadline_at: None,
                    earliest_start_at: None,
                    recurrence: None,
                    flexible_constraints: serde_json::json!({}),
                    split_policy: SplitPolicy::Indivisible,
                    importance: 50,
                    urgency: 50,
                    parent_id: None,
                    sibling_order: 0,
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
