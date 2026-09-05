use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use chrono::{DateTime, Datelike as _, NaiveDate, Offset as _, Utc};
use dayweave_core::{
    ItemId, Minutes, Occurrence, OccurrenceId, OccurrenceState, RecurrenceContext,
    RecurrenceException, RecurrenceExceptionAction, RecurrenceExceptionSelector,
    RecurrenceMoveSource, RecurrenceOccurrenceIdentity, RecurrencePartialProgress, RecurrencePause,
    is_valid_habit_quantity_unit,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    habits::{
        HabitDeltaChange, HabitDeltaPage, HabitIdempotency, HabitMissedCancellationReason,
        HabitMissedConfiguration, HabitMissedExplicitAction, HabitMissedPolicy,
        HabitMissedReconcileResult, HabitMissedResolution, HabitMissedResolutionAction,
        HabitMissedResumeAction, HabitMutation, HabitOccurrence, HabitOccurrenceEvidence,
        HabitOutcome, HabitOutcomeStatus, HabitPause, HabitRepository, HabitRepositoryError,
        MAX_HABIT_QUANTITY, MissedReconcileWrite, MissedResolveWrite, OccurrencePageCursor,
        OutcomeWrite, PauseCreate, PauseResume, derive_missed_resolution_action,
        recurrence_identity_ordinal, valid_explicit_missed_cancellation_transition,
        valid_missed_resolution_transition,
    },
    scheduling::ComposeScheduleResult,
};

use super::{DatabaseScope, lock_canonical_item_space};

const MAX_AUTHORITATIVE_MOVED_OCCURRENCES: usize = 10_000;
const MAX_RECURRENCE_IDENTITY_BYTES: usize = 4_096;
const MAX_STORED_RECONCILE_RESOLUTIONS: usize = 200;
const MISSED_RECONCILE_NAMESPACE: &str = "habits.missed.reconcile";
const MISSED_RECONCILE_EPHEMERAL_RESOURCE: &str = "habit_missed_reconcile_receipt";

#[derive(Clone)]
pub struct PostgresHabitRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl std::fmt::Debug for PostgresHabitRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresHabitRepository")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl PostgresHabitRepository {
    #[must_use]
    pub const fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Permanent receipts retain the exact authenticated response.
enum StoredReceipt {
    Occurrence(HabitOccurrence),
    Pause(HabitPause),
    MissedReconcile(HabitMissedReconcileResult),
    MissedResolution(HabitMissedResolution),
}

impl StoredReceipt {
    fn validate(&self) -> Result<(), HabitRepositoryError> {
        match self {
            Self::Occurrence(occurrence) => validate_occurrence(occurrence)?,
            Self::MissedReconcile(result) => {
                if result.resolutions.len() > MAX_STORED_RECONCILE_RESOLUTIONS {
                    return Err(HabitRepositoryError::Internal);
                }
                let mut evidence_ids = BTreeSet::new();
                for resolution in &result.resolutions {
                    validate_resolution(resolution)?;
                    if !evidence_ids.insert(resolution.occurrence_evidence_id) {
                        return Err(HabitRepositoryError::Internal);
                    }
                }
            }
            Self::MissedResolution(resolution) => validate_resolution(resolution)?,
            Self::Pause(_) => {}
        }
        Ok(())
    }
}

fn validate_change(change: &HabitDeltaChange) -> Result<(), HabitRepositoryError> {
    if let HabitDeltaChange::OccurrenceUpsert { occurrence } = change {
        validate_occurrence(occurrence)?;
    }
    Ok(())
}

fn validate_occurrence(occurrence: &HabitOccurrence) -> Result<(), HabitRepositoryError> {
    occurrence
        .evidence
        .validate()
        .map_err(|_| HabitRepositoryError::Internal)?;
    if let Some(resolution) = &occurrence.missed_resolution {
        validate_resolution(resolution)?;
        if resolution.occurrence_evidence_id != occurrence.evidence.id
            || resolution.habit_id != occurrence.evidence.habit_id
            || resolution.source_planner_occurrence_id != occurrence.evidence.planner_occurrence_id
            || matches!(
                &resolution.action,
                HabitMissedResolutionAction::ReduceFrequency {
                    suppressed_planner_occurrence_ids,
                } if suppressed_planner_occurrence_ids.contains(&occurrence.evidence.planner_occurrence_id)
            )
        {
            return Err(HabitRepositoryError::Internal);
        }
    }
    Ok(())
}

fn validate_resolution(resolution: &HabitMissedResolution) -> Result<(), HabitRepositoryError> {
    resolution
        .validate()
        .map_err(|_| HabitRepositoryError::Internal)
}

#[async_trait]
#[allow(clippy::too_many_lines)] // Each mutation intentionally keeps its atomic audit/version/change transaction visible.
impl HabitRepository for PostgresHabitRepository {
    fn cursor_scope(&self) -> Uuid {
        self.scope.workspace_id
    }

    async fn replay_outcome(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitOccurrence>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let receipt = replay_receipt(&mut tx, self.scope, idempotency).await?;
        tx.commit().await.map_err(storage)?;
        receipt.map_or(Ok(None), |receipt| match receipt {
            StoredReceipt::Occurrence(value) => Ok(Some(value)),
            StoredReceipt::Pause(_)
            | StoredReceipt::MissedReconcile(_)
            | StoredReceipt::MissedResolution(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_pause(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitPause>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let receipt = replay_receipt(&mut tx, self.scope, idempotency).await?;
        tx.commit().await.map_err(storage)?;
        receipt.map_or(Ok(None), |receipt| match receipt {
            StoredReceipt::Pause(value) => Ok(Some(value)),
            StoredReceipt::Occurrence(_)
            | StoredReceipt::MissedReconcile(_)
            | StoredReceipt::MissedResolution(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_missed_reconcile(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitMissedReconcileResult>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let receipt = replay_receipt(&mut tx, self.scope, idempotency).await?;
        tx.commit().await.map_err(storage)?;
        receipt.map_or(Ok(None), |receipt| match receipt {
            StoredReceipt::MissedReconcile(value) => Ok(Some(value)),
            StoredReceipt::Occurrence(_)
            | StoredReceipt::Pause(_)
            | StoredReceipt::MissedResolution(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn replay_missed_resolution(
        &self,
        idempotency: &HabitIdempotency,
    ) -> Result<Option<HabitMissedResolution>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let receipt = replay_receipt(&mut tx, self.scope, idempotency).await?;
        tx.commit().await.map_err(storage)?;
        receipt.map_or(Ok(None), |receipt| match receipt {
            StoredReceipt::MissedResolution(value) => Ok(Some(value)),
            StoredReceipt::Occurrence(_)
            | StoredReceipt::Pause(_)
            | StoredReceipt::MissedReconcile(_) => Err(HabitRepositoryError::IdempotencyConflict),
        })
    }

    async fn put_outcome(
        &self,
        write: OutcomeWrite,
    ) -> Result<HabitMutation<HabitOccurrence>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_habit_mutation_space(&mut tx, self.scope.workspace_id).await?;
        if let Some(receipt) = replay_receipt(&mut tx, self.scope, &write.idempotency).await? {
            let StoredReceipt::Occurrence(value) = receipt else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            tx.commit().await.map_err(storage)?;
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        require_active_habit(&mut tx, self.scope.workspace_id, write.habit_id).await?;
        let row = occurrence_row_for_update(
            &mut tx,
            self.scope.workspace_id,
            write.habit_id,
            write.occurrence_id,
        )
        .await?
        .ok_or(HabitRepositoryError::OccurrenceNotFound(
            write.occurrence_id,
        ))?;
        let current = occurrence_from_row(&row)?;
        let actual_revision = current.outcome.as_ref().map_or(0, |value| value.revision);
        if actual_revision != write.expected_revision {
            return Err(HabitRepositoryError::RevisionConflict {
                expected: write.expected_revision,
                actual: actual_revision,
                current_occurrence: Some(Box::new(current)),
                current_pause: None,
            });
        }
        if let (Some(expected), Some(actual)) = (
            current.evidence.expected_unit.as_deref(),
            write.outcome.unit.as_deref(),
        ) && expected != actual
        {
            return Err(HabitRepositoryError::TargetUnitMismatch);
        }
        let revision = actual_revision
            .checked_add(1)
            .ok_or(HabitRepositoryError::Internal)?;
        let outcome = HabitOutcome::from_input(write.outcome, revision, write.recorded_at);
        let previous_snapshot = current
            .outcome
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| HabitRepositoryError::Internal)?;
        let outcome_snapshot =
            serde_json::to_value(&outcome).map_err(|_| HabitRepositoryError::Internal)?;
        sqlx::query(
            "INSERT INTO habit_occurrence_outcomes (workspace_id, occurrence_evidence_id, revision, \
             status, progress_basis_points, quantity, unit, actual_seconds, note, occurred_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (workspace_id, occurrence_evidence_id) DO UPDATE SET \
               revision = EXCLUDED.revision, status = EXCLUDED.status, \
               progress_basis_points = EXCLUDED.progress_basis_points, quantity = EXCLUDED.quantity, \
               unit = EXCLUDED.unit, actual_seconds = EXCLUDED.actual_seconds, note = EXCLUDED.note, \
               occurred_at = EXCLUDED.occurred_at, updated_at = EXCLUDED.updated_at \
             WHERE habit_occurrence_outcomes.revision = $12",
        )
        .bind(self.scope.workspace_id)
        .bind(write.occurrence_id)
        .bind(to_i64(revision)?)
        .bind(outcome_status_name(outcome.status))
        .bind(i32::from(outcome.progress_basis_points))
        .bind(outcome.quantity)
        .bind(&outcome.unit)
        .bind(outcome.actual_seconds.map(to_i64).transpose()?)
        .bind(&outcome.note)
        .bind(outcome.occurred_at)
        .bind(outcome.updated_at)
        .bind(to_i64(actual_revision)?)
        .execute(&mut *tx)
        .await
        .map_err(storage)
        .and_then(|result| {
            if result.rows_affected() == 1 {
                Ok(())
            } else {
                Err(HabitRepositoryError::Internal)
            }
        })?;

        let version_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO habit_occurrence_versions (id, workspace_id, occurrence_evidence_id, \
             revision, operation_id, previous_snapshot, outcome_snapshot, occurred_at, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(version_id)
        .bind(self.scope.workspace_id)
        .bind(write.occurrence_id)
        .bind(to_i64(revision)?)
        .bind(write.idempotency.operation_id)
        .bind(previous_snapshot)
        .bind(outcome_snapshot)
        .bind(outcome.occurred_at)
        .bind(write.recorded_at)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;

        let updated = HabitOccurrence {
            evidence: current.evidence,
            outcome: Some(outcome),
            missed_resolution: current.missed_resolution,
        };
        let change = HabitDeltaChange::OccurrenceUpsert {
            occurrence: updated.clone(),
        };
        let sequence = insert_change(
            &mut tx,
            self.scope.workspace_id,
            "occurrence_upsert",
            write.occurrence_id,
            revision,
            &change,
            write.recorded_at,
        )
        .await?;
        insert_audit(
            &mut tx,
            self.scope,
            write.idempotency.actor_session_id,
            write.idempotency.operation_id,
            "habit.occurrence.outcome",
            "habit_occurrence",
            write.occurrence_id,
            actual_revision,
            revision,
            json!({
                "version_id": version_id,
                "previous_status": current.outcome.as_ref().map(|value| outcome_status_name(value.status)),
                "result_status": outcome_status_name(updated.outcome.as_ref().expect("set above").status),
                "change_sequence": sequence,
            }),
            write.recorded_at,
        )
        .await?;
        insert_content_free_outbox(
            &mut tx,
            self.scope.workspace_id,
            write.occurrence_id,
            revision,
            "habit.occurrence.changed",
            sequence,
            write.recorded_at,
        )
        .await?;
        let receipt = StoredReceipt::Occurrence(updated.clone());
        insert_receipt(
            &mut tx,
            self.scope,
            &write.idempotency,
            &receipt,
            write.recorded_at,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(HabitMutation {
            value: updated,
            replayed: false,
        })
    }

    async fn create_pause(
        &self,
        create: PauseCreate,
    ) -> Result<HabitMutation<HabitPause>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_habit_mutation_space(&mut tx, self.scope.workspace_id).await?;
        if let Some(receipt) = replay_receipt(&mut tx, self.scope, &create.idempotency).await? {
            let StoredReceipt::Pause(value) = receipt else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            tx.commit().await.map_err(storage)?;
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        let constraints =
            require_active_habit(&mut tx, self.scope.workspace_id, create.habit_id).await?;
        let authoritative_preserves = constraints
            .get("preserves_streak_when_paused")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if authoritative_preserves != create.preserves_streak {
            return Err(HabitRepositoryError::EvidenceConflict);
        }
        if create.expected_revision != 0 {
            return Err(HabitRepositoryError::RevisionConflict {
                expected: create.expected_revision,
                actual: 0,
                current_occurrence: None,
                current_pause: None,
            });
        }
        if let Some(row) = sqlx::query(
            "SELECT id, habit_id, revision, started_at, ended_at, preserves_streak, created_at, \
             updated_at FROM habit_pauses WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(create.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        {
            return Err(HabitRepositoryError::PauseIdentityConflict(Box::new(
                pause_from_row(&row)?,
            )));
        }
        if let Some(row) = sqlx::query(
            "SELECT id, habit_id, revision, started_at, ended_at, preserves_streak, created_at, \
             updated_at FROM habit_pauses WHERE workspace_id = $1 AND habit_id = $2 \
             AND ended_at IS NULL FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(create.habit_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        {
            return Err(HabitRepositoryError::OpenPauseConflict(Box::new(
                pause_from_row(&row)?,
            )));
        }
        let overlaps = sqlx::query(
            "SELECT id, habit_id, revision, started_at, ended_at, preserves_streak, created_at, \
             updated_at FROM habit_pauses WHERE workspace_id = $1 AND habit_id = $2 \
             AND (ended_at IS NULL OR ended_at > $3) ORDER BY started_at LIMIT 1 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(create.habit_id)
        .bind(create.started_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        if let Some(row) = overlaps {
            return Err(HabitRepositoryError::OpenPauseConflict(Box::new(
                pause_from_row(&row)?,
            )));
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
        sqlx::query(
            "INSERT INTO habit_pauses (id, workspace_id, habit_id, revision, started_at, ended_at, \
             preserves_streak, created_at, updated_at) VALUES ($1, $2, $3, 1, $4, NULL, $5, $6, $6)",
        )
        .bind(pause.id)
        .bind(self.scope.workspace_id)
        .bind(pause.habit_id)
        .bind(pause.started_at)
        .bind(pause.preserves_streak)
        .bind(pause.created_at)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let snapshot = serde_json::to_value(&pause).map_err(|_| HabitRepositoryError::Internal)?;
        let version_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO habit_pause_versions (id, workspace_id, pause_id, revision, operation_id, \
             previous_snapshot, pause_snapshot, recorded_at) VALUES ($1, $2, $3, 1, $4, NULL, $5, $6)",
        )
        .bind(version_id)
        .bind(self.scope.workspace_id)
        .bind(pause.id)
        .bind(create.idempotency.operation_id)
        .bind(snapshot)
        .bind(create.recorded_at)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let change = HabitDeltaChange::PauseUpsert {
            pause: pause.clone(),
        };
        let sequence = insert_change(
            &mut tx,
            self.scope.workspace_id,
            "pause_upsert",
            pause.id,
            1,
            &change,
            create.recorded_at,
        )
        .await?;
        insert_audit(
            &mut tx,
            self.scope,
            create.idempotency.actor_session_id,
            create.idempotency.operation_id,
            "habit.pause.started",
            "habit_pause",
            pause.id,
            0,
            1,
            json!({"version_id":version_id,"change_sequence":sequence,"preserves_streak":pause.preserves_streak}),
            create.recorded_at,
        )
        .await?;
        insert_content_free_outbox(
            &mut tx,
            self.scope.workspace_id,
            pause.id,
            1,
            "habit.pause.changed",
            sequence,
            create.recorded_at,
        )
        .await?;
        let receipt = StoredReceipt::Pause(pause.clone());
        insert_receipt(
            &mut tx,
            self.scope,
            &create.idempotency,
            &receipt,
            create.recorded_at,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(HabitMutation {
            value: pause,
            replayed: false,
        })
    }

    async fn resume_pause(
        &self,
        resume: PauseResume,
    ) -> Result<HabitMutation<HabitPause>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_habit_mutation_space(&mut tx, self.scope.workspace_id).await?;
        if let Some(receipt) = replay_receipt(&mut tx, self.scope, &resume.idempotency).await? {
            let StoredReceipt::Pause(value) = receipt else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            tx.commit().await.map_err(storage)?;
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        require_active_habit(&mut tx, self.scope.workspace_id, resume.habit_id).await?;
        let row = sqlx::query(
            "SELECT id, habit_id, revision, started_at, ended_at, preserves_streak, created_at, \
             updated_at FROM habit_pauses WHERE workspace_id = $1 AND habit_id = $2 AND id = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(resume.habit_id)
        .bind(resume.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(HabitRepositoryError::PauseNotFound(resume.id))?;
        let current = pause_from_row(&row)?;
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
        let mut pause = current.clone();
        pause.revision = revision;
        pause.ended_at = Some(resume.ended_at);
        pause.updated_at = resume.recorded_at;
        let updated = sqlx::query(
            "UPDATE habit_pauses SET revision = $4, ended_at = $5, updated_at = $6 \
             WHERE workspace_id = $1 AND habit_id = $2 AND id = $3 AND revision = $7",
        )
        .bind(self.scope.workspace_id)
        .bind(resume.habit_id)
        .bind(resume.id)
        .bind(to_i64(revision)?)
        .bind(resume.ended_at)
        .bind(resume.recorded_at)
        .bind(to_i64(current.revision)?)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        if updated.rows_affected() != 1 {
            return Err(HabitRepositoryError::Internal);
        }
        let previous_snapshot =
            serde_json::to_value(&current).map_err(|_| HabitRepositoryError::Internal)?;
        let snapshot = serde_json::to_value(&pause).map_err(|_| HabitRepositoryError::Internal)?;
        let version_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO habit_pause_versions (id, workspace_id, pause_id, revision, operation_id, \
             previous_snapshot, pause_snapshot, recorded_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(version_id)
        .bind(self.scope.workspace_id)
        .bind(pause.id)
        .bind(to_i64(revision)?)
        .bind(resume.idempotency.operation_id)
        .bind(previous_snapshot)
        .bind(snapshot)
        .bind(resume.recorded_at)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let change = HabitDeltaChange::PauseUpsert {
            pause: pause.clone(),
        };
        let sequence = insert_change(
            &mut tx,
            self.scope.workspace_id,
            "pause_upsert",
            pause.id,
            revision,
            &change,
            resume.recorded_at,
        )
        .await?;
        insert_audit(
            &mut tx,
            self.scope,
            resume.idempotency.actor_session_id,
            resume.idempotency.operation_id,
            "habit.pause.resumed",
            "habit_pause",
            pause.id,
            current.revision,
            revision,
            json!({"version_id":version_id,"change_sequence":sequence}),
            resume.recorded_at,
        )
        .await?;
        insert_content_free_outbox(
            &mut tx,
            self.scope.workspace_id,
            pause.id,
            revision,
            "habit.pause.changed",
            sequence,
            resume.recorded_at,
        )
        .await?;
        let receipt = StoredReceipt::Pause(pause.clone());
        insert_receipt(
            &mut tx,
            self.scope,
            &resume.idempotency,
            &receipt,
            resume.recorded_at,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(HabitMutation {
            value: pause,
            replayed: false,
        })
    }

    async fn reconcile_missed(
        &self,
        write: MissedReconcileWrite,
    ) -> Result<HabitMutation<HabitMissedReconcileResult>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_habit_mutation_space(&mut tx, self.scope.workspace_id).await?;
        if let Some(receipt) = replay_receipt(&mut tx, self.scope, &write.idempotency).await? {
            let StoredReceipt::MissedReconcile(value) = receipt else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            tx.commit().await.map_err(storage)?;
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        if write.policies.is_empty() {
            let result = HabitMissedReconcileResult {
                resolutions: Vec::new(),
                has_more: false,
            };
            insert_ephemeral_reconcile_receipt(&mut tx, self.scope, &write.idempotency, &result)
                .await?;
            tx.commit().await.map_err(storage)?;
            return Ok(HabitMutation {
                value: result,
                replayed: false,
            });
        }
        let habit_ids = write.policies.keys().copied().collect::<Vec<_>>();
        let item_revisions = write
            .policies
            .values()
            .map(|configuration| to_i64(configuration.item_revision))
            .collect::<Result<Vec<_>, _>>()?;
        let policy_fingerprints = write
            .policies
            .values()
            .map(|configuration| configuration.policy_fingerprint.to_vec())
            .collect::<Vec<_>>();
        let active_items = write
            .policies
            .values()
            .map(|configuration| configuration.is_active)
            .collect::<Vec<_>>();
        let mut resolutions = Vec::with_capacity(write.limit);
        let mut transitioned = Vec::new();
        let mut deferred_pending_work = false;

        // First sweep actions whose applicability changed after a correction,
        // pause, recurrence edit, carried-window expiry, or reduction-target
        // change. The SQL predicate is deliberately the same proof used by the
        // transition helper, so bounded pages cannot be consumed by no-ops.
        let maintenance_capacity = write.limit.saturating_sub(resolutions.len());
        let maintenance_limit = i64::try_from(maintenance_capacity.saturating_add(1))
            .map_err(|_| HabitRepositoryError::Internal)?;
        let maintenance_sql = format!(
            "WITH configuration AS MATERIALIZED ( \
               SELECT * FROM UNNEST($2::uuid[], $3::bigint[], $4::bytea[], $5::boolean[]) \
                 AS configured(habit_id, item_revision, policy_fingerprint, is_active) \
             ), ranked AS MATERIALIZED ( \
               SELECT evidence.id AS candidate_id, \
                 ROW_NUMBER() OVER (PARTITION BY evidence.habit_id \
                   ORDER BY resolution.created_at, evidence.id) AS candidate_rank \
               FROM habit_occurrence_evidence evidence \
               JOIN habit_missed_resolutions resolution \
                 ON resolution.workspace_id = evidence.workspace_id \
                AND resolution.occurrence_evidence_id = evidence.id \
               JOIN configuration configured ON configured.habit_id = evidence.habit_id \
               WHERE evidence.workspace_id = $1 \
             ), candidates AS MATERIALIZED ( \
               SELECT evidence.id AS candidate_id, resolution.updated_at AS candidate_updated_at, \
                 ranked.candidate_rank \
               FROM habit_occurrence_evidence evidence \
               JOIN habit_missed_resolutions resolution \
                 ON resolution.workspace_id = evidence.workspace_id \
                AND resolution.occurrence_evidence_id = evidence.id \
               JOIN ranked ON ranked.candidate_id = evidence.id \
               JOIN configuration configured ON configured.habit_id = evidence.habit_id \
               JOIN items item ON item.workspace_id = evidence.workspace_id \
                 AND item.id = evidence.habit_id AND item.revision = configured.item_revision \
               LEFT JOIN habit_occurrence_outcomes outcome \
                 ON outcome.workspace_id = evidence.workspace_id \
                AND outcome.occurrence_evidence_id = evidence.id \
               LEFT JOIN LATERAL ( \
                 SELECT CASE WHEN version.previous_snapshot #>> '{{action,type}}' = 'carry' \
                   THEN (version.previous_snapshot #>> '{{action,window_start}}')::timestamptz END \
                     AS window_start, \
                   CASE WHEN version.previous_snapshot #>> '{{action,type}}' = 'carry' \
                   THEN (version.previous_snapshot #>> '{{action,window_end}}')::timestamptz END \
                     AS window_end \
                 FROM habit_missed_resolution_versions version \
                 WHERE version.workspace_id = resolution.workspace_id \
                   AND version.occurrence_evidence_id = resolution.occurrence_evidence_id \
                   AND version.revision = resolution.revision \
               ) cancelled_prior ON resolution.action = 'cancelled' \
               WHERE evidence.workspace_id = $1 AND item.kind = 'habit' \
                 AND ( \
                   (resolution.action <> 'cancelled' AND ( \
                     NOT configured.is_active \
                     OR item.trashed_at IS NOT NULL \
                     OR item.status IN ('completed', 'skipped', 'cancelled', 'blocked') \
                     OR outcome.status IN ('completed', 'skipped') \
                     OR evidence.policy_fingerprint <> configured.policy_fingerprint \
                     OR EXISTS (SELECT 1 FROM habit_pauses source_pause \
                       WHERE source_pause.workspace_id = evidence.workspace_id \
                         AND source_pause.habit_id = evidence.habit_id \
                         AND source_pause.started_at < CASE WHEN resolution.action = 'carry' \
                           THEN resolution.carry_window_end ELSE evidence.window_end END \
                         AND (source_pause.ended_at IS NULL OR source_pause.ended_at > \
                           CASE WHEN resolution.action = 'carry' \
                             THEN resolution.carry_window_start ELSE evidence.window_start END)) \
                     OR (resolution.action = 'carry' AND resolution.carry_window_end <= $6) \
                     OR (resolution.action = 'reduce_frequency' AND EXISTS ( \
                       SELECT 1 FROM habit_occurrence_evidence target \
                       LEFT JOIN habit_occurrence_outcomes target_outcome \
                         ON target_outcome.workspace_id = target.workspace_id \
                        AND target_outcome.occurrence_evidence_id = target.id \
                       WHERE target.workspace_id = evidence.workspace_id \
                         AND target.habit_id = evidence.habit_id \
                         AND target.planner_occurrence_id = resolution.suppressed_planner_occurrence_id \
                         AND ( \
                           (target_outcome.status IS NOT NULL AND target_outcome.status <> 'unresolved') \
                           OR EXISTS (SELECT 1 FROM habit_pauses target_pause \
                             WHERE target_pause.workspace_id = target.workspace_id \
                               AND target_pause.habit_id = target.habit_id \
                               AND target_pause.started_at < target.window_end \
                               AND (target_pause.ended_at IS NULL \
                                 OR target_pause.ended_at > target.window_start)) \
                           OR EXISTS (SELECT 1 FROM schedule_revisions current_revision \
                             WHERE current_revision.workspace_id = target.workspace_id \
                               AND current_revision.state = 'published' \
                               AND current_revision.horizon_start <= target.window_start \
                               AND current_revision.horizon_end >= target.window_end \
                               AND NOT EXISTS (SELECT 1 FROM habit_occurrence_publications publication \
                                 WHERE publication.workspace_id = target.workspace_id \
                                   AND publication.schedule_revision_id = current_revision.id \
                                   AND publication.occurrence_evidence_id = target.id \
                                   AND publication.occurrence_state IN ('generated', 'skipped') \
                                   AND target.policy_fingerprint = configured.policy_fingerprint)) \
                         ))) \
                   )) \
                   OR (resolution.action = 'cancelled' \
                     AND configured.is_active \
                     AND item.trashed_at IS NULL \
                     AND item.status NOT IN ('completed', 'skipped', 'cancelled', 'blocked') \
                     AND outcome.status IS DISTINCT FROM 'completed' \
                     AND outcome.status IS DISTINCT FROM 'skipped' \
                     AND evidence.policy_fingerprint = configured.policy_fingerprint \
                     AND NOT EXISTS (SELECT 1 FROM habit_pauses source_pause \
                       WHERE source_pause.workspace_id = evidence.workspace_id \
                         AND source_pause.habit_id = evidence.habit_id \
                         AND source_pause.started_at < \
                           COALESCE(cancelled_prior.window_end, evidence.window_end) \
                         AND (source_pause.ended_at IS NULL \
                           OR source_pause.ended_at > \
                             COALESCE(cancelled_prior.window_start, evidence.window_start)))) \
                 ) \
             ), selected AS MATERIALIZED ( \
               SELECT candidate_id, candidate_updated_at, candidate_rank FROM candidates \
               ORDER BY candidate_rank, candidate_updated_at, candidate_id LIMIT $7 \
             ) \
             SELECT {EVIDENCE_COLUMNS}, selected.candidate_rank, selected.candidate_updated_at{OCCURRENCE_OUTCOME_SELECT} \
             JOIN selected ON selected.candidate_id = evidence.id \
             WHERE evidence.workspace_id = $1 \
             ORDER BY selected.candidate_rank, selected.candidate_updated_at, evidence.id \
             FOR UPDATE OF evidence"
        );
        let maintenance_rows = sqlx::query(AssertSqlSafe(maintenance_sql))
            .bind(self.scope.workspace_id)
            .bind(&habit_ids)
            .bind(&item_revisions)
            .bind(&policy_fingerprints)
            .bind(&active_items)
            .bind(write.recorded_at)
            .bind(maintenance_limit)
            .fetch_all(&mut *tx)
            .await
            .map_err(storage)?;
        let maintenance_overflow = maintenance_rows.len() > maintenance_capacity;
        for row in maintenance_rows.iter().take(maintenance_capacity) {
            let occurrence = occurrence_from_row(row)?;
            let occurrence_id = occurrence.evidence.id;
            let configuration = write
                .policies
                .get(&occurrence.evidence.habit_id)
                .ok_or(HabitRepositoryError::EvidenceConflict)?;
            let current = occurrence
                .missed_resolution
                .clone()
                .ok_or(HabitRepositoryError::Internal)?;
            let Some(action) = maintenance_action_tx(
                &mut tx,
                self.scope.workspace_id,
                &occurrence,
                configuration,
                write.recorded_at,
            )
            .await?
            else {
                return Err(HabitRepositoryError::EvidenceConflict);
            };
            if matches!(action, HabitMissedResolutionAction::ReductionPending) {
                match reduction_action_tx(
                    &mut tx,
                    self.scope.workspace_id,
                    &occurrence,
                    configuration,
                    write.recorded_at,
                )
                .await
                {
                    Ok(_) => deferred_pending_work = true,
                    Err(HabitRepositoryError::MissedReductionUnavailable) => {}
                    Err(error) => return Err(error),
                }
            }
            let resolution = HabitMissedResolution {
                revision: current
                    .revision
                    .checked_add(1)
                    .ok_or(HabitRepositoryError::Internal)?,
                action,
                updated_at: write.recorded_at,
                ..current.clone()
            };
            let child_operation_id = Uuid::new_v5(
                &write.idempotency.operation_id,
                occurrence.evidence.id.as_bytes(),
            );
            update_missed_resolution_tx(
                &mut tx,
                self.scope,
                write.idempotency.actor_session_id,
                child_operation_id,
                occurrence,
                &current,
                &resolution,
                false,
                write.recorded_at,
            )
            .await?;
            resolutions.push(resolution);
            transitioned.push(occurrence_id);
        }

        // Bind durable reduction-pending rows only when the exact target is a
        // generated occurrence in the unique current publication. Ranking by
        // per-habit position prevents one dense habit from starving others.
        let pending_capacity = write.limit.saturating_sub(resolutions.len());
        let pending_limit = i64::try_from(pending_capacity.saturating_add(1))
            .map_err(|_| HabitRepositoryError::Internal)?;
        let pending_rows = sqlx::query(AssertSqlSafe(format!(
            "WITH configuration AS MATERIALIZED ( \
               SELECT * FROM UNNEST($2::uuid[], $3::bigint[], $4::bytea[], $5::boolean[]) \
                 AS configured(habit_id, item_revision, policy_fingerprint, is_active) \
             ), ranked AS MATERIALIZED ( \
               SELECT evidence.id AS candidate_id, \
                 ROW_NUMBER() OVER (PARTITION BY evidence.habit_id \
                   ORDER BY resolution.created_at, evidence.id) AS candidate_rank \
               FROM habit_occurrence_evidence evidence \
               JOIN habit_missed_resolutions resolution \
                 ON resolution.workspace_id = evidence.workspace_id \
                AND resolution.occurrence_evidence_id = evidence.id \
               JOIN configuration configured ON configured.habit_id = evidence.habit_id \
               WHERE evidence.workspace_id = $1 \
             ), candidates AS MATERIALIZED ( \
               SELECT evidence.id AS candidate_id, resolution.created_at AS candidate_created_at, \
                 ranked.candidate_rank \
               FROM habit_occurrence_evidence evidence \
               JOIN habit_missed_resolutions resolution \
                 ON resolution.workspace_id = evidence.workspace_id \
                AND resolution.occurrence_evidence_id = evidence.id \
               JOIN ranked ON ranked.candidate_id = evidence.id \
               JOIN configuration configured ON configured.habit_id = evidence.habit_id \
               JOIN items item ON item.workspace_id = evidence.workspace_id \
                 AND item.id = evidence.habit_id AND item.revision = configured.item_revision \
               LEFT JOIN habit_occurrence_outcomes outcome \
                 ON outcome.workspace_id = evidence.workspace_id \
                AND outcome.occurrence_evidence_id = evidence.id \
               WHERE evidence.workspace_id = $1 AND configured.is_active \
                 AND item.kind = 'habit' AND item.trashed_at IS NULL \
                 AND item.status NOT IN ('completed', 'skipped', 'cancelled', 'blocked') \
                 AND resolution.action = 'reduction_pending' \
                 AND evidence.policy_fingerprint = configured.policy_fingerprint \
                 AND (outcome.status IS NULL OR outcome.status IN ('unresolved', 'partial')) \
                 AND NOT (evidence.id = ANY($7::uuid[])) \
                 AND NOT EXISTS (SELECT 1 FROM habit_pauses source_pause \
                   WHERE source_pause.workspace_id = evidence.workspace_id \
                     AND source_pause.habit_id = evidence.habit_id \
                     AND source_pause.started_at < evidence.window_end \
                     AND (source_pause.ended_at IS NULL \
                       OR source_pause.ended_at > evidence.window_start)) \
                 AND EXISTS (SELECT 1 FROM habit_available_reduction_target( \
                   evidence.workspace_id, evidence.habit_id, evidence.id, \
                   evidence.nominal_start, evidence.recurrence_ordinal, evidence.planner_occurrence_id, \
                   configured.policy_fingerprint, $6)) \
             ), selected AS MATERIALIZED ( \
               SELECT candidate_id, candidate_created_at, candidate_rank FROM candidates \
               ORDER BY candidate_rank, candidate_created_at, candidate_id LIMIT $8 \
             ) \
             SELECT {EVIDENCE_COLUMNS}, selected.candidate_rank, selected.candidate_created_at{OCCURRENCE_OUTCOME_SELECT} \
             JOIN selected ON selected.candidate_id = evidence.id \
             WHERE evidence.workspace_id = $1 \
             ORDER BY selected.candidate_rank, selected.candidate_created_at, evidence.id \
             FOR UPDATE OF evidence"
        )))
        .bind(self.scope.workspace_id)
        .bind(&habit_ids)
        .bind(&item_revisions)
        .bind(&policy_fingerprints)
        .bind(&active_items)
        .bind(write.recorded_at)
        .bind(&transitioned)
        .bind(pending_limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        let pending_overflow = pending_rows.len() > pending_capacity;
        for row in pending_rows.iter().take(pending_capacity) {
            let occurrence = occurrence_from_row(row)?;
            let configuration = write
                .policies
                .get(&occurrence.evidence.habit_id)
                .ok_or(HabitRepositoryError::EvidenceConflict)?;
            let current = occurrence
                .missed_resolution
                .clone()
                .ok_or(HabitRepositoryError::Internal)?;
            let action = match reduction_action_tx(
                &mut tx,
                self.scope.workspace_id,
                &occurrence,
                configuration,
                write.recorded_at,
            )
            .await
            {
                Ok(action) => action,
                Err(HabitRepositoryError::MissedReductionUnavailable) => {
                    deferred_pending_work = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let resolution = HabitMissedResolution {
                revision: current
                    .revision
                    .checked_add(1)
                    .ok_or(HabitRepositoryError::Internal)?,
                action,
                updated_at: write.recorded_at,
                ..current.clone()
            };
            let child_operation_id = Uuid::new_v5(
                &write.idempotency.operation_id,
                occurrence.evidence.id.as_bytes(),
            );
            update_missed_resolution_tx(
                &mut tx,
                self.scope,
                write.idempotency.actor_session_id,
                child_operation_id,
                occurrence,
                &current,
                &resolution,
                false,
                write.recorded_at,
            )
            .await?;
            resolutions.push(resolution);
            transitioned.push(current.occurrence_evidence_id);
        }

        // Finally admit never-before-reconciled overdue evidence. Immutable
        // evidence must carry the exact current policy fingerprint; obsolete
        // history is not silently assigned a new policy after a recurrence edit.
        let fresh_capacity = write.limit.saturating_sub(resolutions.len());
        let fresh_limit = i64::try_from(fresh_capacity.saturating_add(1))
            .map_err(|_| HabitRepositoryError::Internal)?;
        let fresh_rows = sqlx::query(AssertSqlSafe(format!(
            "WITH configuration AS MATERIALIZED ( \
               SELECT * FROM UNNEST($2::uuid[], $3::bigint[], $4::bytea[], $5::boolean[]) \
                 AS configured(habit_id, item_revision, policy_fingerprint, is_active) \
             ), ranked AS MATERIALIZED ( \
               SELECT evidence.id AS candidate_id, evidence.window_end AS candidate_window_end, \
                 ROW_NUMBER() OVER (PARTITION BY evidence.habit_id \
                   ORDER BY evidence.window_end, evidence.id) AS candidate_rank \
               FROM habit_occurrence_evidence evidence \
               JOIN configuration configured ON configured.habit_id = evidence.habit_id \
               JOIN items item ON item.workspace_id = evidence.workspace_id \
                 AND item.id = evidence.habit_id AND item.revision = configured.item_revision \
               LEFT JOIN habit_occurrence_outcomes outcome \
                 ON outcome.workspace_id = evidence.workspace_id \
                AND outcome.occurrence_evidence_id = evidence.id \
               WHERE evidence.workspace_id = $1 AND configured.is_active \
                 AND item.kind = 'habit' AND item.trashed_at IS NULL \
                 AND item.status NOT IN ('completed', 'skipped', 'cancelled', 'blocked') \
                 AND evidence.policy_fingerprint = configured.policy_fingerprint \
                 AND evidence.window_end <= $6 \
                 AND (outcome.status IS NULL OR outcome.status IN ('unresolved', 'partial')) \
                 AND NOT EXISTS (SELECT 1 FROM habit_effective_reduction_targets( \
                   evidence.workspace_id, evidence.habit_id, configured.policy_fingerprint) \
                   WHERE planner_occurrence_id = evidence.planner_occurrence_id) \
                 AND NOT EXISTS (SELECT 1 FROM habit_pauses source_pause \
                   WHERE source_pause.workspace_id = evidence.workspace_id \
                     AND source_pause.habit_id = evidence.habit_id \
                     AND source_pause.started_at < evidence.window_end \
                     AND (source_pause.ended_at IS NULL \
                       OR source_pause.ended_at > evidence.window_start)) \
             ), candidates AS MATERIALIZED ( \
               SELECT ranked.candidate_id, ranked.candidate_window_end, ranked.candidate_rank \
               FROM ranked LEFT JOIN habit_missed_resolutions resolution \
                 ON resolution.workspace_id = $1 \
                AND resolution.occurrence_evidence_id = ranked.candidate_id \
               WHERE resolution.occurrence_evidence_id IS NULL \
             ), selected AS MATERIALIZED ( \
               SELECT candidate_id, candidate_window_end, candidate_rank FROM candidates \
               ORDER BY candidate_rank, candidate_window_end, candidate_id LIMIT $7 \
             ) \
             SELECT {EVIDENCE_COLUMNS}, item.scheduling_constraints AS item_constraints, \
               selected.candidate_rank, selected.candidate_window_end{OCCURRENCE_OUTCOME_SELECT} \
             JOIN items item ON item.workspace_id = evidence.workspace_id AND item.id = evidence.habit_id \
             JOIN selected ON selected.candidate_id = evidence.id \
             WHERE evidence.workspace_id = $1 \
             ORDER BY selected.candidate_rank, selected.candidate_window_end, evidence.id \
             FOR UPDATE OF evidence"
        )))
        .bind(self.scope.workspace_id)
        .bind(&habit_ids)
        .bind(&item_revisions)
        .bind(&policy_fingerprints)
        .bind(&active_items)
        .bind(write.recorded_at)
        .bind(fresh_limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        let fresh_overflow = fresh_rows.len() > fresh_capacity;
        for row in fresh_rows.iter().take(fresh_capacity) {
            let occurrence = occurrence_from_row(row)?;
            let configuration = write
                .policies
                .get(&occurrence.evidence.habit_id)
                .ok_or(HabitRepositoryError::EvidenceConflict)?;
            let constraints: Value = row.try_get("item_constraints").map_err(storage)?;
            if missed_policy_from_constraints(&constraints)? != configuration.policy {
                return Err(HabitRepositoryError::EvidenceConflict);
            }
            let action = missed_action_tx(
                &mut tx,
                self.scope.workspace_id,
                &occurrence,
                configuration,
                write.recorded_at,
            )
            .await?;
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
            let child_operation_id = Uuid::new_v5(
                &write.idempotency.operation_id,
                occurrence.evidence.id.as_bytes(),
            );
            insert_new_missed_resolution_tx(
                &mut tx,
                self.scope,
                write.idempotency.actor_session_id,
                child_operation_id,
                occurrence,
                &resolution,
                write.recorded_at,
            )
            .await?;
            resolutions.push(resolution);
        }
        let result = HabitMissedReconcileResult {
            resolutions,
            has_more: maintenance_overflow
                || pending_overflow
                || fresh_overflow
                || deferred_pending_work,
        };
        if !result.resolutions.is_empty() || result.has_more {
            insert_receipt(
                &mut tx,
                self.scope,
                &write.idempotency,
                &StoredReceipt::MissedReconcile(result.clone()),
                write.recorded_at,
            )
            .await?;
        } else {
            insert_ephemeral_reconcile_receipt(&mut tx, self.scope, &write.idempotency, &result)
                .await?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(HabitMutation {
            value: result,
            replayed: false,
        })
    }

    async fn resolve_missed(
        &self,
        write: MissedResolveWrite,
    ) -> Result<HabitMutation<HabitMissedResolution>, HabitRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock_habit_mutation_space(&mut tx, self.scope.workspace_id).await?;
        if let Some(receipt) = replay_receipt(&mut tx, self.scope, &write.idempotency).await? {
            let StoredReceipt::MissedResolution(value) = receipt else {
                return Err(HabitRepositoryError::IdempotencyConflict);
            };
            tx.commit().await.map_err(storage)?;
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        let current_item = sqlx::query(
            "SELECT item.revision, item.status, item.trashed_at, item.recurrence, \
               item.scheduling_constraints, \
               NOT EXISTS (SELECT 1 FROM item_hierarchy child_edge \
                 JOIN items child ON child.workspace_id = child_edge.workspace_id \
                   AND child.id = child_edge.child_item_id \
                 WHERE child_edge.workspace_id = item.workspace_id \
                   AND child_edge.parent_item_id = item.id AND child.trashed_at IS NULL) AS is_leaf \
             FROM items item WHERE item.workspace_id = $1 AND item.id = $2 \
               AND item.kind = 'habit' FOR SHARE OF item",
        )
        .bind(self.scope.workspace_id)
        .bind(write.habit_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or(HabitRepositoryError::HabitNotFound(write.habit_id))?;
        // A stored ask prompt can race any canonical edit. Parse the current
        // policy for integrity, but let applicability below turn the race into
        // a durable SourceObsolete cancellation and replayable HTTP 200.
        let constraints: Value = current_item
            .try_get("scheduling_constraints")
            .map_err(storage)?;
        missed_policy_from_constraints(&constraints)?;
        let current_item_revision: i64 = current_item.try_get("revision").map_err(storage)?;
        if from_i64(current_item_revision)? != write.current_item_revision {
            return Err(HabitRepositoryError::EvidenceConflict);
        }
        let current_item_status: String = current_item.try_get("status").map_err(storage)?;
        let trashed_at: Option<DateTime<Utc>> =
            current_item.try_get("trashed_at").map_err(storage)?;
        let recurrence: Option<Value> = current_item.try_get("recurrence").map_err(storage)?;
        let is_leaf: bool = current_item.try_get("is_leaf").map_err(storage)?;
        let item_is_active = trashed_at.is_none()
            && recurrence.is_some()
            && is_leaf
            && !matches!(
                current_item_status.as_str(),
                "completed" | "skipped" | "cancelled" | "blocked"
            );
        if item_is_active != write.current_item_is_active {
            return Err(HabitRepositoryError::EvidenceConflict);
        }
        let configuration = HabitMissedConfiguration {
            item_revision: write.current_item_revision,
            policy_fingerprint: write.current_policy_fingerprint,
            policy: HabitMissedPolicy::Ask,
            is_active: write.current_item_is_active,
        };
        let row = occurrence_row_for_update(
            &mut tx,
            self.scope.workspace_id,
            write.habit_id,
            write.occurrence_id,
        )
        .await?
        .ok_or(HabitRepositoryError::MissedResolutionNotFound(
            write.occurrence_id,
        ))?;
        let occurrence = occurrence_from_row(&row)?;
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
        let cancellation_reason = if item_is_active {
            source_cancellation_reason_tx(
                &mut tx,
                self.scope.workspace_id,
                &occurrence,
                &configuration,
            )
            .await?
        } else {
            Some(HabitMissedCancellationReason::SourceObsolete)
        };
        let action = if let Some(reason) = cancellation_reason {
            let resume_action = match write.action {
                HabitMissedExplicitAction::Skip => HabitMissedResumeAction::Skip,
                HabitMissedExplicitAction::Carry => HabitMissedResumeAction::Carry,
                HabitMissedExplicitAction::ReduceFrequency => {
                    HabitMissedResumeAction::ReduceFrequency
                }
            };
            HabitMissedResolutionAction::Cancelled {
                reason,
                resume_action,
            }
        } else {
            explicit_missed_action_tx(
                &mut tx,
                self.scope.workspace_id,
                &occurrence,
                write.action,
                &configuration,
                write.recorded_at,
            )
            .await?
        };
        let resolution = HabitMissedResolution {
            revision: current
                .revision
                .checked_add(1)
                .ok_or(HabitRepositoryError::Internal)?,
            action,
            updated_at: write.recorded_at,
            ..current.clone()
        };
        update_missed_resolution_tx(
            &mut tx,
            self.scope,
            write.idempotency.actor_session_id,
            write.idempotency.operation_id,
            occurrence,
            &current,
            &resolution,
            matches!(
                resolution.action,
                HabitMissedResolutionAction::Cancelled {
                    resume_action: HabitMissedResumeAction::Skip
                        | HabitMissedResumeAction::Carry
                        | HabitMissedResumeAction::ReduceFrequency,
                    ..
                }
            ),
            write.recorded_at,
        )
        .await?;
        insert_receipt(
            &mut tx,
            self.scope,
            &write.idempotency,
            &StoredReceipt::MissedResolution(resolution.clone()),
            write.recorded_at,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
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
        let after_date = after.map(|cursor| cursor.local_date);
        let after_start = after.map(|cursor| cursor.nominal_start);
        let after_id = after.map(|cursor| cursor.id);
        let fetch_limit =
            i64::try_from(limit.saturating_add(1)).map_err(|_| HabitRepositoryError::Internal)?;
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {EVIDENCE_COLUMNS}{OCCURRENCE_OUTCOME_SELECT} WHERE evidence.workspace_id = $1 AND evidence.habit_id = $2 \
             AND evidence.local_date BETWEEN $3 AND $4 \
             AND ($5::date IS NULL OR (evidence.local_date, evidence.nominal_start, evidence.id) > ($5, $6, $7)) \
             ORDER BY evidence.local_date, evidence.nominal_start, evidence.id LIMIT $8"
        )))
        .bind(self.scope.workspace_id)
        .bind(habit_id)
        .bind(start_date)
        .bind(end_date)
        .bind(after_date)
        .bind(after_start)
        .bind(after_id)
        .bind(fetch_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut values = rows
            .iter()
            .map(occurrence_from_row)
            .collect::<Result<Vec<_>, _>>()?;
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
        let targets = sqlx::query_scalar(
            "SELECT target.planner_occurrence_id \
             FROM habit_effective_reduction_targets($1, $2, $3) target \
             WHERE target.planner_occurrence_id = ANY($4)",
        )
        .bind(self.scope.workspace_id)
        .bind(habit_id)
        .bind(current_policy_fingerprint.as_slice())
        .bind(planner_occurrence_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        Ok(targets.into_iter().collect())
    }

    async fn list_pauses(
        &self,
        habit_id: Uuid,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<HabitPause>, HabitRepositoryError> {
        let rows = sqlx::query(
            "SELECT id, habit_id, revision, started_at, ended_at, preserves_streak, created_at, \
             updated_at FROM habit_pauses WHERE workspace_id = $1 AND habit_id = $2 \
             AND started_at < $4 AND (ended_at IS NULL OR ended_at > $3) ORDER BY started_at, id",
        )
        .bind(self.scope.workspace_id)
        .bind(habit_id)
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(pause_from_row).collect()
    }

    async fn delta_head(&self) -> Result<u64, HabitRepositoryError> {
        let value: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) FROM habit_changes WHERE workspace_id = $1",
        )
        .bind(self.scope.workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        from_i64(value)
    }

    async fn delta(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<HabitDeltaPage, HabitRepositoryError> {
        let head = self.delta_head().await?;
        if after > head {
            return Err(HabitRepositoryError::InvalidCursor);
        }
        let rows = sqlx::query(
            "SELECT sequence, payload FROM habit_changes WHERE workspace_id = $1 AND sequence > $2 \
             ORDER BY sequence LIMIT $3",
        )
        .bind(self.scope.workspace_id)
        .bind(to_i64(after)?)
        .bind(i64::try_from(limit.saturating_add(1)).map_err(|_| HabitRepositoryError::Internal)?)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let has_more = rows.len() > limit;
        let selected = rows.iter().take(limit);
        let mut watermark = after;
        let mut changes = Vec::with_capacity(limit.min(rows.len()));
        for row in selected {
            watermark = from_i64(row.try_get("sequence").map_err(storage)?)?;
            let payload: Value = row.try_get("payload").map_err(storage)?;
            let change =
                serde_json::from_value(payload).map_err(|_| HabitRepositoryError::Internal)?;
            validate_change(&change)?;
            changes.push(change);
        }
        Ok(HabitDeltaPage {
            changes,
            watermark,
            has_more,
        })
    }
}

const EVIDENCE_COLUMNS: &str = "evidence.id, evidence.habit_id, evidence.planner_occurrence_id, \
     evidence.source_schedule_revision_id, evidence.source_item_revision, \
     evidence.policy_fingerprint, evidence.recurrence_identity, evidence.nominal_start, \
     evidence.nominal_end, evidence.window_start, evidence.window_end, evidence.local_date, \
     evidence.timezone_name, evidence.expected_duration_seconds, evidence.expected_quantity, \
     evidence.expected_unit";

const OCCURRENCE_OUTCOME_SELECT: &str = ", outcome.revision AS outcome_revision, outcome.status AS outcome_status, \
     outcome.progress_basis_points, outcome.quantity, outcome.unit, outcome.actual_seconds, \
     outcome.note, outcome.occurred_at, outcome.updated_at, \
     resolution.revision AS missed_resolution_revision, \
     resolution.source_planner_occurrence_id AS missed_source_planner_occurrence_id, \
     resolution.configured_policy AS missed_configured_policy, \
     resolution.action AS missed_action, resolution.cancellation_reason, \
     resolution.cancelled_resume_action, resolution.carry_window_start, \
     resolution.carry_window_end, resolution.suppressed_planner_occurrence_ids, \
     resolution.created_at AS missed_created_at, resolution.updated_at AS missed_updated_at \
     FROM habit_occurrence_evidence evidence LEFT JOIN habit_occurrence_outcomes outcome \
       ON outcome.workspace_id = evidence.workspace_id \
      AND outcome.occurrence_evidence_id = evidence.id \
     LEFT JOIN habit_missed_resolutions resolution \
       ON resolution.workspace_id = evidence.workspace_id \
      AND resolution.occurrence_evidence_id = evidence.id";

async fn occurrence_row_for_update(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    habit_id: Uuid,
    occurrence_id: Uuid,
) -> Result<Option<sqlx::postgres::PgRow>, HabitRepositoryError> {
    sqlx::query(AssertSqlSafe(format!(
        "SELECT {EVIDENCE_COLUMNS}{OCCURRENCE_OUTCOME_SELECT} WHERE evidence.workspace_id = $1 AND evidence.habit_id = $2 AND evidence.id = $3 FOR UPDATE OF evidence"
    )))
    .bind(workspace_id)
    .bind(habit_id)
    .bind(occurrence_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)
}

fn occurrence_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<HabitOccurrence, HabitRepositoryError> {
    let evidence = evidence_from_row(row)?;
    let outcome_revision: Option<i64> = row.try_get("outcome_revision").map_err(storage)?;
    let outcome = outcome_revision
        .map(|revision| {
            let status: String = row.try_get("outcome_status").map_err(storage)?;
            let progress: i32 = row.try_get("progress_basis_points").map_err(storage)?;
            Ok(HabitOutcome {
                revision: from_i64(revision)?,
                status: parse_outcome_status(&status)?,
                progress_basis_points: u16::try_from(progress)
                    .map_err(|_| HabitRepositoryError::Internal)?,
                quantity: row.try_get("quantity").map_err(storage)?,
                unit: row.try_get("unit").map_err(storage)?,
                actual_seconds: row
                    .try_get::<Option<i64>, _>("actual_seconds")
                    .map_err(storage)?
                    .map(from_i64)
                    .transpose()?,
                note: row.try_get("note").map_err(storage)?,
                occurred_at: row.try_get("occurred_at").map_err(storage)?,
                updated_at: row.try_get("updated_at").map_err(storage)?,
            })
        })
        .transpose()?;
    let missed_revision: Option<i64> =
        row.try_get("missed_resolution_revision").map_err(storage)?;
    let missed_resolution = missed_revision
        .map(|revision| missed_resolution_from_row(row, revision))
        .transpose()?;
    let occurrence = HabitOccurrence {
        evidence,
        outcome,
        missed_resolution,
    };
    validate_occurrence(&occurrence)?;
    Ok(occurrence)
}

fn missed_resolution_from_row(
    row: &sqlx::postgres::PgRow,
    revision: i64,
) -> Result<HabitMissedResolution, HabitRepositoryError> {
    let policy: String = row.try_get("missed_configured_policy").map_err(storage)?;
    let action: String = row.try_get("missed_action").map_err(storage)?;
    let cancellation_reason: Option<String> =
        row.try_get("cancellation_reason").map_err(storage)?;
    let cancelled_resume_action: Option<String> =
        row.try_get("cancelled_resume_action").map_err(storage)?;
    let carry_start: Option<DateTime<Utc>> = row.try_get("carry_window_start").map_err(storage)?;
    let carry_end: Option<DateTime<Utc>> = row.try_get("carry_window_end").map_err(storage)?;
    let suppressed: Vec<Uuid> = row
        .try_get("suppressed_planner_occurrence_ids")
        .map_err(storage)?;
    let resolution = HabitMissedResolution {
        occurrence_evidence_id: row.try_get("id").map_err(storage)?,
        habit_id: row.try_get("habit_id").map_err(storage)?,
        source_planner_occurrence_id: row
            .try_get("missed_source_planner_occurrence_id")
            .map_err(storage)?,
        revision: from_i64(revision)?,
        configured_policy: parse_missed_policy(&policy)?,
        action: parse_missed_action(
            &action,
            cancellation_reason.as_deref(),
            cancelled_resume_action.as_deref(),
            carry_start,
            carry_end,
            suppressed,
        )?,
        created_at: row.try_get("missed_created_at").map_err(storage)?,
        updated_at: row.try_get("missed_updated_at").map_err(storage)?,
    };
    validate_resolution(&resolution)?;
    Ok(resolution)
}

fn evidence_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<HabitOccurrenceEvidence, HabitRepositoryError> {
    let fingerprint: Vec<u8> = row.try_get("policy_fingerprint").map_err(storage)?;
    if fingerprint.len() != 32 {
        return Err(HabitRepositoryError::Internal);
    }
    let source_revision: i64 = row.try_get("source_item_revision").map_err(storage)?;
    let expected_duration: Option<i64> =
        row.try_get("expected_duration_seconds").map_err(storage)?;
    let evidence = HabitOccurrenceEvidence {
        id: row.try_get("id").map_err(storage)?,
        habit_id: row.try_get("habit_id").map_err(storage)?,
        planner_occurrence_id: row.try_get("planner_occurrence_id").map_err(storage)?,
        source_schedule_revision_id: row
            .try_get("source_schedule_revision_id")
            .map_err(storage)?,
        source_item_revision: from_i64(source_revision)?,
        policy_fingerprint: prefixed_hex(&fingerprint),
        identity: row.try_get("recurrence_identity").map_err(storage)?,
        nominal_start: row.try_get("nominal_start").map_err(storage)?,
        nominal_end: row.try_get("nominal_end").map_err(storage)?,
        window_start: row.try_get("window_start").map_err(storage)?,
        window_end: row.try_get("window_end").map_err(storage)?,
        local_date: row.try_get("local_date").map_err(storage)?,
        timezone_name: row.try_get("timezone_name").map_err(storage)?,
        expected_duration_seconds: expected_duration.map(from_i64).transpose()?,
        expected_quantity: row.try_get("expected_quantity").map_err(storage)?,
        expected_unit: row.try_get("expected_unit").map_err(storage)?,
    };
    evidence
        .validate()
        .map_err(|_| HabitRepositoryError::Internal)?;
    Ok(evidence)
}

fn pause_from_row(row: &sqlx::postgres::PgRow) -> Result<HabitPause, HabitRepositoryError> {
    Ok(HabitPause {
        id: row.try_get("id").map_err(storage)?,
        habit_id: row.try_get("habit_id").map_err(storage)?,
        revision: from_i64(row.try_get("revision").map_err(storage)?)?,
        started_at: row.try_get("started_at").map_err(storage)?,
        ended_at: row.try_get("ended_at").map_err(storage)?,
        preserves_streak: row.try_get("preserves_streak").map_err(storage)?,
        created_at: row.try_get("created_at").map_err(storage)?,
        updated_at: row.try_get("updated_at").map_err(storage)?,
    })
}

fn missed_policy_from_constraints(
    value: &Value,
) -> Result<HabitMissedPolicy, HabitRepositoryError> {
    let metadata: dayweave_compose::SchedulingMetadata =
        serde_json::from_value(value.clone()).map_err(|_| HabitRepositoryError::Internal)?;
    Ok(metadata.habit_missed_policy.into())
}

async fn missed_action_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let action = derive_missed_resolution_action(occurrence, configuration.policy, now)
        .map_err(|_| HabitRepositoryError::Internal)?;
    if matches!(action, HabitMissedResolutionAction::ReductionPending) {
        match reduction_action_tx(tx, workspace_id, occurrence, configuration, now).await {
            Ok(bound) => Ok(bound),
            Err(HabitRepositoryError::MissedReductionUnavailable) => Ok(action),
            Err(error) => Err(error),
        }
    } else {
        Ok(action)
    }
}

async fn source_cancellation_reason_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
) -> Result<Option<HabitMissedCancellationReason>, HabitRepositoryError> {
    if !configuration.is_active {
        return Ok(Some(HabitMissedCancellationReason::SourceObsolete));
    }
    match occurrence.outcome.as_ref().map(|outcome| outcome.status) {
        Some(HabitOutcomeStatus::Completed) => {
            return Ok(Some(HabitMissedCancellationReason::SourceCompleted));
        }
        Some(HabitOutcomeStatus::Skipped) => {
            return Ok(Some(HabitMissedCancellationReason::SourceSkipped));
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
            resume_action: HabitMissedResumeAction::Carry,
            ..
        }) => {
            let previous_snapshot: Option<Value> = sqlx::query_scalar(
                "SELECT previous_snapshot FROM habit_missed_resolution_versions \
                 WHERE workspace_id = $1 AND occurrence_evidence_id = $2 AND revision = $3",
            )
            .bind(workspace_id)
            .bind(occurrence.evidence.id)
            .bind(to_i64(
                occurrence
                    .missed_resolution
                    .as_ref()
                    .ok_or(HabitRepositoryError::Internal)?
                    .revision,
            )?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(storage)?
            .flatten();
            previous_snapshot
                .and_then(|snapshot| serde_json::from_value::<HabitMissedResolution>(snapshot).ok())
                .and_then(|resolution| match resolution.action {
                    HabitMissedResolutionAction::Carry {
                        window_start,
                        window_end,
                    } => Some((window_start, window_end)),
                    _ => None,
                })
                .unwrap_or((
                    occurrence.evidence.window_start,
                    occurrence.evidence.window_end,
                ))
        }
        _ => (
            occurrence.evidence.window_start,
            occurrence.evidence.window_end,
        ),
    };
    let paused: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM habit_pauses WHERE workspace_id = $1 \
         AND habit_id = $2 AND started_at < $4 \
         AND (ended_at IS NULL OR ended_at > $3))",
    )
    .bind(workspace_id)
    .bind(occurrence.evidence.habit_id)
    .bind(window_start)
    .bind(window_end)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    if paused {
        return Ok(Some(HabitMissedCancellationReason::SourcePaused));
    }
    if occurrence.evidence.policy_fingerprint
        != prefixed_hex(configuration.policy_fingerprint.as_slice())
    {
        return Ok(Some(HabitMissedCancellationReason::SourceObsolete));
    }
    Ok(None)
}

async fn bound_reduction_needs_rebind_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
) -> Result<bool, HabitRepositoryError> {
    let Some(HabitMissedResolutionAction::ReduceFrequency {
        suppressed_planner_occurrence_ids,
    }) = occurrence
        .missed_resolution
        .as_ref()
        .map(|resolution| &resolution.action)
    else {
        return Ok(false);
    };
    let Some(target_id) = suppressed_planner_occurrence_ids.first() else {
        return Ok(true);
    };
    let row = sqlx::query(
        "SELECT \
           (target_outcome.status IS NULL OR target_outcome.status = 'unresolved') AS outcome_eligible, \
           EXISTS (SELECT 1 FROM habit_pauses target_pause \
             WHERE target_pause.workspace_id = target.workspace_id \
               AND target_pause.habit_id = target.habit_id \
               AND target_pause.started_at < target.window_end \
               AND (target_pause.ended_at IS NULL \
                 OR target_pause.ended_at > target.window_start)) AS target_paused, \
           EXISTS (SELECT 1 FROM schedule_revisions current_revision \
             WHERE current_revision.workspace_id = target.workspace_id \
               AND current_revision.state = 'published' \
               AND current_revision.horizon_start <= target.window_start \
               AND current_revision.horizon_end >= target.window_end) AS horizon_covers_target, \
           EXISTS (SELECT 1 FROM habit_occurrence_publications publication \
             JOIN schedule_revisions current_revision \
               ON current_revision.workspace_id = publication.workspace_id \
              AND current_revision.id = publication.schedule_revision_id \
              AND current_revision.state = 'published' \
             WHERE publication.workspace_id = target.workspace_id \
               AND publication.occurrence_evidence_id = target.id \
               AND publication.occurrence_state IN ('generated', 'skipped') \
               AND target.policy_fingerprint = $4) AS current_member \
         FROM habit_occurrence_evidence target \
         LEFT JOIN habit_occurrence_outcomes target_outcome \
           ON target_outcome.workspace_id = target.workspace_id \
          AND target_outcome.occurrence_evidence_id = target.id \
         WHERE target.workspace_id = $1 AND target.habit_id = $2 \
           AND target.planner_occurrence_id = $3",
    )
    .bind(workspace_id)
    .bind(occurrence.evidence.habit_id)
    .bind(target_id)
    .bind(configuration.policy_fingerprint.as_slice())
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    let Some(row) = row else {
        return Ok(true);
    };
    let outcome_eligible: bool = row.try_get("outcome_eligible").map_err(storage)?;
    let target_paused: bool = row.try_get("target_paused").map_err(storage)?;
    let horizon_covers_target: bool = row.try_get("horizon_covers_target").map_err(storage)?;
    let current_member: bool = row.try_get("current_member").map_err(storage)?;
    Ok(!outcome_eligible || target_paused || (horizon_covers_target && !current_member))
}

async fn maintenance_action_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
    now: DateTime<Utc>,
) -> Result<Option<HabitMissedResolutionAction>, HabitRepositoryError> {
    let current = occurrence
        .missed_resolution
        .as_ref()
        .ok_or(HabitRepositoryError::Internal)?;
    if let Some(reason) =
        source_cancellation_reason_tx(tx, workspace_id, occurrence, configuration).await?
    {
        if matches!(
            current.action,
            HabitMissedResolutionAction::Cancelled { .. }
        ) {
            return Ok(None);
        }
        let resume_action =
            missed_resume_action(&current.action).ok_or(HabitRepositoryError::Internal)?;
        return Ok(Some(HabitMissedResolutionAction::Cancelled {
            reason,
            resume_action,
        }));
    }
    let reduction_needs_rebind = if matches!(
        current.action,
        HabitMissedResolutionAction::ReduceFrequency { .. }
    ) {
        bound_reduction_needs_rebind_tx(tx, workspace_id, occurrence, configuration).await?
    } else {
        false
    };
    match &current.action {
        HabitMissedResolutionAction::Cancelled {
            reason:
                HabitMissedCancellationReason::SourceCompleted
                | HabitMissedCancellationReason::SourceSkipped
                | HabitMissedCancellationReason::SourcePaused
                | HabitMissedCancellationReason::SourceObsolete,
            resume_action,
        } => restore_action_tx(
            tx,
            workspace_id,
            occurrence,
            *resume_action,
            configuration,
            now,
        )
        .await
        .map(Some),
        HabitMissedResolutionAction::ReduceFrequency { .. } if reduction_needs_rebind => {
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

async fn restore_action_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    resume_action: HabitMissedResumeAction,
    configuration: &HabitMissedConfiguration,
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let policy = match resume_action {
        HabitMissedResumeAction::DecisionRequired => HabitMissedPolicy::Ask,
        HabitMissedResumeAction::Skip => HabitMissedPolicy::Skip,
        HabitMissedResumeAction::Carry => HabitMissedPolicy::Carry,
        HabitMissedResumeAction::ReduceFrequency => HabitMissedPolicy::ReduceFrequency,
    };
    let action = derive_missed_resolution_action(occurrence, policy, now)
        .map_err(|_| HabitRepositoryError::Internal)?;
    if matches!(action, HabitMissedResolutionAction::ReductionPending) {
        match reduction_action_tx(tx, workspace_id, occurrence, configuration, now).await {
            Ok(action) => Ok(action),
            Err(HabitRepositoryError::MissedReductionUnavailable) => {
                Ok(HabitMissedResolutionAction::ReductionPending)
            }
            Err(error) => Err(error),
        }
    } else {
        Ok(action)
    }
}

const fn missed_resume_action(
    action: &HabitMissedResolutionAction,
) -> Option<HabitMissedResumeAction> {
    match action {
        HabitMissedResolutionAction::DecisionRequired => {
            Some(HabitMissedResumeAction::DecisionRequired)
        }
        HabitMissedResolutionAction::Skip => Some(HabitMissedResumeAction::Skip),
        HabitMissedResolutionAction::Carry { .. } => Some(HabitMissedResumeAction::Carry),
        HabitMissedResolutionAction::ReductionPending
        | HabitMissedResolutionAction::ReduceFrequency { .. } => {
            Some(HabitMissedResumeAction::ReduceFrequency)
        }
        HabitMissedResolutionAction::Cancelled { .. } => None,
    }
}

async fn explicit_missed_action_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    action: HabitMissedExplicitAction,
    configuration: &HabitMissedConfiguration,
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
        match reduction_action_tx(tx, workspace_id, occurrence, configuration, now).await {
            Ok(bound) => Ok(bound),
            Err(HabitRepositoryError::MissedReductionUnavailable) => Ok(derived),
            Err(error) => Err(error),
        }
    } else {
        Ok(derived)
    }
}

async fn reduction_action_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    occurrence: &HabitOccurrence,
    configuration: &HabitMissedConfiguration,
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    let source_ordinal = recurrence_identity_ordinal(&occurrence.evidence.identity)
        .ok_or(HabitRepositoryError::Internal)?;
    let target: Option<Uuid> = sqlx::query_scalar(
        "SELECT planner_occurrence_id FROM habit_available_reduction_target( \
           $1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(workspace_id)
    .bind(occurrence.evidence.habit_id)
    .bind(occurrence.evidence.id)
    .bind(occurrence.evidence.nominal_start)
    .bind(i64::from(source_ordinal))
    .bind(occurrence.evidence.planner_occurrence_id)
    .bind(configuration.policy_fingerprint.as_slice())
    .bind(now)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    let target = target.ok_or(HabitRepositoryError::MissedReductionUnavailable)?;
    Ok(HabitMissedResolutionAction::ReduceFrequency {
        suppressed_planner_occurrence_ids: vec![target],
    })
}

async fn insert_new_missed_resolution_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    actor_session_id: Option<Uuid>,
    operation_id: Uuid,
    occurrence: HabitOccurrence,
    resolution: &HabitMissedResolution,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
    validate_resolution(resolution)?;
    if occurrence.evidence.id != resolution.occurrence_evidence_id
        || occurrence.evidence.habit_id != resolution.habit_id
        || occurrence.evidence.planner_occurrence_id != resolution.source_planner_occurrence_id
        || occurrence.missed_resolution.is_some()
    {
        return Err(HabitRepositoryError::Internal);
    }
    let columns = missed_action_columns(&resolution.action);
    sqlx::query(
        "INSERT INTO habit_missed_resolutions (workspace_id, occurrence_evidence_id, habit_id, \
         source_planner_occurrence_id, revision, configured_policy, action, cancellation_reason, \
         cancelled_resume_action, carry_window_start, carry_window_end, \
         suppressed_planner_occurrence_ids, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$13)",
    )
    .bind(scope.workspace_id)
    .bind(resolution.occurrence_evidence_id)
    .bind(resolution.habit_id)
    .bind(resolution.source_planner_occurrence_id)
    .bind(to_i64(resolution.revision)?)
    .bind(missed_policy_name(resolution.configured_policy))
    .bind(columns.action)
    .bind(columns.cancellation_reason)
    .bind(columns.cancelled_resume_action)
    .bind(columns.carry_start)
    .bind(columns.carry_end)
    .bind(&columns.suppressed)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    let snapshot = serde_json::to_value(resolution).map_err(|_| HabitRepositoryError::Internal)?;
    let version_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO habit_missed_resolution_versions (id, workspace_id, occurrence_evidence_id, \
         revision, operation_id, previous_snapshot, resolution_snapshot, recorded_at) \
         VALUES ($1,$2,$3,$4,$5,NULL,$6,$7)",
    )
    .bind(version_id)
    .bind(scope.workspace_id)
    .bind(resolution.occurrence_evidence_id)
    .bind(to_i64(resolution.revision)?)
    .bind(operation_id)
    .bind(snapshot)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    emit_missed_resolution_change_tx(
        tx,
        scope,
        actor_session_id,
        operation_id,
        occurrence,
        resolution,
        0,
        version_id,
        now,
    )
    .await
}

#[allow(clippy::too_many_arguments)] // Projection, immutable history, audit, delta, and outbox share one transaction.
async fn update_missed_resolution_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    actor_session_id: Option<Uuid>,
    operation_id: Uuid,
    occurrence: HabitOccurrence,
    previous: &HabitMissedResolution,
    resolution: &HabitMissedResolution,
    explicit_selection: bool,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
    if !(valid_missed_resolution_transition(previous, resolution)
        || explicit_selection
            && valid_explicit_missed_cancellation_transition(previous, resolution))
    {
        return Err(HabitRepositoryError::Internal);
    }
    let columns = missed_action_columns(&resolution.action);
    let updated = sqlx::query(
        "UPDATE habit_missed_resolutions SET revision = $4, action = $5, \
         cancellation_reason = $6, cancelled_resume_action = $7, \
         cancelled_explicit_selection = $8, carry_window_start = $9, carry_window_end = $10, \
         suppressed_planner_occurrence_ids = $11, updated_at = $12 \
         WHERE workspace_id = $1 AND occurrence_evidence_id = $2 AND habit_id = $3 AND revision = $13",
    )
    .bind(scope.workspace_id)
    .bind(resolution.occurrence_evidence_id)
    .bind(resolution.habit_id)
    .bind(to_i64(resolution.revision)?)
    .bind(columns.action)
    .bind(columns.cancellation_reason)
    .bind(columns.cancelled_resume_action)
    .bind(explicit_selection)
    .bind(columns.carry_start)
    .bind(columns.carry_end)
    .bind(&columns.suppressed)
    .bind(now)
    .bind(to_i64(previous.revision)?)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    if updated.rows_affected() != 1 {
        return Err(HabitRepositoryError::Internal);
    }
    let previous_snapshot =
        serde_json::to_value(previous).map_err(|_| HabitRepositoryError::Internal)?;
    let snapshot = serde_json::to_value(resolution).map_err(|_| HabitRepositoryError::Internal)?;
    let version_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO habit_missed_resolution_versions (id, workspace_id, occurrence_evidence_id, \
         revision, operation_id, previous_snapshot, resolution_snapshot, recorded_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(version_id)
    .bind(scope.workspace_id)
    .bind(resolution.occurrence_evidence_id)
    .bind(to_i64(resolution.revision)?)
    .bind(operation_id)
    .bind(previous_snapshot)
    .bind(snapshot)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    emit_missed_resolution_change_tx(
        tx,
        scope,
        actor_session_id,
        operation_id,
        occurrence,
        resolution,
        previous.revision,
        version_id,
        now,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn emit_missed_resolution_change_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    actor_session_id: Option<Uuid>,
    operation_id: Uuid,
    mut occurrence: HabitOccurrence,
    resolution: &HabitMissedResolution,
    previous_revision: u64,
    version_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
    occurrence.missed_resolution = Some(resolution.clone());
    validate_occurrence(&occurrence)?;
    let change = HabitDeltaChange::OccurrenceUpsert { occurrence };
    let sequence = insert_change(
        tx,
        scope.workspace_id,
        "occurrence_upsert",
        resolution.occurrence_evidence_id,
        resolution.revision,
        &change,
        now,
    )
    .await?;
    insert_audit(
        tx,
        scope,
        actor_session_id,
        operation_id,
        "habit.missed.resolved",
        "habit_occurrence",
        resolution.occurrence_evidence_id,
        previous_revision,
        resolution.revision,
        json!({
            "version_id": version_id,
            "action": missed_action_name(&resolution.action),
            "change_sequence": sequence,
        }),
        now,
    )
    .await?;
    insert_content_free_outbox(
        tx,
        scope.workspace_id,
        resolution.occurrence_evidence_id,
        resolution.revision,
        "habit.occurrence.changed",
        sequence,
        now,
    )
    .await
}

struct MissedActionColumns {
    action: &'static str,
    cancellation_reason: Option<&'static str>,
    cancelled_resume_action: Option<&'static str>,
    carry_start: Option<DateTime<Utc>>,
    carry_end: Option<DateTime<Utc>>,
    suppressed: Vec<Uuid>,
}

fn missed_action_columns(action: &HabitMissedResolutionAction) -> MissedActionColumns {
    match action {
        HabitMissedResolutionAction::DecisionRequired => MissedActionColumns {
            action: "decision_required",
            cancellation_reason: None,
            cancelled_resume_action: None,
            carry_start: None,
            carry_end: None,
            suppressed: Vec::new(),
        },
        HabitMissedResolutionAction::ReductionPending => MissedActionColumns {
            action: "reduction_pending",
            cancellation_reason: None,
            cancelled_resume_action: None,
            carry_start: None,
            carry_end: None,
            suppressed: Vec::new(),
        },
        HabitMissedResolutionAction::Cancelled {
            reason,
            resume_action,
        } => MissedActionColumns {
            action: "cancelled",
            cancellation_reason: Some(missed_cancellation_reason_name(*reason)),
            cancelled_resume_action: Some(missed_resume_action_name(*resume_action)),
            carry_start: None,
            carry_end: None,
            suppressed: Vec::new(),
        },
        HabitMissedResolutionAction::Skip => MissedActionColumns {
            action: "skip",
            cancellation_reason: None,
            cancelled_resume_action: None,
            carry_start: None,
            carry_end: None,
            suppressed: Vec::new(),
        },
        HabitMissedResolutionAction::Carry {
            window_start,
            window_end,
        } => MissedActionColumns {
            action: "carry",
            cancellation_reason: None,
            cancelled_resume_action: None,
            carry_start: Some(*window_start),
            carry_end: Some(*window_end),
            suppressed: Vec::new(),
        },
        HabitMissedResolutionAction::ReduceFrequency {
            suppressed_planner_occurrence_ids,
        } => MissedActionColumns {
            action: "reduce_frequency",
            cancellation_reason: None,
            cancelled_resume_action: None,
            carry_start: None,
            carry_end: None,
            suppressed: suppressed_planner_occurrence_ids.clone(),
        },
    }
}

async fn lock_habit_mutation_space(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), HabitRepositoryError> {
    lock_canonical_item_space(tx, workspace_id)
        .await
        .map_err(storage)?;
    lock_habit_change_space(tx, workspace_id)
        .await
        .map_err(storage)
}

/// Serializes changes to the authoritative habit projection after the caller
/// has acquired the canonical-item workspace lock. Schedule publication uses
/// the same lock after its execution/canonical locks so its habit-change-head
/// fence remains valid until commit.
pub(crate) async fn lock_habit_change_space(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.habits.v1:' || $1::text, 0))",
    )
    .bind(workspace_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn require_active_habit(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    habit_id: Uuid,
) -> Result<Value, HabitRepositoryError> {
    let row = sqlx::query(
        "SELECT kind, recurrence, scheduling_constraints FROM items \
         WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL FOR SHARE",
    )
    .bind(workspace_id)
    .bind(habit_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or(HabitRepositoryError::HabitNotFound(habit_id))?;
    let kind: String = row.try_get("kind").map_err(storage)?;
    let recurrence: Option<Value> = row.try_get("recurrence").map_err(storage)?;
    if kind != "habit" || recurrence.is_none() {
        return Err(HabitRepositoryError::NotHabit(habit_id));
    }
    row.try_get("scheduling_constraints").map_err(storage)
}

async fn replay_receipt(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &HabitIdempotency,
) -> Result<Option<StoredReceipt>, HabitRepositoryError> {
    let mut rows = sqlx::query(
        "SELECT namespace, key_hash, request_fingerprint, response_json \
         FROM habit_operation_receipts WHERE workspace_id = $1 \
         AND ((namespace = $2 AND key_hash = $3) OR operation_id = $4) FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .bind(idempotency.operation_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage)?;
    if rows.is_empty() {
        // Empty reconcile receipts share the workspace-global operation-id
        // namespace with permanent habit receipts. Every habit mutation must
        // observe a still-live ephemeral operation before it can claim the
        // same identifier in the permanent ledger.
        sqlx::query(
            "DELETE FROM idempotency_keys WHERE workspace_id = $1 \
             AND namespace = $2 \
             AND resource_type = $3 AND expires_at <= clock_timestamp() \
             AND (resource_id = $5 OR ($6 AND key_hash = $4))",
        )
        .bind(scope.workspace_id)
        .bind(MISSED_RECONCILE_NAMESPACE)
        .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
        .bind(idempotency.key_hash.as_slice())
        .bind(idempotency.operation_id)
        .bind(idempotency.namespace == MISSED_RECONCILE_NAMESPACE)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
        rows = sqlx::query(
            "SELECT namespace, key_hash, request_fingerprint, response_json \
             FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
               AND resource_type = $3 AND state = 'completed' \
               AND expires_at > clock_timestamp() \
               AND (resource_id = $5 OR ($6 AND key_hash = $4)) FOR SHARE",
        )
        .bind(scope.workspace_id)
        .bind(MISSED_RECONCILE_NAMESPACE)
        .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
        .bind(idempotency.key_hash.as_slice())
        .bind(idempotency.operation_id)
        .bind(idempotency.namespace == MISSED_RECONCILE_NAMESPACE)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
    }
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1 {
        return Err(HabitRepositoryError::IdempotencyConflict);
    }
    let row = &rows[0];
    let namespace: String = row.try_get("namespace").map_err(storage)?;
    let key_hash: Vec<u8> = row.try_get("key_hash").map_err(storage)?;
    let fingerprint: Vec<u8> = row.try_get("request_fingerprint").map_err(storage)?;
    if namespace != idempotency.namespace
        || key_hash.as_slice() != idempotency.key_hash
        || fingerprint.as_slice() != idempotency.request_fingerprint
    {
        return Err(HabitRepositoryError::IdempotencyConflict);
    }
    let response: Value = row.try_get("response_json").map_err(storage)?;
    let receipt = serde_json::from_value(response).map_err(|_| HabitRepositoryError::Internal)?;
    StoredReceipt::validate(&receipt)?;
    Ok(Some(receipt))
}

async fn insert_receipt(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &HabitIdempotency,
    response: &StoredReceipt,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
    response.validate()?;
    let response = serde_json::to_value(response).map_err(|_| HabitRepositoryError::Internal)?;
    sqlx::query(
        "INSERT INTO habit_operation_receipts (workspace_id, namespace, key_hash, operation_id, \
         request_fingerprint, response_json, completed_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .bind(idempotency.operation_id)
    .bind(idempotency.request_fingerprint.as_slice())
    .bind(response)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            HabitRepositoryError::IdempotencyConflict
        } else {
            HabitRepositoryError::Internal
        }
    })?;
    Ok(())
}

async fn insert_ephemeral_reconcile_receipt(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &HabitIdempotency,
    response: &HabitMissedReconcileResult,
) -> Result<(), HabitRepositoryError> {
    let stored = StoredReceipt::MissedReconcile(response.clone());
    stored.validate()?;
    let response = serde_json::to_value(stored).map_err(|_| HabitRepositoryError::Internal)?;

    // Automatic clients poll frequently. Expiry plus a hard per-workspace cap
    // preserves a useful exact-retry window without allowing terminal scans to
    // grow the immutable mutation-receipt ledger forever.
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
         AND resource_type = $3 AND expires_at <= clock_timestamp()",
    )
    .bind(scope.workspace_id)
    .bind(MISSED_RECONCILE_NAMESPACE)
    .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    let retained: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
         AND resource_type = $3",
    )
    .bind(scope.workspace_id)
    .bind(MISSED_RECONCILE_NAMESPACE)
    .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    if retained >= 4_096 {
        sqlx::query(
            "DELETE FROM idempotency_keys WHERE ctid IN ( \
               SELECT ctid FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
                 AND resource_type = $3 \
                 AND created_at <= clock_timestamp() - INTERVAL '12 hours' \
               ORDER BY created_at, expires_at LIMIT 1)",
        )
        .bind(scope.workspace_id)
        .bind(MISSED_RECONCILE_NAMESPACE)
        .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
        let retained: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
             AND resource_type = $3",
        )
        .bind(scope.workspace_id)
        .bind(MISSED_RECONCILE_NAMESPACE)
        .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)?;
        if retained >= 4_096 {
            return Err(HabitRepositoryError::ReconcileReceiptCapacity);
        }
    }
    sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
           state, resource_type, resource_id, response_json, created_at, updated_at, expires_at) \
         VALUES ($1, $2, $3, $4, 'completed', $5, $6, $7, clock_timestamp(), \
           clock_timestamp(), clock_timestamp() + INTERVAL '24 hours')",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .bind(idempotency.request_fingerprint.as_slice())
    .bind(MISSED_RECONCILE_EPHEMERAL_RESOURCE)
    .bind(idempotency.operation_id)
    .bind(response)
    .execute(&mut **tx)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            HabitRepositoryError::IdempotencyConflict
        } else {
            HabitRepositoryError::Internal
        }
    })?;
    Ok(())
}

async fn insert_change(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    kind: &str,
    entity_id: Uuid,
    component_revision: u64,
    change: &HabitDeltaChange,
    changed_at: DateTime<Utc>,
) -> Result<u64, HabitRepositoryError> {
    validate_change(change)?;
    let payload = serde_json::to_value(change).map_err(|_| HabitRepositoryError::Internal)?;
    let sequence: i64 = sqlx::query_scalar(
        "INSERT INTO habit_changes (workspace_id, change_kind, entity_id, component_revision, payload, changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING sequence",
    )
    .bind(workspace_id)
    .bind(kind)
    .bind(entity_id)
    .bind(to_i64(component_revision)?)
    .bind(payload)
    .bind(changed_at)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    from_i64(sequence)
}

#[allow(clippy::too_many_arguments)]
async fn insert_audit(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    actor_session_id: Option<Uuid>,
    operation_id: Uuid,
    operation_type: &str,
    entity_type: &str,
    entity_id: Uuid,
    base_revision: u64,
    result_revision: u64,
    metadata: Value,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, actor_session_id, request_id, \
         operation_type, entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'succeeded', $11, $12)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(actor_session_id)
    .bind(operation_id.to_string())
    .bind(operation_type)
    .bind(entity_type)
    .bind(entity_id)
    .bind((base_revision > 0).then(|| to_i64(base_revision)).transpose()?)
    .bind(to_i64(result_revision)?)
    .bind(metadata)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn insert_content_free_outbox(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    entity_id: Uuid,
    component_revision: u64,
    event_type: &str,
    sequence: u64,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
    sqlx::query(
        "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, aggregate_revision, \
         event_type, deduplication_key, payload, headers, available_at, created_at, updated_at) \
         VALUES ($1, $2, 'habit', $3, $4, $5, $6, $7, '{}'::jsonb, $8, $8, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(workspace_id)
    .bind(entity_id)
    .bind(to_i64(sequence)?)
    .bind(event_type)
    .bind(format!("habit:{entity_id}:{sequence}"))
    .bind(json!({
        "entity_id": entity_id,
        "aggregate_revision": sequence,
        "component_revision": component_revision,
        "change_sequence": sequence,
    }))
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum PublishedHabitEvidenceError {
    #[error("published habit occurrence evidence conflicts with prior history")]
    Conflict,
    #[error("published habit occurrence evidence is invalid")]
    Invalid,
    #[error("published habit occurrence evidence storage is unavailable")]
    Unavailable,
}

fn authoritative_evidence_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<HabitOccurrenceEvidence, PublishedHabitEvidenceError> {
    evidence_from_row(row).map_err(|_| PublishedHabitEvidenceError::Invalid)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AuthoritativeHabitRecurrence {
    pub(crate) change_head: u64,
    pub(crate) context: RecurrenceContext,
}

#[allow(clippy::too_many_lines)] // One repeatable snapshot assembles all recurrence lifecycle evidence.
pub(crate) async fn authoritative_habit_recurrence_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    horizon_start: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
    moved_occurrence_ids: &[Uuid],
) -> Result<AuthoritativeHabitRecurrence, PublishedHabitEvidenceError> {
    if horizon_start >= horizon_end
        || moved_occurrence_ids.len() > MAX_AUTHORITATIVE_MOVED_OCCURRENCES
    {
        return Err(PublishedHabitEvidenceError::Invalid);
    }
    let head: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) FROM habit_changes WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let change_head = u64::try_from(head).map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {EVIDENCE_COLUMNS}, outcome.status, outcome.progress_basis_points \
         FROM habit_occurrence_evidence evidence JOIN habit_occurrence_outcomes outcome \
           ON outcome.workspace_id = evidence.workspace_id \
          AND outcome.occurrence_evidence_id = evidence.id \
         JOIN items item ON item.workspace_id = evidence.workspace_id AND item.id = evidence.habit_id \
         WHERE evidence.workspace_id = $1 AND item.kind = 'habit' AND item.trashed_at IS NULL \
           AND outcome.status IN ('partial', 'completed', 'skipped') \
           AND ((evidence.window_start < $3 AND evidence.window_end > $2) \
                OR evidence.planner_occurrence_id = ANY($4)) \
         ORDER BY evidence.habit_id, evidence.planner_occurrence_id"
    )))
    .bind(workspace_id)
    .bind(horizon_start)
    .bind(horizon_end)
    .bind(moved_occurrence_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let mut context = RecurrenceContext::default();
    for row in rows {
        let evidence = authoritative_evidence_from_row(&row)?;
        let habit_id = evidence.habit_id;
        let occurrence_id = evidence.planner_occurrence_id;
        let status: String = row
            .try_get("status")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        match status.as_str() {
            "partial" => {
                let progress: i32 = row
                    .try_get("progress_basis_points")
                    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
                let progress_basis_points =
                    u16::try_from(progress).map_err(|_| PublishedHabitEvidenceError::Invalid)?;
                if !(1..10_000).contains(&progress_basis_points) {
                    return Err(PublishedHabitEvidenceError::Invalid);
                }
                // Quantity-only habits have no time demand to reduce. For a
                // timed habit, use the exact immutable estimate admitted with
                // this occurrence and let core derive the positive,
                // ceiling-rounded remainder from normalized progress.
                let expected_seconds = evidence.expected_duration_seconds;
                if let Some(expected_seconds) = expected_seconds {
                    let expected_minutes = expected_seconds
                        .checked_add(59)
                        .ok_or(PublishedHabitEvidenceError::Invalid)?
                        / 60;
                    let expected_duration_minutes = Minutes(
                        u32::try_from(expected_minutes)
                            .map_err(|_| PublishedHabitEvidenceError::Invalid)?,
                    );
                    if context
                        .partial_progress
                        .insert(
                            OccurrenceId(occurrence_id),
                            RecurrencePartialProgress {
                                progress_basis_points,
                                expected_duration_minutes,
                                remaining_duration_minutes: None,
                            },
                        )
                        .is_some()
                    {
                        return Err(PublishedHabitEvidenceError::Conflict);
                    }
                }
            }
            "completed" => {
                context
                    .completed_occurrence_ids
                    .insert(OccurrenceId(occurrence_id));
            }
            "skipped" => context.exceptions.push(RecurrenceException {
                item_id: ItemId(habit_id),
                selector: RecurrenceExceptionSelector::Occurrence {
                    id: OccurrenceId(occurrence_id),
                },
                action: RecurrenceExceptionAction::Skip,
            }),
            _ => return Err(PublishedHabitEvidenceError::Invalid),
        }
    }
    let missed_rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {EVIDENCE_COLUMNS}, item.revision AS current_item_revision, \
         item.timezone_name AS current_timezone_name, item.recurrence AS current_recurrence, \
         item.scheduling_constraints AS current_constraints, \
         item.duration_seconds AS current_duration_seconds, item.duration_kind AS current_duration_kind, \
         item.duration_min_seconds AS current_duration_min_seconds, \
         item.duration_max_seconds AS current_duration_max_seconds, \
         item.duration_source AS current_duration_source, item.split_allowed AS current_split_allowed, \
         item.minimum_chunk_seconds AS current_minimum_chunk_seconds, \
         item.maximum_chunk_seconds AS current_maximum_chunk_seconds, \
         resolution.revision AS missed_resolution_revision, \
         resolution.source_planner_occurrence_id AS missed_source_planner_occurrence_id, \
         resolution.configured_policy AS missed_configured_policy, \
         resolution.action AS missed_action, resolution.cancellation_reason, \
         resolution.cancelled_resume_action, resolution.carry_window_start, \
         resolution.carry_window_end, resolution.suppressed_planner_occurrence_ids, \
         resolution.created_at AS missed_created_at, resolution.updated_at AS missed_updated_at, \
         source_outcome.status AS missed_source_outcome_status, \
         EXISTS (SELECT 1 FROM habit_effective_reduction_targets( \
           evidence.workspace_id, evidence.habit_id, evidence.policy_fingerprint) \
           WHERE planner_occurrence_id = evidence.planner_occurrence_id) \
           AS missed_source_is_effectively_suppressed, \
         CASE WHEN resolution.action = 'reduce_frequency' THEN EXISTS ( \
           SELECT 1 FROM habit_effective_reduction_targets( \
             evidence.workspace_id, evidence.habit_id, evidence.policy_fingerprint) \
           WHERE planner_occurrence_id = resolution.suppressed_planner_occurrence_id) \
         ELSE false END AS missed_reduction_is_effective, \
         EXISTS (SELECT 1 FROM habit_pauses source_pause \
           WHERE source_pause.workspace_id = evidence.workspace_id \
             AND source_pause.habit_id = evidence.habit_id \
             AND source_pause.started_at < CASE WHEN resolution.action = 'carry' \
               THEN resolution.carry_window_end ELSE evidence.window_end END \
             AND (source_pause.ended_at IS NULL OR source_pause.ended_at > \
               CASE WHEN resolution.action = 'carry' \
                 THEN resolution.carry_window_start ELSE evidence.window_start END)) \
           AS missed_source_paused, \
         CASE WHEN resolution.action = 'reduce_frequency' THEN EXISTS ( \
           SELECT 1 FROM habit_occurrence_evidence target \
           LEFT JOIN habit_occurrence_outcomes target_outcome \
             ON target_outcome.workspace_id = target.workspace_id \
            AND target_outcome.occurrence_evidence_id = target.id \
           WHERE target.workspace_id = evidence.workspace_id \
             AND target.habit_id = evidence.habit_id \
             AND target.planner_occurrence_id = resolution.suppressed_planner_occurrence_id \
             AND ( \
               (target_outcome.status IS NOT NULL AND target_outcome.status <> 'unresolved') \
               OR EXISTS (SELECT 1 FROM habit_pauses target_pause \
                 WHERE target_pause.workspace_id = target.workspace_id \
                   AND target_pause.habit_id = target.habit_id \
                   AND target_pause.started_at < target.window_end \
                   AND (target_pause.ended_at IS NULL \
                     OR target_pause.ended_at > target.window_start)) \
               OR EXISTS (SELECT 1 FROM schedule_revisions current_revision \
                 WHERE current_revision.workspace_id = target.workspace_id \
                   AND current_revision.state = 'published' \
                   AND current_revision.horizon_start <= target.window_start \
                   AND current_revision.horizon_end >= target.window_end \
                   AND NOT EXISTS (SELECT 1 FROM habit_occurrence_publications publication \
                     WHERE publication.workspace_id = target.workspace_id \
                       AND publication.schedule_revision_id = current_revision.id \
                       AND publication.occurrence_evidence_id = target.id \
                       AND publication.occurrence_state IN ('generated', 'skipped'))))) \
         ELSE false END AS missed_target_ineligible \
         FROM habit_occurrence_evidence evidence JOIN habit_missed_resolutions resolution \
           ON resolution.workspace_id = evidence.workspace_id \
          AND resolution.occurrence_evidence_id = evidence.id \
         LEFT JOIN habit_occurrence_outcomes source_outcome \
           ON source_outcome.workspace_id = evidence.workspace_id \
          AND source_outcome.occurrence_evidence_id = evidence.id \
         JOIN items item ON item.workspace_id = evidence.workspace_id AND item.id = evidence.habit_id \
         WHERE evidence.workspace_id = $1 AND item.kind = 'habit' \
           AND item.recurrence IS NOT NULL AND item.trashed_at IS NULL \
           AND item.status NOT IN ('completed', 'skipped', 'cancelled', 'blocked') \
           AND NOT EXISTS (SELECT 1 FROM item_hierarchy child_edge \
             JOIN items child ON child.workspace_id = child_edge.workspace_id \
               AND child.id = child_edge.child_item_id \
             WHERE child_edge.workspace_id = item.workspace_id \
               AND child_edge.parent_item_id = item.id AND child.trashed_at IS NULL) \
           AND ((evidence.window_start < $3 AND evidence.window_end > $2) \
             OR (resolution.action = 'carry' AND resolution.carry_window_start < $3 \
                 AND resolution.carry_window_end > $2) \
             OR evidence.planner_occurrence_id = ANY($4) \
             OR resolution.suppressed_planner_occurrence_id = ANY($4) \
             OR EXISTS (SELECT 1 FROM habit_occurrence_evidence target \
                 WHERE target.workspace_id = evidence.workspace_id \
                   AND target.habit_id = evidence.habit_id \
                   AND target.planner_occurrence_id = resolution.suppressed_planner_occurrence_id \
                   AND target.window_start < $3 AND target.window_end > $2)) \
         ORDER BY evidence.habit_id, evidence.nominal_start, evidence.recurrence_ordinal, evidence.id"
    )))
    .bind(workspace_id)
    .bind(horizon_start)
    .bind(horizon_end)
    .bind(moved_occurrence_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    // A reduction only suppresses its target while the reduction source and
    // target are both currently applicable. Process immutable source order so
    // a suppressed occurrence cannot cascade its own stale reduction action.
    // Reduction targets are strictly later occurrences by domain and schema
    // validation, making this a deterministic forward pass.
    let mut effective_reduction_targets = BTreeSet::new();
    for row in missed_rows {
        let evidence = authoritative_evidence_from_row(&row)?;
        let revision: i64 = row
            .try_get("missed_resolution_revision")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let resolution = missed_resolution_from_row(&row, revision)
            .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
        if resolution.occurrence_evidence_id != evidence.id
            || resolution.habit_id != evidence.habit_id
            || resolution.source_planner_occurrence_id != evidence.planner_occurrence_id
        {
            return Err(PublishedHabitEvidenceError::Invalid);
        }
        let source_outcome_status: Option<String> = row
            .try_get("missed_source_outcome_status")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let source_paused: bool = row
            .try_get("missed_source_paused")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let source_is_effectively_suppressed: bool = row
            .try_get("missed_source_is_effectively_suppressed")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let current_policy_fingerprint = current_policy_fingerprint_from_row(&row)?;
        if matches!(
            source_outcome_status.as_deref(),
            Some("completed" | "skipped")
        ) || source_paused
            || source_is_effectively_suppressed
            || evidence.policy_fingerprint != prefixed_hex(&current_policy_fingerprint)
            || effective_reduction_targets
                .contains(&(evidence.habit_id, evidence.planner_occurrence_id))
        {
            continue;
        }
        match &resolution.action {
            HabitMissedResolutionAction::DecisionRequired
            | HabitMissedResolutionAction::ReductionPending
            | HabitMissedResolutionAction::Cancelled { .. } => {}
            HabitMissedResolutionAction::Skip => {
                context
                    .partial_progress
                    .remove(&OccurrenceId(evidence.planner_occurrence_id));
                push_unique_skip(
                    &mut context,
                    ItemId(evidence.habit_id),
                    OccurrenceId(evidence.planner_occurrence_id),
                );
            }
            HabitMissedResolutionAction::Carry {
                window_start,
                window_end,
            } => {
                context.exceptions.retain(|exception| {
                    !(exception.item_id == ItemId(evidence.habit_id)
                        && matches!(
                            exception.selector,
                            RecurrenceExceptionSelector::Occurrence { id }
                                if id == OccurrenceId(evidence.planner_occurrence_id)
                        ))
                });
                if horizon_start <= *window_start && *window_end <= horizon_end {
                    let current_item_revision: i64 = row
                        .try_get("current_item_revision")
                        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
                    let source = recurrence_move_source(&evidence, current_item_revision)?;
                    context.exceptions.push(RecurrenceException {
                        item_id: ItemId(evidence.habit_id),
                        selector: RecurrenceExceptionSelector::Occurrence {
                            id: OccurrenceId(evidence.planner_occurrence_id),
                        },
                        action: RecurrenceExceptionAction::Move {
                            start: chrono_to_offset(*window_start)?,
                            end: chrono_to_offset(*window_end)?,
                            source,
                        },
                    });
                } else {
                    context
                        .partial_progress
                        .remove(&OccurrenceId(evidence.planner_occurrence_id));
                    push_unique_skip(
                        &mut context,
                        ItemId(evidence.habit_id),
                        OccurrenceId(evidence.planner_occurrence_id),
                    );
                }
            }
            HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids,
            } => {
                let target_ineligible: bool = row
                    .try_get("missed_target_ineligible")
                    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
                let reduction_is_effective: bool = row
                    .try_get("missed_reduction_is_effective")
                    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
                if target_ineligible || !reduction_is_effective {
                    continue;
                }
                for occurrence_id in suppressed_planner_occurrence_ids {
                    effective_reduction_targets.insert((evidence.habit_id, *occurrence_id));
                    context
                        .partial_progress
                        .remove(&OccurrenceId(*occurrence_id));
                    push_unique_skip(
                        &mut context,
                        ItemId(evidence.habit_id),
                        OccurrenceId(*occurrence_id),
                    );
                }
            }
        }
    }
    let anchors = sqlx::query(AssertSqlSafe(format!(
        "SELECT DISTINCT ON (evidence.habit_id) {EVIDENCE_COLUMNS}, outcome.occurred_at \
         FROM habit_occurrence_evidence evidence JOIN habit_occurrence_outcomes outcome \
           ON outcome.workspace_id = evidence.workspace_id \
          AND outcome.occurrence_evidence_id = evidence.id \
         JOIN items item ON item.workspace_id = evidence.workspace_id AND item.id = evidence.habit_id \
         WHERE evidence.workspace_id = $1 AND item.kind = 'habit' AND item.trashed_at IS NULL \
           AND outcome.status = 'completed' \
         ORDER BY evidence.habit_id, outcome.occurred_at DESC, evidence.id DESC"
    )))
    .bind(workspace_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    for row in anchors {
        let evidence = authoritative_evidence_from_row(&row)?;
        let occurred_at: DateTime<Utc> = row
            .try_get("occurred_at")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        context.completion_anchors.insert(
            ItemId(evidence.habit_id),
            chrono_to_offset(occurred_at).map_err(|_| PublishedHabitEvidenceError::Invalid)?,
        );
    }
    let pauses = sqlx::query(
        "SELECT pause.habit_id, pause.started_at, pause.ended_at \
         FROM habit_pauses pause JOIN items item \
           ON item.workspace_id = pause.workspace_id AND item.id = pause.habit_id \
         WHERE pause.workspace_id = $1 AND item.kind = 'habit' AND item.trashed_at IS NULL \
           AND pause.started_at < $3 AND (pause.ended_at IS NULL OR pause.ended_at > $2) \
         ORDER BY pause.habit_id, pause.started_at, pause.id",
    )
    .bind(workspace_id)
    .bind(horizon_start)
    .bind(horizon_end)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    for row in pauses {
        let habit_id: Uuid = row
            .try_get("habit_id")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let start: DateTime<Utc> = row
            .try_get("started_at")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let end: Option<DateTime<Utc>> = row
            .try_get("ended_at")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let start = start.max(horizon_start);
        let end = end.unwrap_or(horizon_end).min(horizon_end);
        if start < end {
            context.pauses.push(RecurrencePause {
                item_id: ItemId(habit_id),
                start: chrono_to_offset(start).map_err(|_| PublishedHabitEvidenceError::Invalid)?,
                end: chrono_to_offset(end).map_err(|_| PublishedHabitEvidenceError::Invalid)?,
            });
        }
    }
    Ok(AuthoritativeHabitRecurrence {
        change_head,
        context,
    })
}

/// Records immutable occurrence evidence inside the schedule publication transaction.
/// An arbitrary client UUID can therefore never become an outcome target.
#[allow(clippy::too_many_lines)] // Policy hydration and all immutable evidence checks share the publication transaction.
pub(crate) async fn record_published_habit_occurrences_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    schedule_revision_id: Uuid,
    result: &ComposeScheduleResult,
    published_at: DateTime<Utc>,
) -> Result<(), PublishedHabitEvidenceError> {
    let ids = result
        .plan
        .occurrences
        .iter()
        .map(|occurrence| occurrence.series_item_id.0)
        .collect::<BTreeSet<_>>();
    if ids.is_empty() {
        return Ok(());
    }
    let ids = ids.into_iter().collect::<Vec<_>>();
    let rows = sqlx::query(
        "SELECT id, kind, revision, timezone_name, recurrence, scheduling_constraints, \
         duration_seconds, duration_kind, duration_min_seconds, duration_max_seconds, duration_source, \
         split_allowed, minimum_chunk_seconds, maximum_chunk_seconds \
         FROM items WHERE workspace_id = $1 AND id = ANY($2) AND trashed_at IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let mut policies = BTreeMap::new();
    for row in rows {
        let id: Uuid = row
            .try_get("id")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let kind: String = row
            .try_get("kind")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        if kind != "habit" {
            continue;
        }
        let revision: i64 = row
            .try_get("revision")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let source_revision =
            u64::try_from(revision).map_err(|_| PublishedHabitEvidenceError::Invalid)?;
        if result.source_item_revisions.get(&id) != Some(&source_revision) {
            return Err(PublishedHabitEvidenceError::Invalid);
        }
        let recurrence: Value = row
            .try_get::<Option<Value>, _>("recurrence")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?
            .ok_or(PublishedHabitEvidenceError::Invalid)?;
        let constraints: Value = row
            .try_get("scheduling_constraints")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let timezone_name: String = row
            .try_get("timezone_name")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let duration_seconds: Option<i32> = row
            .try_get("duration_seconds")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let policy = json!({
            "schema":"dayweave-habit-policy/1",
            "habit_id":id,
            "timezone_name":timezone_name,
            "recurrence":recurrence,
            "constraints":constraints,
            "duration":{
                "kind":row.try_get::<String,_>("duration_kind").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
                "seconds":duration_seconds,
                "minimum_seconds":row.try_get::<Option<i32>,_>("duration_min_seconds").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
                "maximum_seconds":row.try_get::<Option<i32>,_>("duration_max_seconds").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
                "source":row.try_get::<Option<String>,_>("duration_source").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            },
            "split":{
                "allowed":row.try_get::<bool,_>("split_allowed").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
                "minimum_seconds":row.try_get::<Option<i32>,_>("minimum_chunk_seconds").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
                "maximum_seconds":row.try_get::<Option<i32>,_>("maximum_chunk_seconds").map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            }
        });
        let policy_bytes =
            serde_json::to_vec(&policy).map_err(|_| PublishedHabitEvidenceError::Invalid)?;
        let (expected_quantity, expected_unit) = match (
            constraints.pointer("/habit_target/amount"),
            constraints.pointer("/habit_target/unit"),
        ) {
            (None, None) => (None, None),
            (Some(amount), Some(unit)) => {
                let amount = amount
                    .as_u64()
                    .and_then(|amount| i64::try_from(amount).ok())
                    .filter(|amount| (1..=MAX_HABIT_QUANTITY).contains(amount))
                    .ok_or(PublishedHabitEvidenceError::Invalid)?;
                let unit = unit
                    .as_str()
                    .filter(|unit| is_valid_habit_quantity_unit(unit))
                    .ok_or(PublishedHabitEvidenceError::Invalid)?
                    .to_owned();
                (Some(amount), Some(unit))
            }
            _ => return Err(PublishedHabitEvidenceError::Invalid),
        };
        policies.insert(
            id,
            PublishedHabitPolicy {
                source_revision,
                fingerprint: Sha256::digest(policy_bytes).into(),
                timezone_name,
                expected_duration_seconds: duration_seconds
                    .map(|value| {
                        u64::try_from(value).map_err(|_| PublishedHabitEvidenceError::Invalid)
                    })
                    .transpose()?,
                expected_quantity,
                expected_unit,
                is_sensitive: result
                    .source_item_sensitivity
                    .get(&id)
                    .copied()
                    .ok_or(PublishedHabitEvidenceError::Invalid)?,
            },
        );
    }
    for occurrence in &result.plan.occurrences {
        let habit_id = occurrence.series_item_id.0;
        let Some(policy) = policies.get(&habit_id) else {
            continue;
        };
        insert_published_occurrence(
            tx,
            scope.workspace_id,
            schedule_revision_id,
            occurrence,
            policy,
            published_at,
        )
        .await?;
    }
    Ok(())
}

struct PublishedHabitPolicy {
    source_revision: u64,
    fingerprint: [u8; 32],
    timezone_name: String,
    expected_duration_seconds: Option<u64>,
    expected_quantity: Option<i64>,
    expected_unit: Option<String>,
    is_sensitive: bool,
}

#[allow(clippy::too_many_lines)] // Admission, conflict verification, delta creation, and provenance remain atomic.
async fn insert_published_occurrence(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    schedule_revision_id: Uuid,
    occurrence: &Occurrence,
    policy: &PublishedHabitPolicy,
    published_at: DateTime<Utc>,
) -> Result<(), PublishedHabitEvidenceError> {
    let identity = serde_json::to_value(occurrence.identity)
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    if serde_json::to_vec(&identity)
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?
        .len()
        > MAX_RECURRENCE_IDENTITY_BYTES
    {
        return Err(PublishedHabitEvidenceError::Invalid);
    }
    let nominal_start = offset_to_chrono(occurrence.nominal_start)?;
    let nominal_end = offset_to_chrono(occurrence.nominal_end)?;
    let window_start = offset_to_chrono(occurrence.window_start)?;
    let window_end = offset_to_chrono(occurrence.window_end)?;
    let local_date =
        occurrence_local_date(occurrence.local_date, nominal_start, &policy.timezone_name)?;
    let evidence_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM habit_occurrence_evidence WHERE workspace_id = $1 \
         AND habit_id = $2 AND planner_occurrence_id = $3)",
    )
    .bind(workspace_id)
    .bind(occurrence.series_item_id.0)
    .bind(occurrence.id.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    if nominal_end <= nominal_start
        || window_end <= window_start
        || (!evidence_exists && (nominal_start < window_start || nominal_end > window_end))
    {
        return Err(PublishedHabitEvidenceError::Invalid);
    }
    let evidence_id = Uuid::new_v4();
    let evidence = HabitOccurrenceEvidence {
        id: evidence_id,
        habit_id: occurrence.series_item_id.0,
        planner_occurrence_id: occurrence.id.0,
        source_schedule_revision_id: schedule_revision_id,
        source_item_revision: policy.source_revision,
        policy_fingerprint: prefixed_hex(&policy.fingerprint),
        identity: identity.clone(),
        nominal_start,
        nominal_end,
        window_start,
        window_end,
        local_date,
        timezone_name: policy.timezone_name.clone(),
        expected_duration_seconds: policy.expected_duration_seconds,
        expected_quantity: policy.expected_quantity,
        expected_unit: policy.expected_unit.clone(),
    };
    if !evidence_exists {
        evidence
            .validate()
            .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    }
    let inserted = if evidence_exists {
        false
    } else {
        sqlx::query(
        "INSERT INTO habit_occurrence_evidence (id, workspace_id, habit_id, planner_occurrence_id, \
         source_schedule_revision_id, source_item_revision, policy_fingerprint, recurrence_identity, \
         nominal_start, nominal_end, window_start, window_end, local_date, timezone_name, \
         expected_duration_seconds, expected_quantity, expected_unit, is_sensitive, created_at, last_published_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$19) \
         ON CONFLICT (workspace_id, habit_id, planner_occurrence_id) DO NOTHING",
    )
    .bind(evidence_id)
    .bind(workspace_id)
    .bind(occurrence.series_item_id.0)
    .bind(occurrence.id.0)
    .bind(schedule_revision_id)
    .bind(i64::try_from(policy.source_revision).map_err(|_| PublishedHabitEvidenceError::Invalid)?)
    .bind(policy.fingerprint.as_slice())
    .bind(&identity)
    .bind(nominal_start)
    .bind(nominal_end)
    .bind(window_start)
    .bind(window_end)
    .bind(local_date)
    .bind(&policy.timezone_name)
    .bind(policy.expected_duration_seconds.map(i64::try_from).transpose().map_err(|_| PublishedHabitEvidenceError::Invalid)?)
    .bind(policy.expected_quantity)
    .bind(&policy.expected_unit)
    .bind(policy.is_sensitive)
        .bind(published_at)
        .execute(&mut **tx)
        .await
        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?
        .rows_affected()
            == 1
    };
    if inserted {
        let change = HabitDeltaChange::OccurrenceUpsert {
            occurrence: HabitOccurrence {
                evidence,
                outcome: None,
                missed_resolution: None,
            },
        };
        insert_change(
            tx,
            workspace_id,
            "occurrence_upsert",
            evidence_id,
            1,
            &change,
            published_at,
        )
        .await
        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        record_occurrence_publication(
            tx,
            workspace_id,
            schedule_revision_id,
            evidence_id,
            policy.source_revision,
            occurrence.state,
            published_at,
        )
        .await?;
        return Ok(());
    }
    let existing = sqlx::query(AssertSqlSafe(format!(
        "SELECT {EVIDENCE_COLUMNS}, (policy_fingerprint = $4 \
         AND recurrence_identity = $5 AND nominal_start = $6 \
         AND nominal_end = $7 AND local_date = $10 \
         AND timezone_name = $11 AND expected_duration_seconds IS NOT DISTINCT FROM $12 \
         AND expected_quantity IS NOT DISTINCT FROM $13 \
         AND expected_unit IS NOT DISTINCT FROM $14) AS base_content_matches, \
         (evidence.window_start = $8 AND evidence.window_end = $9) AS window_matches, \
         resolution.action AS missed_action, resolution.carry_window_start, \
         resolution.carry_window_end \
         FROM habit_occurrence_evidence evidence \
         LEFT JOIN habit_missed_resolutions resolution \
           ON resolution.workspace_id = evidence.workspace_id \
          AND resolution.occurrence_evidence_id = evidence.id \
         WHERE evidence.workspace_id = $1 AND evidence.habit_id = $2 \
           AND evidence.planner_occurrence_id = $3"
    )))
    .bind(workspace_id)
    .bind(occurrence.series_item_id.0)
    .bind(occurrence.id.0)
    .bind(policy.fingerprint.as_slice())
    .bind(identity)
    .bind(nominal_start)
    .bind(nominal_end)
    .bind(window_start)
    .bind(window_end)
    .bind(local_date)
    .bind(&policy.timezone_name)
    .bind(
        policy
            .expected_duration_seconds
            .map(i64::try_from)
            .transpose()
            .map_err(|_| PublishedHabitEvidenceError::Invalid)?,
    )
    .bind(policy.expected_quantity)
    .bind(&policy.expected_unit)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    // A matching unique key may have been written by an older binary or a
    // privileged operator. Equality alone cannot promote malformed stored
    // evidence into trusted history, so hydrate the complete row first.
    let existing_evidence = authoritative_evidence_from_row(&existing)
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let base_matches: bool = existing
        .try_get("base_content_matches")
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let window_matches: bool = existing
        .try_get("window_matches")
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let missed_action: Option<String> = existing
        .try_get("missed_action")
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let carry_start: Option<DateTime<Utc>> = existing
        .try_get("carry_window_start")
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let carry_end: Option<DateTime<Utc>> = existing
        .try_get("carry_window_end")
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let authorized_carry = missed_action.as_deref() == Some("carry")
        && carry_start == Some(window_start)
        && carry_end == Some(window_end);
    if !base_matches || (!window_matches && !authorized_carry) {
        return Err(PublishedHabitEvidenceError::Conflict);
    }
    record_occurrence_publication(
        tx,
        workspace_id,
        schedule_revision_id,
        existing_evidence.id,
        policy.source_revision,
        occurrence.state,
        published_at,
    )
    .await?;
    // An exact re-publication is intentionally a storage no-op. In
    // particular, read/preview refreshes must not manufacture a habit change
    // or advance an observation timestamp merely because the schedule content
    // was revalidated. The first admitted publication remains the immutable
    // provenance for this occurrence.
    Ok(())
}

async fn record_occurrence_publication(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    schedule_revision_id: Uuid,
    occurrence_evidence_id: Uuid,
    item_revision: u64,
    state: OccurrenceState,
    recorded_at: DateTime<Utc>,
) -> Result<(), PublishedHabitEvidenceError> {
    let state = match state {
        OccurrenceState::Generated => "generated",
        OccurrenceState::Completed => "completed",
        OccurrenceState::Paused => "paused",
        OccurrenceState::Skipped => "skipped",
    };
    sqlx::query(
        "INSERT INTO habit_occurrence_publications (workspace_id, schedule_revision_id, \
         occurrence_evidence_id, item_revision, occurrence_state, recorded_at) \
         VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
    )
    .bind(workspace_id)
    .bind(schedule_revision_id)
    .bind(occurrence_evidence_id)
    .bind(i64::try_from(item_revision).map_err(|_| PublishedHabitEvidenceError::Invalid)?)
    .bind(state)
    .bind(recorded_at)
    .execute(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let matches: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM habit_occurrence_publications \
         WHERE workspace_id = $1 AND schedule_revision_id = $2 \
           AND occurrence_evidence_id = $3 AND item_revision = $4 \
           AND occurrence_state = $5)",
    )
    .bind(workspace_id)
    .bind(schedule_revision_id)
    .bind(occurrence_evidence_id)
    .bind(i64::try_from(item_revision).map_err(|_| PublishedHabitEvidenceError::Invalid)?)
    .bind(state)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    if matches {
        Ok(())
    } else {
        Err(PublishedHabitEvidenceError::Conflict)
    }
}

fn occurrence_local_date(
    explicit: Option<time::Date>,
    nominal_start: DateTime<Utc>,
    timezone_name: &str,
) -> Result<NaiveDate, PublishedHabitEvidenceError> {
    // Parse even when the core supplied a local date: every evidence row must
    // retain a valid IANA zone so its wall-clock snapshot remains explainable.
    let timezone = timezone_name
        .parse::<chrono_tz::Tz>()
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    explicit
        .map(|date| {
            date.to_string()
                .parse::<NaiveDate>()
                .map_err(|_| PublishedHabitEvidenceError::Invalid)
        })
        .transpose()
        .map(|date| date.unwrap_or_else(|| nominal_start.with_timezone(&timezone).date_naive()))
}

fn offset_to_chrono(
    value: time::OffsetDateTime,
) -> Result<DateTime<Utc>, PublishedHabitEvidenceError> {
    DateTime::from_timestamp(value.unix_timestamp(), value.nanosecond())
        .map(|value| DateTime::from_timestamp_micros(value.timestamp_micros()).unwrap_or(value))
        .ok_or(PublishedHabitEvidenceError::Invalid)
}

fn chrono_to_offset(
    value: DateTime<Utc>,
) -> Result<time::OffsetDateTime, PublishedHabitEvidenceError> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(value.timestamp_micros()) * 1_000)
        .map_err(|_| PublishedHabitEvidenceError::Invalid)
}

fn current_policy_fingerprint_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<[u8; 32], PublishedHabitEvidenceError> {
    let habit_id: Uuid = row
        .try_get("habit_id")
        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let timezone_name: String = row
        .try_get("current_timezone_name")
        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let recurrence: Value = row
        .try_get::<Option<Value>, _>("current_recurrence")
        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?
        .ok_or(PublishedHabitEvidenceError::Invalid)?;
    let constraints: Value = row
        .try_get("current_constraints")
        .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let policy = json!({
        "schema":"dayweave-habit-policy/1",
        "habit_id":habit_id,
        "timezone_name":timezone_name,
        "recurrence":recurrence,
        "constraints":constraints,
        "duration":{
            "kind":row.try_get::<String,_>("current_duration_kind")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            "seconds":row.try_get::<Option<i32>,_>("current_duration_seconds")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            "minimum_seconds":row.try_get::<Option<i32>,_>("current_duration_min_seconds")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            "maximum_seconds":row.try_get::<Option<i32>,_>("current_duration_max_seconds")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            "source":row.try_get::<Option<String>,_>("current_duration_source")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
        },
        "split":{
            "allowed":row.try_get::<bool,_>("current_split_allowed")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            "minimum_seconds":row.try_get::<Option<i32>,_>("current_minimum_chunk_seconds")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
            "maximum_seconds":row.try_get::<Option<i32>,_>("current_maximum_chunk_seconds")
                .map_err(|_| PublishedHabitEvidenceError::Unavailable)?,
        }
    });
    let bytes = serde_json::to_vec(&policy).map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    Ok(Sha256::digest(bytes).into())
}

fn recurrence_move_source(
    evidence: &HabitOccurrenceEvidence,
    current_item_revision: i64,
) -> Result<RecurrenceMoveSource, PublishedHabitEvidenceError> {
    let identity: RecurrenceOccurrenceIdentity = serde_json::from_value(evidence.identity.clone())
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let ordinal = recurrence_identity_ordinal(&evidence.identity)
        .ok_or(PublishedHabitEvidenceError::Invalid)?;
    let month = u8::try_from(evidence.local_date.month())
        .ok()
        .and_then(|month| time::Month::try_from(month).ok())
        .ok_or(PublishedHabitEvidenceError::Invalid)?;
    let local_date = Some(
        time::Date::from_calendar_date(
            evidence.local_date.year(),
            month,
            u8::try_from(evidence.local_date.day())
                .map_err(|_| PublishedHabitEvidenceError::Invalid)?,
        )
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?,
    );
    Ok(RecurrenceMoveSource {
        item_revision: u64::try_from(current_item_revision)
            .map_err(|_| PublishedHabitEvidenceError::Invalid)?,
        identity,
        nominal_start: chrono_to_offset_in_timezone(
            evidence.nominal_start,
            &evidence.timezone_name,
        )?,
        nominal_end: chrono_to_offset_in_timezone(evidence.nominal_end, &evidence.timezone_name)?,
        local_date,
        ordinal,
    })
}

fn chrono_to_offset_in_timezone(
    value: DateTime<Utc>,
    timezone_name: &str,
) -> Result<time::OffsetDateTime, PublishedHabitEvidenceError> {
    let timezone = timezone_name
        .parse::<chrono_tz::Tz>()
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    let offset_seconds = value
        .with_timezone(&timezone)
        .offset()
        .fix()
        .local_minus_utc();
    let offset = time::UtcOffset::from_whole_seconds(offset_seconds)
        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
    chrono_to_offset(value).map(|value| value.to_offset(offset))
}

fn push_unique_skip(context: &mut RecurrenceContext, item_id: ItemId, occurrence_id: OccurrenceId) {
    let exists = context.exceptions.iter().any(|exception| {
        exception.item_id == item_id
            && matches!(
                (exception.selector, exception.action),
                (
                    RecurrenceExceptionSelector::Occurrence { id },
                    RecurrenceExceptionAction::Skip
                ) if id == occurrence_id
            )
    });
    if !exists {
        context.exceptions.push(RecurrenceException {
            item_id,
            selector: RecurrenceExceptionSelector::Occurrence { id: occurrence_id },
            action: RecurrenceExceptionAction::Skip,
        });
    }
}

fn outcome_status_name(status: crate::habits::HabitOutcomeStatus) -> &'static str {
    match status {
        crate::habits::HabitOutcomeStatus::Unresolved => "unresolved",
        crate::habits::HabitOutcomeStatus::Partial => "partial",
        crate::habits::HabitOutcomeStatus::Completed => "completed",
        crate::habits::HabitOutcomeStatus::Skipped => "skipped",
    }
}

fn parse_outcome_status(
    value: &str,
) -> Result<crate::habits::HabitOutcomeStatus, HabitRepositoryError> {
    match value {
        "unresolved" => Ok(crate::habits::HabitOutcomeStatus::Unresolved),
        "partial" => Ok(crate::habits::HabitOutcomeStatus::Partial),
        "completed" => Ok(crate::habits::HabitOutcomeStatus::Completed),
        "skipped" => Ok(crate::habits::HabitOutcomeStatus::Skipped),
        _ => Err(HabitRepositoryError::Internal),
    }
}

const fn missed_policy_name(policy: HabitMissedPolicy) -> &'static str {
    match policy {
        HabitMissedPolicy::Skip => "skip",
        HabitMissedPolicy::Carry => "carry",
        HabitMissedPolicy::ReduceFrequency => "reduce_frequency",
        HabitMissedPolicy::Ask => "ask",
    }
}

fn parse_missed_policy(value: &str) -> Result<HabitMissedPolicy, HabitRepositoryError> {
    match value {
        "skip" => Ok(HabitMissedPolicy::Skip),
        "carry" => Ok(HabitMissedPolicy::Carry),
        "reduce_frequency" => Ok(HabitMissedPolicy::ReduceFrequency),
        "ask" => Ok(HabitMissedPolicy::Ask),
        _ => Err(HabitRepositoryError::Internal),
    }
}

fn missed_action_name(action: &HabitMissedResolutionAction) -> &'static str {
    match action {
        HabitMissedResolutionAction::DecisionRequired => "decision_required",
        HabitMissedResolutionAction::ReductionPending => "reduction_pending",
        HabitMissedResolutionAction::Cancelled { .. } => "cancelled",
        HabitMissedResolutionAction::Skip => "skip",
        HabitMissedResolutionAction::Carry { .. } => "carry",
        HabitMissedResolutionAction::ReduceFrequency { .. } => "reduce_frequency",
    }
}

const fn missed_cancellation_reason_name(reason: HabitMissedCancellationReason) -> &'static str {
    match reason {
        HabitMissedCancellationReason::SourceCompleted => "source_completed",
        HabitMissedCancellationReason::SourceSkipped => "source_skipped",
        HabitMissedCancellationReason::SourcePaused => "source_paused",
        HabitMissedCancellationReason::SourceObsolete => "source_obsolete",
    }
}

fn parse_missed_cancellation_reason(
    value: &str,
) -> Result<HabitMissedCancellationReason, HabitRepositoryError> {
    match value {
        "source_completed" => Ok(HabitMissedCancellationReason::SourceCompleted),
        "source_skipped" => Ok(HabitMissedCancellationReason::SourceSkipped),
        "source_paused" => Ok(HabitMissedCancellationReason::SourcePaused),
        "source_obsolete" => Ok(HabitMissedCancellationReason::SourceObsolete),
        _ => Err(HabitRepositoryError::Internal),
    }
}

const fn missed_resume_action_name(action: HabitMissedResumeAction) -> &'static str {
    match action {
        HabitMissedResumeAction::DecisionRequired => "decision_required",
        HabitMissedResumeAction::Skip => "skip",
        HabitMissedResumeAction::Carry => "carry",
        HabitMissedResumeAction::ReduceFrequency => "reduce_frequency",
    }
}

fn parse_missed_resume_action(
    value: &str,
) -> Result<HabitMissedResumeAction, HabitRepositoryError> {
    match value {
        "decision_required" => Ok(HabitMissedResumeAction::DecisionRequired),
        "skip" => Ok(HabitMissedResumeAction::Skip),
        "carry" => Ok(HabitMissedResumeAction::Carry),
        "reduce_frequency" => Ok(HabitMissedResumeAction::ReduceFrequency),
        _ => Err(HabitRepositoryError::Internal),
    }
}

fn parse_missed_action(
    value: &str,
    cancellation_reason: Option<&str>,
    cancelled_resume_action: Option<&str>,
    carry_start: Option<DateTime<Utc>>,
    carry_end: Option<DateTime<Utc>>,
    suppressed: Vec<Uuid>,
) -> Result<HabitMissedResolutionAction, HabitRepositoryError> {
    match (
        value,
        cancellation_reason,
        cancelled_resume_action,
        carry_start,
        carry_end,
        suppressed.as_slice(),
    ) {
        ("decision_required", None, None, None, None, []) => {
            Ok(HabitMissedResolutionAction::DecisionRequired)
        }
        ("reduction_pending", None, None, None, None, []) => {
            Ok(HabitMissedResolutionAction::ReductionPending)
        }
        ("skip", None, None, None, None, []) => Ok(HabitMissedResolutionAction::Skip),
        ("carry", None, None, Some(window_start), Some(window_end), []) => {
            Ok(HabitMissedResolutionAction::Carry {
                window_start,
                window_end,
            })
        }
        ("reduce_frequency", None, None, None, None, [_]) => {
            Ok(HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids: suppressed,
            })
        }
        ("cancelled", Some(reason), Some(resume_action), None, None, []) => {
            Ok(HabitMissedResolutionAction::Cancelled {
                reason: parse_missed_cancellation_reason(reason)?,
                resume_action: parse_missed_resume_action(resume_action)?,
            })
        }
        _ => Err(HabitRepositoryError::Internal),
    }
}

fn prefixed_hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(7 + bytes.len() * 2);
    value.push_str("sha256:");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn to_i64(value: u64) -> Result<i64, HabitRepositoryError> {
    i64::try_from(value).map_err(|_| HabitRepositoryError::Internal)
}

fn from_i64(value: i64) -> Result<u64, HabitRepositoryError> {
    u64::try_from(value).map_err(|_| HabitRepositoryError::Internal)
}

fn storage(_: sqlx::Error) -> HabitRepositoryError {
    HabitRepositoryError::Internal
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::{PublishedHabitEvidenceError, occurrence_local_date};

    #[test]
    fn fallback_local_date_is_stable_across_a_dst_fold() {
        let first_fold = Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap();
        let second_fold = Utc.with_ymd_and_hms(2026, 10, 25, 1, 30, 0).unwrap();

        let first = occurrence_local_date(None, first_fold, "Europe/Paris").unwrap();
        let second = occurrence_local_date(None, second_fold, "Europe/Paris").unwrap();

        assert_eq!(first.to_string(), "2026-10-25");
        assert_eq!(second, first);
    }

    #[test]
    fn local_date_requires_a_valid_iana_zone_even_when_explicit() {
        let explicit = time::Date::from_calendar_date(2026, time::Month::September, 4).unwrap();
        let at = Utc.with_ymd_and_hms(2026, 9, 4, 12, 0, 0).unwrap();

        assert_eq!(
            occurrence_local_date(Some(explicit), at, "not/a-zone"),
            Err(PublishedHabitEvidenceError::Invalid)
        );
    }
}
