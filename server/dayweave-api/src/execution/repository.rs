use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{ExecutionCommand, ExecutionDomainError, ExecutionSession};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct ExecutionSnapshot {
    pub revision: u64,
    pub active_session: Option<ExecutionSession>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct ExecutionMutation {
    pub revision: u64,
    pub active_session: Option<ExecutionSession>,
    pub changed_session: ExecutionSession,
    pub replayed: bool,
}

impl ExecutionMutation {
    #[must_use]
    pub fn snapshot(&self) -> ExecutionSnapshot {
        ExecutionSnapshot {
            revision: self.revision,
            active_session: self.active_session.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExecutionIdempotency {
    pub key_hash: [u8; 32],
    pub fingerprint: [u8; 32],
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ExecutionRepositoryError {
    #[error("execution snapshot revision conflict: expected {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("another execution session is already active")]
    ActiveSessionConflict,
    #[error("execution session {0} was not found")]
    SessionNotFound(Uuid),
    #[error("execution session {0} already exists")]
    DuplicateSession(Uuid),
    #[error("item revision changed before the execution command committed")]
    ItemRevisionConflict,
    #[error("item is not an executable active leaf")]
    ItemNotExecutable,
    #[error("idempotency key was used for different execution content")]
    IdempotencyConflict,
    #[error(transparent)]
    InvalidCommand(#[from] ExecutionDomainError),
    #[error("execution repository operation failed")]
    Internal,
}

#[async_trait]
pub trait ExecutionRepository: Send + Sync {
    async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionRepositoryError>;

    async fn replay(
        &self,
        now: DateTime<Utc>,
        idempotency: &ExecutionIdempotency,
    ) -> Result<Option<ExecutionMutation>, ExecutionRepositoryError>;

    async fn apply(
        &self,
        expected_revision: u64,
        command: ExecutionCommand,
        now: DateTime<Utc>,
        idempotency: ExecutionIdempotency,
    ) -> Result<ExecutionMutation, ExecutionRepositoryError>;

    async fn history(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ExecutionSession>, ExecutionRepositoryError>;
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryExecutionRepository {
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryState {
    revision: u64,
    active_session_id: Option<Uuid>,
    protocol_updated_at: Option<DateTime<Utc>>,
    sessions: HashMap<Uuid, ExecutionSession>,
    idempotency: HashMap<[u8; 32], MemoryIdempotency>,
}

#[derive(Clone, Debug)]
struct MemoryIdempotency {
    fingerprint: [u8; 32],
    expires_at: DateTime<Utc>,
    mutation: ExecutionMutation,
}

#[async_trait]
impl ExecutionRepository for InMemoryExecutionRepository {
    async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionRepositoryError> {
        let state = self.state.lock().await;
        Ok(snapshot(&state))
    }

    async fn replay(
        &self,
        now: DateTime<Utc>,
        idempotency: &ExecutionIdempotency,
    ) -> Result<Option<ExecutionMutation>, ExecutionRepositoryError> {
        let mut state = self.state.lock().await;
        state
            .idempotency
            .retain(|_, remembered| remembered.expires_at > now);
        let Some(remembered) = state.idempotency.get(&idempotency.key_hash) else {
            return Ok(None);
        };
        if remembered.fingerprint != idempotency.fingerprint {
            return Err(ExecutionRepositoryError::IdempotencyConflict);
        }
        Ok(Some(ExecutionMutation {
            replayed: true,
            ..remembered.mutation.clone()
        }))
    }

    async fn apply(
        &self,
        expected_revision: u64,
        command: ExecutionCommand,
        now: DateTime<Utc>,
        idempotency: ExecutionIdempotency,
    ) -> Result<ExecutionMutation, ExecutionRepositoryError> {
        let mut guard = self.state.lock().await;
        guard
            .idempotency
            .retain(|_, remembered| remembered.expires_at > now);
        if let Some(remembered) = guard.idempotency.get(&idempotency.key_hash) {
            if remembered.fingerprint != idempotency.fingerprint {
                return Err(ExecutionRepositoryError::IdempotencyConflict);
            }
            return Ok(ExecutionMutation {
                replayed: true,
                ..remembered.mutation.clone()
            });
        }
        if expected_revision != guard.revision {
            return Err(ExecutionRepositoryError::RevisionConflict {
                expected: expected_revision,
                actual: guard.revision,
            });
        }

        let transition_at = next_protocol_time(now, guard.protocol_updated_at)?;

        let mut next = guard.clone();
        let changed_session = if let ExecutionCommand::Start(input) = &command {
            if next.active_session_id.is_some() {
                return Err(ExecutionRepositoryError::ActiveSessionConflict);
            }
            if next.sessions.contains_key(&input.session_id) {
                return Err(ExecutionRepositoryError::DuplicateSession(input.session_id));
            }
            let session = ExecutionSession::start_with_protocol_time(input, transition_at, now);
            next.active_session_id = Some(session.id);
            next.sessions.insert(session.id, session.clone());
            session
        } else {
            let active_id = next
                .active_session_id
                .ok_or_else(|| ExecutionRepositoryError::SessionNotFound(command.session_id()))?;
            if active_id != command.session_id() {
                return Err(ExecutionRepositoryError::SessionNotFound(
                    command.session_id(),
                ));
            }
            let current = next
                .sessions
                .get(&active_id)
                .cloned()
                .ok_or(ExecutionRepositoryError::Internal)?;
            let updated = current.apply_with_protocol_time(&command, transition_at, now)?;
            if updated.status.is_open() {
                next.active_session_id = Some(updated.id);
            } else {
                next.active_session_id = None;
            }
            next.sessions.insert(updated.id, updated.clone());
            updated
        };
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(ExecutionRepositoryError::Internal)?;
        next.protocol_updated_at = Some(transition_at);
        let mutation = ExecutionMutation {
            revision: next.revision,
            active_session: next
                .active_session_id
                .and_then(|id| next.sessions.get(&id).cloned()),
            changed_session,
            replayed: false,
        };
        next.idempotency.insert(
            idempotency.key_hash,
            MemoryIdempotency {
                fingerprint: idempotency.fingerprint,
                expires_at: idempotency.expires_at,
                mutation: mutation.clone(),
            },
        );
        *guard = next;
        Ok(mutation)
    }

    async fn history(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ExecutionSession>, ExecutionRepositoryError> {
        let state = self.state.lock().await;
        let mut sessions: Vec<_> = state.sessions.values().cloned().collect();
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(sessions.into_iter().skip(offset).take(limit).collect())
    }
}

pub(crate) fn next_protocol_time(
    now: DateTime<Utc>,
    previous: Option<DateTime<Utc>>,
) -> Result<DateTime<Utc>, ExecutionRepositoryError> {
    let Some(previous) = previous else {
        return Ok(now);
    };
    if now > previous {
        Ok(now)
    } else {
        previous
            .checked_add_signed(chrono::Duration::microseconds(1))
            .ok_or(ExecutionRepositoryError::Internal)
    }
}

fn snapshot(state: &MemoryState) -> ExecutionSnapshot {
    ExecutionSnapshot {
        revision: state.revision,
        active_session: state
            .active_session_id
            .and_then(|id| state.sessions.get(&id).cloned()),
    }
}
