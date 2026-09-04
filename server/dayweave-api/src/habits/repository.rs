use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    HabitDeltaChange, HabitDeltaPage, HabitMutation, HabitOccurrence, HabitOccurrenceEvidence,
    HabitOutcome, HabitOutcomeInput, HabitPause,
};

#[derive(Clone, Debug)]
pub struct HabitIdempotency {
    pub namespace: &'static str,
    pub key_hash: [u8; 32],
    pub request_fingerprint: [u8; 32],
    pub operation_id: Uuid,
    pub actor_session_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OccurrencePageCursor {
    pub local_date: NaiveDate,
    pub nominal_start: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Debug)]
pub struct OutcomeWrite {
    pub habit_id: Uuid,
    pub occurrence_id: Uuid,
    pub expected_revision: u64,
    pub outcome: HabitOutcomeInput,
    pub recorded_at: DateTime<Utc>,
    pub idempotency: HabitIdempotency,
}

#[derive(Clone, Debug)]
pub struct PauseCreate {
    pub id: Uuid,
    pub habit_id: Uuid,
    pub expected_revision: u64,
    pub started_at: DateTime<Utc>,
    pub preserves_streak: bool,
    pub recorded_at: DateTime<Utc>,
    pub idempotency: HabitIdempotency,
}

#[derive(Clone, Debug)]
pub struct PauseResume {
    pub id: Uuid,
    pub habit_id: Uuid,
    pub expected_revision: u64,
    pub ended_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub idempotency: HabitIdempotency,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum HabitRepositoryError {
    #[error("habit {0} was not found")]
    HabitNotFound(Uuid),
    #[error("item {0} is not an active habit")]
    NotHabit(Uuid),
    #[error("habit occurrence {0} has no authoritative published evidence")]
    OccurrenceNotFound(Uuid),
    #[error("habit pause {0} was not found")]
    PauseNotFound(Uuid),
    #[error("revision conflict: expected {expected}, found {actual}")]
    RevisionConflict {
        expected: u64,
        actual: u64,
        current_occurrence: Option<Box<HabitOccurrence>>,
        current_pause: Option<Box<HabitPause>>,
    },
    #[error("this habit already has an open pause")]
    OpenPauseConflict(Box<HabitPause>),
    #[error("the pause is already closed")]
    PauseAlreadyClosed(Box<HabitPause>),
    #[error("pause id is already bound to another operation")]
    PauseIdentityConflict(Box<HabitPause>),
    #[error("pause end must be later than its start")]
    InvalidPauseInterval,
    #[error("quantity unit does not match authoritative occurrence target")]
    TargetUnitMismatch,
    #[error("idempotency key or operation id was reused for different content")]
    IdempotencyConflict,
    #[error("habit cursor is invalid")]
    InvalidCursor,
    #[error("authoritative occurrence evidence conflicts with prior evidence")]
    EvidenceConflict,
    #[error("habit repository operation failed")]
    Internal,
}

#[async_trait]
pub trait HabitRepository: Send + Sync {
    fn cursor_scope(&self) -> Uuid;

    async fn replay_outcome(
        &self,
        _idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitOccurrence>, HabitRepositoryError> {
        Ok(None)
    }

    async fn replay_pause(
        &self,
        _idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitPause>, HabitRepositoryError> {
        Ok(None)
    }

    async fn put_outcome(
        &self,
        write: OutcomeWrite,
    ) -> Result<HabitMutation<HabitOccurrence>, HabitRepositoryError>;

    async fn create_pause(
        &self,
        create: PauseCreate,
    ) -> Result<HabitMutation<HabitPause>, HabitRepositoryError>;

    async fn resume_pause(
        &self,
        resume: PauseResume,
    ) -> Result<HabitMutation<HabitPause>, HabitRepositoryError>;

    async fn list_occurrences(
        &self,
        habit_id: Uuid,
        start_date: NaiveDate,
        end_date: NaiveDate,
        after: Option<OccurrencePageCursor>,
        limit: usize,
    ) -> Result<(Vec<HabitOccurrence>, bool), HabitRepositoryError>;

    async fn list_pauses(
        &self,
        habit_id: Uuid,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<HabitPause>, HabitRepositoryError>;

    async fn delta_head(&self) -> Result<u64, HabitRepositoryError>;

    async fn delta(&self, after: u64, limit: usize)
    -> Result<HabitDeltaPage, HabitRepositoryError>;
}

#[derive(Clone)]
pub struct InMemoryHabitRepository {
    scope: Uuid,
    state: Arc<Mutex<MemoryState>>,
}

impl std::fmt::Debug for InMemoryHabitRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InMemoryHabitRepository")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl Default for InMemoryHabitRepository {
    fn default() -> Self {
        Self {
            scope: Uuid::new_v4(),
            state: Arc::new(Mutex::new(MemoryState::default())),
        }
    }
}

#[derive(Default)]
struct MemoryState {
    occurrences: HashMap<Uuid, HabitOccurrence>,
    pauses: HashMap<Uuid, HabitPause>,
    receipts: HashMap<(String, [u8; 32]), MemoryReceipt>,
    operation_receipts: HashMap<Uuid, MemoryReceipt>,
    changes: Vec<(u64, HabitDeltaChange)>,
    next_sequence: u64,
}

#[derive(Clone)]
struct MemoryReceipt {
    namespace: String,
    key_hash: [u8; 32],
    fingerprint: [u8; 32],
    response: MemoryResponse,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Mirrors the exact wire receipt without extra projections.
enum MemoryResponse {
    Occurrence(HabitOccurrence),
    Pause(HabitPause),
}

impl InMemoryHabitRepository {
    /// Seeds evidence produced by a trusted schedule publisher. This is not an
    /// HTTP capability and is intentionally available only on the concrete test adapter.
    ///
    /// # Errors
    ///
    /// Returns an evidence conflict for a reused ledger/planner identity with
    /// different immutable evidence, or an internal error if delta admission fails.
    pub async fn insert_authoritative_occurrence(
        &self,
        evidence: HabitOccurrenceEvidence,
    ) -> Result<(), HabitRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(existing) = state.occurrences.get(&evidence.id) {
            if existing.evidence != evidence {
                return Err(HabitRepositoryError::EvidenceConflict);
            }
            return Ok(());
        }
        if state.occurrences.values().any(|existing| {
            existing.evidence.habit_id == evidence.habit_id
                && existing.evidence.planner_occurrence_id == evidence.planner_occurrence_id
                && existing.evidence != evidence
        }) {
            return Err(HabitRepositoryError::EvidenceConflict);
        }
        let occurrence = HabitOccurrence {
            evidence,
            outcome: None,
        };
        state
            .occurrences
            .insert(occurrence.evidence.id, occurrence.clone());
        append_change(
            &mut state,
            HabitDeltaChange::OccurrenceUpsert { occurrence },
        )?;
        Ok(())
    }
}

#[async_trait]
impl HabitRepository for InMemoryHabitRepository {
    fn cursor_scope(&self) -> Uuid {
        self.scope
    }

    async fn replay_outcome(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitOccurrence>, HabitRepositoryError> {
        let state = self.state.lock().await;
        replay(&state, idempotency)?.map_or(Ok(None), |response| match response {
            MemoryResponse::Occurrence(value) => Ok(Some(value)),
            MemoryResponse::Pause(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_pause(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitPause>, HabitRepositoryError> {
        let state = self.state.lock().await;
        replay(&state, idempotency)?.map_or(Ok(None), |response| match response {
            MemoryResponse::Pause(value) => Ok(Some(value)),
            MemoryResponse::Occurrence(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn put_outcome(
        &self,
        write: OutcomeWrite,
    ) -> Result<HabitMutation<HabitOccurrence>, HabitRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(response) = replay(&state, &write.idempotency)? {
            let MemoryResponse::Occurrence(value) = response else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        let occurrence = state.occurrences.get(&write.occurrence_id).cloned().ok_or(
            HabitRepositoryError::OccurrenceNotFound(write.occurrence_id),
        )?;
        if occurrence.evidence.habit_id != write.habit_id {
            return Err(HabitRepositoryError::OccurrenceNotFound(
                write.occurrence_id,
            ));
        }
        let actual = occurrence
            .outcome
            .as_ref()
            .map_or(0, |value| value.revision);
        if actual != write.expected_revision {
            return Err(HabitRepositoryError::RevisionConflict {
                expected: write.expected_revision,
                actual,
                current_occurrence: Some(Box::new(occurrence)),
                current_pause: None,
            });
        }
        if let (Some(expected), Some(actual)) = (
            occurrence.evidence.expected_unit.as_deref(),
            write.outcome.unit.as_deref(),
        ) && expected != actual
        {
            return Err(HabitRepositoryError::TargetUnitMismatch);
        }
        let revision = actual
            .checked_add(1)
            .ok_or(HabitRepositoryError::Internal)?;
        let mut updated = occurrence;
        updated.outcome = Some(HabitOutcome::from_input(
            write.outcome,
            revision,
            write.recorded_at,
        ));
        state
            .occurrences
            .insert(write.occurrence_id, updated.clone());
        append_change(
            &mut state,
            HabitDeltaChange::OccurrenceUpsert {
                occurrence: updated.clone(),
            },
        )?;
        store_receipt(
            &mut state,
            &write.idempotency,
            MemoryResponse::Occurrence(updated.clone()),
        )?;
        Ok(HabitMutation {
            value: updated,
            replayed: false,
        })
    }

    async fn create_pause(
        &self,
        create: PauseCreate,
    ) -> Result<HabitMutation<HabitPause>, HabitRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(response) = replay(&state, &create.idempotency)? {
            let MemoryResponse::Pause(value) = response else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        if let Some(existing) = state.pauses.get(&create.id).cloned() {
            return Err(HabitRepositoryError::PauseIdentityConflict(Box::new(
                existing,
            )));
        }
        if let Some(open) = state
            .pauses
            .values()
            .find(|pause| pause.habit_id == create.habit_id && pause.ended_at.is_none())
            .cloned()
        {
            return Err(HabitRepositoryError::OpenPauseConflict(Box::new(open)));
        }
        if let Some(overlap) = state
            .pauses
            .values()
            .find(|pause| {
                pause.habit_id == create.habit_id
                    && pause
                        .ended_at
                        .is_none_or(|ended_at| ended_at > create.started_at)
            })
            .cloned()
        {
            return Err(HabitRepositoryError::OpenPauseConflict(Box::new(overlap)));
        }
        if create.expected_revision != 0 {
            return Err(HabitRepositoryError::RevisionConflict {
                expected: create.expected_revision,
                actual: 0,
                current_occurrence: None,
                current_pause: None,
            });
        }
        let pause = HabitPause {
            id: create.id,
            habit_id: create.habit_id,
            revision: 1,
            started_at: create.started_at,
            ended_at: None,
            preserves_streak: create.preserves_streak,
            created_at: create.recorded_at,
            updated_at: create.recorded_at,
        };
        state.pauses.insert(pause.id, pause.clone());
        append_change(
            &mut state,
            HabitDeltaChange::PauseUpsert {
                pause: pause.clone(),
            },
        )?;
        store_receipt(
            &mut state,
            &create.idempotency,
            MemoryResponse::Pause(pause.clone()),
        )?;
        Ok(HabitMutation {
            value: pause,
            replayed: false,
        })
    }

    async fn resume_pause(
        &self,
        resume: PauseResume,
    ) -> Result<HabitMutation<HabitPause>, HabitRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(response) = replay(&state, &resume.idempotency)? {
            let MemoryResponse::Pause(value) = response else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        let current = state
            .pauses
            .get(&resume.id)
            .cloned()
            .ok_or(HabitRepositoryError::PauseNotFound(resume.id))?;
        if current.habit_id != resume.habit_id {
            return Err(HabitRepositoryError::PauseNotFound(resume.id));
        }
        if current.revision != resume.expected_revision {
            return Err(HabitRepositoryError::RevisionConflict {
                expected: resume.expected_revision,
                actual: current.revision,
                current_occurrence: None,
                current_pause: Some(Box::new(current)),
            });
        }
        if current.ended_at.is_some() {
            return Err(HabitRepositoryError::PauseAlreadyClosed(Box::new(current)));
        }
        if resume.ended_at <= current.started_at {
            return Err(HabitRepositoryError::InvalidPauseInterval);
        }
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(HabitRepositoryError::Internal)?;
        let mut updated = current;
        updated.revision = revision;
        updated.ended_at = Some(resume.ended_at);
        updated.updated_at = resume.recorded_at;
        state.pauses.insert(updated.id, updated.clone());
        append_change(
            &mut state,
            HabitDeltaChange::PauseUpsert {
                pause: updated.clone(),
            },
        )?;
        store_receipt(
            &mut state,
            &resume.idempotency,
            MemoryResponse::Pause(updated.clone()),
        )?;
        Ok(HabitMutation {
            value: updated,
            replayed: false,
        })
    }

    async fn list_occurrences(
        &self,
        habit_id: Uuid,
        start_date: NaiveDate,
        end_date: NaiveDate,
        after: Option<OccurrencePageCursor>,
        limit: usize,
    ) -> Result<(Vec<HabitOccurrence>, bool), HabitRepositoryError> {
        let state = self.state.lock().await;
        let mut values = state
            .occurrences
            .values()
            .filter(|value| {
                value.evidence.habit_id == habit_id
                    && value.evidence.local_date >= start_date
                    && value.evidence.local_date <= end_date
                    && after.is_none_or(|cursor| {
                        (
                            value.evidence.local_date,
                            value.evidence.nominal_start,
                            value.evidence.id,
                        ) > (cursor.local_date, cursor.nominal_start, cursor.id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        values.sort_by_key(|value| {
            (
                value.evidence.local_date,
                value.evidence.nominal_start,
                value.evidence.id,
            )
        });
        let has_more = values.len() > limit;
        values.truncate(limit);
        Ok((values, has_more))
    }

    async fn list_pauses(
        &self,
        habit_id: Uuid,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<HabitPause>, HabitRepositoryError> {
        let state = self.state.lock().await;
        let mut pauses = state
            .pauses
            .values()
            .filter(|pause| {
                pause.habit_id == habit_id
                    && pause.started_at < end
                    && pause.ended_at.is_none_or(|ended| ended > start)
            })
            .cloned()
            .collect::<Vec<_>>();
        pauses.sort_by_key(|pause| (pause.started_at, pause.id));
        Ok(pauses)
    }

    async fn delta_head(&self) -> Result<u64, HabitRepositoryError> {
        Ok(self.state.lock().await.next_sequence)
    }

    async fn delta(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<HabitDeltaPage, HabitRepositoryError> {
        let state = self.state.lock().await;
        if after > state.next_sequence {
            return Err(HabitRepositoryError::InvalidCursor);
        }
        let mut rows = state
            .changes
            .iter()
            .filter(|(sequence, _)| *sequence > after)
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let watermark = rows.last().map_or(after, |(sequence, _)| *sequence);
        Ok(HabitDeltaPage {
            changes: rows.into_iter().map(|(_, change)| change).collect(),
            watermark,
            has_more,
        })
    }
}

fn replay(
    state: &MemoryState,
    idempotency: &HabitIdempotency,
) -> Result<Option<MemoryResponse>, HabitRepositoryError> {
    let keyed = state
        .receipts
        .get(&(idempotency.namespace.to_owned(), idempotency.key_hash));
    let operated = state.operation_receipts.get(&idempotency.operation_id);
    for receipt in [keyed, operated].into_iter().flatten() {
        if receipt.namespace != idempotency.namespace
            || receipt.key_hash != idempotency.key_hash
            || receipt.fingerprint != idempotency.request_fingerprint
        {
            return Err(HabitRepositoryError::IdempotencyConflict);
        }
    }
    Ok(keyed.or(operated).map(|receipt| receipt.response.clone()))
}

fn store_receipt(
    state: &mut MemoryState,
    idempotency: &HabitIdempotency,
    response: MemoryResponse,
) -> Result<(), HabitRepositoryError> {
    let receipt = MemoryReceipt {
        namespace: idempotency.namespace.to_owned(),
        key_hash: idempotency.key_hash,
        fingerprint: idempotency.request_fingerprint,
        response,
    };
    if state
        .receipts
        .insert(
            (idempotency.namespace.to_owned(), idempotency.key_hash),
            receipt.clone(),
        )
        .is_some()
        || state
            .operation_receipts
            .insert(idempotency.operation_id, receipt)
            .is_some()
    {
        return Err(HabitRepositoryError::Internal);
    }
    Ok(())
}

fn append_change(
    state: &mut MemoryState,
    change: HabitDeltaChange,
) -> Result<(), HabitRepositoryError> {
    // Serialize here too so the memory adapter exercises the durable 64 KiB envelope.
    let payload = serde_json::to_vec(&change).map_err(|_| HabitRepositoryError::Internal)?;
    if payload.len() > 65_536 {
        return Err(HabitRepositoryError::Internal);
    }
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .ok_or(HabitRepositoryError::Internal)?;
    state.changes.push((state.next_sequence, change));
    Ok(())
}

#[allow(dead_code)]
fn privacy_shape_example(change: &HabitDeltaChange) -> serde_json::Value {
    // Kept close to the delta projection as an explicit reminder: SSE and audit
    // carry only a cursor/revision; authenticated delta is the sole sync path for notes.
    json!({ "change": change })
}
