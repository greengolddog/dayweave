use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    HabitDeltaChange, HabitDeltaPage, HabitMissedCancellationReason, HabitMissedExplicitAction,
    HabitMissedPolicy, HabitMissedReconcileResult, HabitMissedResolution,
    HabitMissedResolutionAction, HabitMutation, HabitOccurrence, HabitOccurrenceEvidence,
    HabitOutcome, HabitOutcomeInput, HabitOutcomeStatus, HabitPause,
    derive_missed_resolution_action, recurrence_identity_ordinal,
    valid_explicit_missed_cancellation_transition, valid_missed_resolution_transition,
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

#[derive(Clone, Debug)]
pub struct HabitMissedConfiguration {
    pub item_revision: u64,
    pub policy_fingerprint: [u8; 32],
    pub policy: HabitMissedPolicy,
    pub is_active: bool,
}

#[derive(Clone, Debug)]
pub struct MissedReconcileWrite {
    pub policies: BTreeMap<Uuid, HabitMissedConfiguration>,
    pub limit: usize,
    pub recorded_at: DateTime<Utc>,
    pub idempotency: HabitIdempotency,
}

#[derive(Clone, Debug)]
pub struct MissedResolveWrite {
    pub habit_id: Uuid,
    pub occurrence_id: Uuid,
    pub expected_revision: u64,
    pub action: HabitMissedExplicitAction,
    pub current_item_revision: u64,
    pub current_policy_fingerprint: [u8; 32],
    pub current_item_is_active: bool,
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
    #[error("missed-occurrence resolution for {0} was not found")]
    MissedResolutionNotFound(Uuid),
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
    #[error("too many recent missed-reconcile receipts; retry later")]
    ReconcileReceiptCapacity,
    #[error("missed-occurrence decision has already been resolved")]
    MissedResolutionAlreadyResolved(Box<HabitMissedResolution>),
    #[error("no authoritative future occurrence is available for frequency reduction")]
    MissedReductionUnavailable,
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

    async fn replay_missed_reconcile(
        &self,
        _idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitMissedReconcileResult>, HabitRepositoryError> {
        Ok(None)
    }

    async fn replay_missed_resolution(
        &self,
        _idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitMissedResolution>, HabitRepositoryError> {
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

    async fn reconcile_missed(
        &self,
        write: MissedReconcileWrite,
    ) -> Result<HabitMutation<HabitMissedReconcileResult>, HabitRepositoryError>;

    async fn resolve_missed(
        &self,
        write: MissedResolveWrite,
    ) -> Result<HabitMutation<HabitMissedResolution>, HabitRepositoryError>;

    async fn list_occurrences(
        &self,
        habit_id: Uuid,
        start_date: NaiveDate,
        end_date: NaiveDate,
        after: Option<OccurrencePageCursor>,
        limit: usize,
    ) -> Result<(Vec<HabitOccurrence>, bool), HabitRepositoryError>;

    async fn effective_reduction_targets(
        &self,
        habit_id: Uuid,
        current_policy_fingerprint: [u8; 32],
        current_item_is_active: bool,
        planner_occurrence_ids: &[Uuid],
    ) -> Result<BTreeSet<Uuid>, HabitRepositoryError>;

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
    cancelled_effective_windows: HashMap<Uuid, (DateTime<Utc>, DateTime<Utc>)>,
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
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Mirrors the exact wire receipt without extra projections.
enum MemoryResponse {
    Occurrence(HabitOccurrence),
    Pause(HabitPause),
    MissedReconcile(HabitMissedReconcileResult),
    MissedResolution(HabitMissedResolution),
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
        evidence
            .validate()
            .map_err(|_| HabitRepositoryError::Internal)?;
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
            missed_resolution: None,
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
#[allow(clippy::too_many_lines)] // Atomic in-memory methods mirror the repository transaction boundaries.
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
            MemoryResponse::Pause(_)
            | MemoryResponse::MissedReconcile(_)
            | MemoryResponse::MissedResolution(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_pause(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitPause>, HabitRepositoryError> {
        let state = self.state.lock().await;
        replay(&state, idempotency)?.map_or(Ok(None), |response| match response {
            MemoryResponse::Pause(value) => Ok(Some(value)),
            MemoryResponse::Occurrence(_)
            | MemoryResponse::MissedReconcile(_)
            | MemoryResponse::MissedResolution(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_missed_reconcile(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitMissedReconcileResult>, HabitRepositoryError> {
        let state = self.state.lock().await;
        replay(&state, idempotency)?.map_or(Ok(None), |response| match response {
            MemoryResponse::MissedReconcile(value) => Ok(Some(value)),
            MemoryResponse::Occurrence(_)
            | MemoryResponse::Pause(_)
            | MemoryResponse::MissedResolution(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_missed_resolution(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitMissedResolution>, HabitRepositoryError> {
        let state = self.state.lock().await;
        replay(&state, idempotency)?.map_or(Ok(None), |response| match response {
            MemoryResponse::MissedResolution(value) => Ok(Some(value)),
            MemoryResponse::Occurrence(_)
            | MemoryResponse::Pause(_)
            | MemoryResponse::MissedReconcile(_) => Err(HabitRepositoryError::IdempotencyConflict),
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

    async fn reconcile_missed(
        &self,
        write: MissedReconcileWrite,
    ) -> Result<HabitMutation<HabitMissedReconcileResult>, HabitRepositoryError> {
        let mut state = self.state.lock().await;
        purge_expired_memory_receipts(&mut state, Utc::now());
        if let Some(response) = replay(&state, &write.idempotency)? {
            let MemoryResponse::MissedReconcile(value) = response else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        let mut resolutions = Vec::with_capacity(write.limit);
        let mut transitioned = std::collections::BTreeSet::new();
        let maintenance = round_robin_resolution_ids(&state, |occurrence| {
            write.policies.contains_key(&occurrence.evidence.habit_id)
        });
        for occurrence_id in maintenance {
            if resolutions.len() == write.limit {
                break;
            }
            let occurrence = state
                .occurrences
                .get(&occurrence_id)
                .cloned()
                .ok_or(HabitRepositoryError::Internal)?;
            let configuration = write
                .policies
                .get(&occurrence.evidence.habit_id)
                .ok_or(HabitRepositoryError::Internal)?;
            let Some(action) =
                memory_maintenance_action(&state, &occurrence, configuration, write.recorded_at)?
            else {
                continue;
            };
            resolutions.push(update_memory_missed_resolution(
                &mut state,
                occurrence,
                action,
                write.recorded_at,
                false,
            )?);
            transitioned.insert(occurrence_id);
        }

        // Rank against every historical resolution for each active habit, then
        // filter for pending work below. Ranking only currently pending rows
        // would promote a dense habit's next row back to rank one on every
        // bounded scan and could starve a sparse habit indefinitely.
        let pending = round_robin_resolution_ids(&state, |occurrence| {
            !transitioned.contains(&occurrence.evidence.id)
                && write
                    .policies
                    .get(&occurrence.evidence.habit_id)
                    .is_some_and(|configuration| configuration.is_active)
        });
        for occurrence_id in pending {
            if resolutions.len() == write.limit {
                break;
            }
            let occurrence = state
                .occurrences
                .get(&occurrence_id)
                .cloned()
                .ok_or(HabitRepositoryError::Internal)?;
            let configuration = write
                .policies
                .get(&occurrence.evidence.habit_id)
                .ok_or(HabitRepositoryError::Internal)?;
            if !occurrence
                .missed_resolution
                .as_ref()
                .is_some_and(|resolution| {
                    matches!(
                        resolution.action,
                        HabitMissedResolutionAction::ReductionPending
                    )
                })
            {
                continue;
            }
            if memory_source_cancellation_reason(&state, &occurrence, configuration).is_some() {
                continue;
            }
            let Ok(action) = memory_reduction_action(
                &state,
                &occurrence,
                configuration.policy_fingerprint,
                write.recorded_at,
            ) else {
                continue;
            };
            resolutions.push(update_memory_missed_resolution(
                &mut state,
                occurrence,
                action,
                write.recorded_at,
                false,
            )?);
            transitioned.insert(occurrence_id);
        }
        let remaining = write.limit.saturating_sub(resolutions.len());
        let mut effective_reduction_targets = std::collections::BTreeSet::new();
        for (habit_id, configuration) in &write.policies {
            if !configuration.is_active {
                continue;
            }
            effective_reduction_targets.extend(
                memory_effective_reduction_targets_for_habit(
                    &state,
                    *habit_id,
                    configuration.policy_fingerprint,
                )?
                .into_iter()
                .map(|target| (*habit_id, target)),
            );
        }
        let mut fresh_by_habit = BTreeMap::<Uuid, Vec<(DateTime<Utc>, Uuid)>>::new();
        for occurrence in state.occurrences.values().filter(|occurrence| {
            write
                .policies
                .get(&occurrence.evidence.habit_id)
                .is_some_and(|configuration| {
                    configuration.is_active
                        && memory_policy_fingerprint_matches(
                            &occurrence.evidence.policy_fingerprint,
                            configuration.policy_fingerprint,
                        )
                })
                && occurrence.evidence.window_end <= write.recorded_at
                && !effective_reduction_targets.contains(&(
                    occurrence.evidence.habit_id,
                    occurrence.evidence.planner_occurrence_id,
                ))
                && !matches!(
                    occurrence.outcome.as_ref().map(|outcome| outcome.status),
                    Some(HabitOutcomeStatus::Completed | HabitOutcomeStatus::Skipped)
                )
                && !state.pauses.values().any(|pause| {
                    pause.habit_id == occurrence.evidence.habit_id
                        && pause.started_at < occurrence.evidence.window_end
                        && pause
                            .ended_at
                            .is_none_or(|ended| ended > occurrence.evidence.window_start)
                })
        }) {
            fresh_by_habit
                .entry(occurrence.evidence.habit_id)
                .or_default()
                .push((occurrence.evidence.window_end, occurrence.evidence.id));
        }
        let mut candidates = Vec::new();
        for values in fresh_by_habit.values_mut() {
            values.sort_unstable();
            candidates.extend(
                values
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, id))| state.occurrences[id].missed_resolution.is_none())
                    .map(|(rank, (window_end, id))| (rank, *window_end, *id)),
            );
        }
        candidates.sort_unstable();
        let fresh_overflow = candidates.len() > remaining;
        for (_, _, occurrence_id) in candidates.into_iter().take(remaining) {
            let occurrence = state
                .occurrences
                .get(&occurrence_id)
                .cloned()
                .ok_or(HabitRepositoryError::Internal)?;
            let configuration = write
                .policies
                .get(&occurrence.evidence.habit_id)
                .ok_or(HabitRepositoryError::Internal)?;
            let action = memory_missed_action(
                &state,
                &occurrence,
                configuration.policy,
                configuration.policy_fingerprint,
                write.recorded_at,
            )?;
            let resolution = HabitMissedResolution {
                occurrence_evidence_id: occurrence.evidence.id,
                habit_id: occurrence.evidence.habit_id,
                source_planner_occurrence_id: occurrence.evidence.planner_occurrence_id,
                revision: 1,
                configured_policy: configuration.policy,
                action,
                created_at: write.recorded_at,
                updated_at: write.recorded_at,
            };
            resolution
                .validate()
                .map_err(|_| HabitRepositoryError::Internal)?;
            let mut updated = occurrence;
            updated.missed_resolution = Some(resolution.clone());
            state.occurrences.insert(occurrence_id, updated.clone());
            append_change(
                &mut state,
                HabitDeltaChange::OccurrenceUpsert {
                    occurrence: updated,
                },
            )?;
            resolutions.push(resolution);
        }
        let mut maintenance_has_more = false;
        let mut pending_has_more = false;
        for occurrence in state.occurrences.values() {
            let Some(configuration) = write.policies.get(&occurrence.evidence.habit_id) else {
                continue;
            };
            maintenance_has_more |=
                memory_maintenance_action(&state, occurrence, configuration, write.recorded_at)?
                    .is_some();
            pending_has_more |= occurrence
                .missed_resolution
                .as_ref()
                .is_some_and(|resolution| {
                    matches!(
                        resolution.action,
                        HabitMissedResolutionAction::ReductionPending
                    )
                })
                && memory_source_cancellation_reason(&state, occurrence, configuration).is_none()
                && memory_reduction_action(
                    &state,
                    occurrence,
                    configuration.policy_fingerprint,
                    write.recorded_at,
                )
                .is_ok();
        }
        let result = HabitMissedReconcileResult {
            resolutions,
            has_more: fresh_overflow || maintenance_has_more || pending_has_more,
        };
        if !result.resolutions.is_empty() || result.has_more {
            store_receipt(
                &mut state,
                &write.idempotency,
                MemoryResponse::MissedReconcile(result.clone()),
            )?;
        } else {
            store_ephemeral_reconcile_receipt(&mut state, &write.idempotency, result.clone())?;
        }
        Ok(HabitMutation {
            value: result,
            replayed: false,
        })
    }

    async fn resolve_missed(
        &self,
        write: MissedResolveWrite,
    ) -> Result<HabitMutation<HabitMissedResolution>, HabitRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(response) = replay(&state, &write.idempotency)? {
            let MemoryResponse::MissedResolution(value) = response else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        let occurrence = state
            .occurrences
            .get(&write.occurrence_id)
            .cloned()
            .filter(|occurrence| occurrence.evidence.habit_id == write.habit_id)
            .ok_or(HabitRepositoryError::MissedResolutionNotFound(
                write.occurrence_id,
            ))?;
        let current = occurrence.missed_resolution.clone().ok_or(
            HabitRepositoryError::MissedResolutionNotFound(write.occurrence_id),
        )?;
        if current.revision != write.expected_revision {
            return Err(HabitRepositoryError::RevisionConflict {
                expected: write.expected_revision,
                actual: current.revision,
                current_occurrence: Some(Box::new(occurrence)),
                current_pause: None,
            });
        }
        if !matches!(
            current.action,
            HabitMissedResolutionAction::DecisionRequired
        ) {
            return Err(HabitRepositoryError::MissedResolutionAlreadyResolved(
                Box::new(current),
            ));
        }
        let configuration = HabitMissedConfiguration {
            item_revision: write.current_item_revision,
            policy_fingerprint: write.current_policy_fingerprint,
            policy: HabitMissedPolicy::Ask,
            is_active: write.current_item_is_active,
        };
        let cancellation_reason = if write.current_item_is_active {
            memory_source_cancellation_reason(&state, &occurrence, &configuration)
        } else {
            Some(HabitMissedCancellationReason::SourceObsolete)
        };
        let action = if let Some(reason) = cancellation_reason {
            HabitMissedResolutionAction::Cancelled {
                reason,
                resume_action: match write.action {
                    HabitMissedExplicitAction::Skip => super::HabitMissedResumeAction::Skip,
                    HabitMissedExplicitAction::Carry => super::HabitMissedResumeAction::Carry,
                    HabitMissedExplicitAction::ReduceFrequency => {
                        super::HabitMissedResumeAction::ReduceFrequency
                    }
                },
            }
        } else {
            memory_explicit_missed_action(
                &state,
                &occurrence,
                write.action,
                write.current_policy_fingerprint,
                write.recorded_at,
            )?
        };
        let resolution = update_memory_missed_resolution(
            &mut state,
            occurrence,
            action,
            write.recorded_at,
            true,
        )?;
        store_receipt(
            &mut state,
            &write.idempotency,
            MemoryResponse::MissedResolution(resolution.clone()),
        )?;
        Ok(HabitMutation {
            value: resolution,
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

    async fn effective_reduction_targets(
        &self,
        habit_id: Uuid,
        current_policy_fingerprint: [u8; 32],
        current_item_is_active: bool,
        planner_occurrence_ids: &[Uuid],
    ) -> Result<BTreeSet<Uuid>, HabitRepositoryError> {
        if !current_item_is_active || planner_occurrence_ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let state = self.state.lock().await;
        let candidates = planner_occurrence_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut effective = memory_effective_reduction_targets_for_habit(
            &state,
            habit_id,
            current_policy_fingerprint,
        )?;
        effective.retain(|planner_occurrence_id| candidates.contains(planner_occurrence_id));
        Ok(effective)
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

fn memory_missed_action(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    policy: HabitMissedPolicy,
    policy_fingerprint: [u8; 32],
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let action = derive_missed_resolution_action(occurrence, policy, now)
        .map_err(|_| HabitRepositoryError::Internal)?;
    if matches!(action, HabitMissedResolutionAction::ReductionPending) {
        match memory_reduction_action(state, occurrence, policy_fingerprint, now) {
            Ok(bound) => Ok(bound),
            Err(HabitRepositoryError::MissedReductionUnavailable) => Ok(action),
            Err(error) => Err(error),
        }
    } else {
        Ok(action)
    }
}

fn memory_explicit_missed_action(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    action: HabitMissedExplicitAction,
    policy_fingerprint: [u8; 32],
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let policy = match action {
        HabitMissedExplicitAction::Skip => HabitMissedPolicy::Skip,
        HabitMissedExplicitAction::Carry => HabitMissedPolicy::Carry,
        HabitMissedExplicitAction::ReduceFrequency => HabitMissedPolicy::ReduceFrequency,
    };
    let derived = derive_missed_resolution_action(occurrence, policy, now)
        .map_err(|_| HabitRepositoryError::Internal)?;
    if matches!(derived, HabitMissedResolutionAction::ReductionPending) {
        match memory_reduction_action(state, occurrence, policy_fingerprint, now) {
            Ok(action) => Ok(action),
            Err(HabitRepositoryError::MissedReductionUnavailable) => {
                Ok(HabitMissedResolutionAction::ReductionPending)
            }
            Err(error) => Err(error),
        }
    } else {
        Ok(derived)
    }
}

fn memory_reduction_action(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    policy_fingerprint: [u8; 32],
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let source_ordinal = recurrence_identity_ordinal(&occurrence.evidence.identity)
        .ok_or(HabitRepositoryError::Internal)?;
    let already_suppressed = memory_effective_reduction_targets_for_habit(
        state,
        occurrence.evidence.habit_id,
        policy_fingerprint,
    )?;
    let (target, _) = state
        .occurrences
        .values()
        .map(|candidate| {
            recurrence_identity_ordinal(&candidate.evidence.identity)
                .map(|ordinal| (candidate, ordinal))
                .ok_or(HabitRepositoryError::Internal)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(candidate, candidate_ordinal)| {
            candidate.evidence.habit_id == occurrence.evidence.habit_id
                && (
                    candidate.evidence.nominal_start,
                    *candidate_ordinal,
                    candidate.evidence.planner_occurrence_id,
                ) > (
                    occurrence.evidence.nominal_start,
                    source_ordinal,
                    occurrence.evidence.planner_occurrence_id,
                )
        })
        .min_by_key(|(candidate, candidate_ordinal)| {
            (
                candidate.evidence.nominal_start,
                *candidate_ordinal,
                candidate.evidence.planner_occurrence_id,
            )
        })
        .ok_or(HabitRepositoryError::MissedReductionUnavailable)?;
    if !memory_policy_fingerprint_matches(&target.evidence.policy_fingerprint, policy_fingerprint)
        || target.evidence.window_end <= now
        || already_suppressed.contains(&target.evidence.planner_occurrence_id)
        || state.pauses.values().any(|pause| {
            pause.habit_id == target.evidence.habit_id
                && pause.started_at < target.evidence.window_end
                && pause
                    .ended_at
                    .is_none_or(|ended| ended > target.evidence.window_start)
        })
        || target
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.status != HabitOutcomeStatus::Unresolved)
    {
        return Err(HabitRepositoryError::MissedReductionUnavailable);
    }
    let target = target.evidence.planner_occurrence_id;
    if state.occurrences.values().any(|source| {
        matches!(
            source
                .missed_resolution
                .as_ref()
                .map(|resolution| &resolution.action),
            Some(HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids,
            }) if suppressed_planner_occurrence_ids.contains(&target)
        )
    }) {
        // An inactive/chained projection may retain the database-equivalent
        // one-target reservation so it can become effective again after a
        // correction. Do not skip an extra later occurrence or create a
        // duplicate claim; remain pending until maintenance changes the graph.
        return Err(HabitRepositoryError::MissedReductionUnavailable);
    }
    Ok(HabitMissedResolutionAction::ReduceFrequency {
        suppressed_planner_occurrence_ids: vec![target],
    })
}

fn memory_source_cancellation_reason(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
) -> Option<HabitMissedCancellationReason> {
    if !configuration.is_active {
        return Some(HabitMissedCancellationReason::SourceObsolete);
    }
    match occurrence.outcome.as_ref().map(|outcome| outcome.status) {
        Some(HabitOutcomeStatus::Completed) => {
            return Some(HabitMissedCancellationReason::SourceCompleted);
        }
        Some(HabitOutcomeStatus::Skipped) => {
            return Some(HabitMissedCancellationReason::SourceSkipped);
        }
        _ => {}
    }
    let (window_start, window_end) = match occurrence
        .missed_resolution
        .as_ref()
        .map(|resolution| &resolution.action)
    {
        Some(HabitMissedResolutionAction::Carry {
            window_start,
            window_end,
        }) => (*window_start, *window_end),
        Some(HabitMissedResolutionAction::Cancelled {
            resume_action: super::HabitMissedResumeAction::Carry,
            ..
        }) => state
            .cancelled_effective_windows
            .get(&occurrence.evidence.id)
            .copied()
            .unwrap_or((
                occurrence.evidence.window_start,
                occurrence.evidence.window_end,
            )),
        _ => (
            occurrence.evidence.window_start,
            occurrence.evidence.window_end,
        ),
    };
    if state.pauses.values().any(|pause| {
        pause.habit_id == occurrence.evidence.habit_id
            && pause.started_at < window_end
            && pause.ended_at.is_none_or(|ended| ended > window_start)
    }) {
        return Some(HabitMissedCancellationReason::SourcePaused);
    }
    if !memory_policy_fingerprint_matches(
        &occurrence.evidence.policy_fingerprint,
        configuration.policy_fingerprint,
    ) {
        return Some(HabitMissedCancellationReason::SourceObsolete);
    }
    None
}

const fn memory_resume_action(
    action: &HabitMissedResolutionAction,
) -> Option<super::HabitMissedResumeAction> {
    match action {
        HabitMissedResolutionAction::DecisionRequired => {
            Some(super::HabitMissedResumeAction::DecisionRequired)
        }
        HabitMissedResolutionAction::Skip => Some(super::HabitMissedResumeAction::Skip),
        HabitMissedResolutionAction::Carry { .. } => Some(super::HabitMissedResumeAction::Carry),
        HabitMissedResolutionAction::ReductionPending
        | HabitMissedResolutionAction::ReduceFrequency { .. } => {
            Some(super::HabitMissedResumeAction::ReduceFrequency)
        }
        HabitMissedResolutionAction::Cancelled { .. } => None,
    }
}

fn memory_bound_reduction_is_eligible(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
) -> bool {
    memory_bound_reduction_target_is_eligible(state, occurrence, configuration.policy_fingerprint)
}

fn memory_bound_reduction_target_is_eligible(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    policy_fingerprint: [u8; 32],
) -> bool {
    let Some(HabitMissedResolutionAction::ReduceFrequency {
        suppressed_planner_occurrence_ids,
    }) = occurrence
        .missed_resolution
        .as_ref()
        .map(|resolution| &resolution.action)
    else {
        return true;
    };
    let Some(target_id) = suppressed_planner_occurrence_ids.first() else {
        return false;
    };
    state.occurrences.values().any(|target| {
        target.evidence.habit_id == occurrence.evidence.habit_id
            && target.evidence.planner_occurrence_id == *target_id
            && memory_policy_fingerprint_matches(
                &target.evidence.policy_fingerprint,
                policy_fingerprint,
            )
            && target
                .outcome
                .as_ref()
                .is_none_or(|outcome| outcome.status == HabitOutcomeStatus::Unresolved)
            && !state.pauses.values().any(|pause| {
                pause.habit_id == target.evidence.habit_id
                    && pause.started_at < target.evidence.window_end
                    && pause
                        .ended_at
                        .is_none_or(|ended| ended > target.evidence.window_start)
            })
    })
}

fn memory_effective_reduction_targets_for_habit(
    state: &MemoryState,
    habit_id: Uuid,
    policy_fingerprint: [u8; 32],
) -> Result<std::collections::BTreeSet<Uuid>, HabitRepositoryError> {
    let mut sources = state
        .occurrences
        .values()
        .filter(|occurrence| {
            occurrence.evidence.habit_id == habit_id
                && matches!(
                    occurrence
                        .missed_resolution
                        .as_ref()
                        .map(|resolution| &resolution.action),
                    Some(HabitMissedResolutionAction::ReduceFrequency { .. })
                )
        })
        .map(|occurrence| {
            recurrence_identity_ordinal(&occurrence.evidence.identity)
                .map(|ordinal| (occurrence, ordinal))
                .ok_or(HabitRepositoryError::Internal)
        })
        .collect::<Result<Vec<_>, _>>()?;
    sources.sort_unstable_by_key(|(occurrence, ordinal)| {
        (
            occurrence.evidence.nominal_start,
            *ordinal,
            occurrence.evidence.planner_occurrence_id,
        )
    });

    let mut targets = std::collections::BTreeSet::new();
    for (source, _) in sources {
        if targets.contains(&source.evidence.planner_occurrence_id)
            || !memory_policy_fingerprint_matches(
                &source.evidence.policy_fingerprint,
                policy_fingerprint,
            )
            || matches!(
                source.outcome.as_ref().map(|outcome| outcome.status),
                Some(HabitOutcomeStatus::Completed | HabitOutcomeStatus::Skipped)
            )
            || state.pauses.values().any(|pause| {
                pause.habit_id == source.evidence.habit_id
                    && pause.started_at < source.evidence.window_end
                    && pause
                        .ended_at
                        .is_none_or(|ended| ended > source.evidence.window_start)
            })
            || !memory_bound_reduction_target_is_eligible(state, source, policy_fingerprint)
        {
            continue;
        }
        let Some(HabitMissedResolutionAction::ReduceFrequency {
            suppressed_planner_occurrence_ids,
        }) = source
            .missed_resolution
            .as_ref()
            .map(|resolution| &resolution.action)
        else {
            continue;
        };
        targets.extend(suppressed_planner_occurrence_ids.iter().copied());
    }
    Ok(targets)
}

fn memory_restore_action(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
    resume_action: super::HabitMissedResumeAction,
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let policy = match resume_action {
        super::HabitMissedResumeAction::DecisionRequired => HabitMissedPolicy::Ask,
        super::HabitMissedResumeAction::Skip => HabitMissedPolicy::Skip,
        super::HabitMissedResumeAction::Carry => HabitMissedPolicy::Carry,
        super::HabitMissedResumeAction::ReduceFrequency => HabitMissedPolicy::ReduceFrequency,
    };
    memory_missed_action(
        state,
        occurrence,
        policy,
        configuration.policy_fingerprint,
        now,
    )
}

fn memory_maintenance_action(
    state: &MemoryState,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
    now: DateTime<Utc>,
) -> Result<Option<HabitMissedResolutionAction>, HabitRepositoryError> {
    let Some(current) = occurrence.missed_resolution.as_ref() else {
        return Ok(None);
    };
    if let Some(reason) = memory_source_cancellation_reason(state, occurrence, configuration) {
        if matches!(
            current.action,
            HabitMissedResolutionAction::Cancelled { .. }
        ) {
            return Ok(None);
        }
        let resume_action =
            memory_resume_action(&current.action).ok_or(HabitRepositoryError::Internal)?;
        return Ok(Some(HabitMissedResolutionAction::Cancelled {
            reason,
            resume_action,
        }));
    }
    match &current.action {
        HabitMissedResolutionAction::Cancelled {
            reason:
                HabitMissedCancellationReason::SourceCompleted
                | HabitMissedCancellationReason::SourceSkipped
                | HabitMissedCancellationReason::SourcePaused
                | HabitMissedCancellationReason::SourceObsolete,
            resume_action,
        } => memory_restore_action(state, occurrence, configuration, *resume_action, now).map(Some),
        HabitMissedResolutionAction::ReduceFrequency { .. }
            if !memory_bound_reduction_is_eligible(state, occurrence, configuration) =>
        {
            Ok(Some(HabitMissedResolutionAction::ReductionPending))
        }
        HabitMissedResolutionAction::Carry { window_end, .. } if *window_end <= now => {
            if current.configured_policy == HabitMissedPolicy::Ask {
                Ok(Some(HabitMissedResolutionAction::DecisionRequired))
            } else {
                derive_missed_resolution_action(occurrence, HabitMissedPolicy::Carry, now)
                    .map(Some)
                    .map_err(|_| HabitRepositoryError::Internal)
            }
        }
        _ => Ok(None),
    }
}

fn memory_policy_fingerprint_matches(value: &str, expected: [u8; 32]) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut rendered = [0_u8; 71];
    rendered[..7].copy_from_slice(b"sha256:");
    for (index, byte) in expected.into_iter().enumerate() {
        rendered[7 + index * 2] = HEX[usize::from(byte >> 4)];
        rendered[8 + index * 2] = HEX[usize::from(byte & 0x0f)];
    }
    value.as_bytes() == rendered
}

fn update_memory_missed_resolution(
    state: &mut MemoryState,
    mut occurrence: HabitOccurrence,
    action: HabitMissedResolutionAction,
    updated_at: DateTime<Utc>,
    explicit_selection: bool,
) -> Result<HabitMissedResolution, HabitRepositoryError> {
    let current = occurrence
        .missed_resolution
        .clone()
        .ok_or(HabitRepositoryError::Internal)?;
    let resolution = HabitMissedResolution {
        revision: current
            .revision
            .checked_add(1)
            .ok_or(HabitRepositoryError::Internal)?,
        action,
        updated_at,
        ..current.clone()
    };
    if !(valid_missed_resolution_transition(&current, &resolution)
        || explicit_selection
            && valid_explicit_missed_cancellation_transition(&current, &resolution))
    {
        return Err(HabitRepositoryError::Internal);
    }
    match (&current.action, &resolution.action) {
        (
            HabitMissedResolutionAction::Carry {
                window_start,
                window_end,
            },
            HabitMissedResolutionAction::Cancelled { .. },
        ) => {
            state
                .cancelled_effective_windows
                .insert(occurrence.evidence.id, (*window_start, *window_end));
        }
        (_, HabitMissedResolutionAction::Cancelled { .. }) => {}
        _ => {
            state
                .cancelled_effective_windows
                .remove(&occurrence.evidence.id);
        }
    }
    occurrence.missed_resolution = Some(resolution.clone());
    state
        .occurrences
        .insert(occurrence.evidence.id, occurrence.clone());
    append_change(state, HabitDeltaChange::OccurrenceUpsert { occurrence })?;
    Ok(resolution)
}

fn round_robin_resolution_ids(
    state: &MemoryState,
    mut include: impl FnMut(&HabitOccurrence) -> bool,
) -> Vec<Uuid> {
    let mut by_habit = BTreeMap::<Uuid, Vec<(DateTime<Utc>, Uuid)>>::new();
    for occurrence in state.occurrences.values().filter(|value| include(value)) {
        let Some(resolution) = occurrence.missed_resolution.as_ref() else {
            continue;
        };
        by_habit
            .entry(occurrence.evidence.habit_id)
            .or_default()
            // The creation coordinate is immutable. Using `updated_at` would
            // collapse a dense habit's next pending row back to rank zero
            // whenever the previous row transitions.
            .push((resolution.created_at, occurrence.evidence.id));
    }
    for values in by_habit.values_mut() {
        values.sort_unstable();
    }
    let max_rank = by_habit.values().map(Vec::len).max().unwrap_or(0);
    let mut ordered = Vec::new();
    for rank in 0..max_rank {
        let mut at_rank = by_habit
            .values()
            .filter_map(|values| values.get(rank).copied())
            .collect::<Vec<_>>();
        at_rank.sort_unstable();
        ordered.extend(at_rank.into_iter().map(|(_, id)| id));
    }
    ordered
}

fn replay(
    state: &MemoryState,
    idempotency: &HabitIdempotency,
) -> Result<Option<MemoryResponse>, HabitRepositoryError> {
    let keyed = state
        .receipts
        .get(&(idempotency.namespace.to_owned(), idempotency.key_hash))
        .filter(|receipt| receipt.expires_at.is_none_or(|expiry| expiry > Utc::now()));
    let operated = state
        .operation_receipts
        .get(&idempotency.operation_id)
        .filter(|receipt| receipt.expires_at.is_none_or(|expiry| expiry > Utc::now()));
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
        created_at: Utc::now(),
        expires_at: None,
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

fn store_ephemeral_reconcile_receipt(
    state: &mut MemoryState,
    idempotency: &HabitIdempotency,
    response: HabitMissedReconcileResult,
) -> Result<(), HabitRepositoryError> {
    purge_expired_memory_receipts(state, Utc::now());
    let ephemeral_count = state
        .receipts
        .values()
        .filter(|receipt| receipt.expires_at.is_some())
        .count();
    if ephemeral_count >= 4_096 {
        let eviction_cutoff = Utc::now() - chrono::Duration::hours(12);
        let oldest = state
            .receipts
            .iter()
            .filter(|(_, receipt)| {
                receipt.expires_at.is_some() && receipt.created_at <= eviction_cutoff
            })
            .filter_map(|(key, receipt)| receipt.expires_at.map(|expiry| (expiry, key.clone())))
            .min();
        if let Some((_, key)) = oldest {
            state.receipts.remove(&key);
            state
                .operation_receipts
                .retain(|_, receipt| !(receipt.namespace == key.0 && receipt.key_hash == key.1));
        } else {
            return Err(HabitRepositoryError::ReconcileReceiptCapacity);
        }
    }
    let receipt = MemoryReceipt {
        namespace: idempotency.namespace.to_owned(),
        key_hash: idempotency.key_hash,
        fingerprint: idempotency.request_fingerprint,
        response: MemoryResponse::MissedReconcile(response),
        created_at: Utc::now(),
        expires_at: Some(Utc::now() + chrono::Duration::hours(24)),
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

fn purge_expired_memory_receipts(state: &mut MemoryState, now: DateTime<Utc>) {
    state
        .receipts
        .retain(|_, receipt| receipt.expires_at.is_none_or(|expiry| expiry > now));
    state
        .operation_receipts
        .retain(|_, receipt| receipt.expires_at.is_none_or(|expiry| expiry > now));
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
