use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use dayweave_core::{
    ItemId, Minutes, Occurrence, OccurrenceId, RecurrenceContext, RecurrenceException,
    RecurrenceExceptionAction, RecurrenceExceptionSelector, RecurrencePartialProgress,
    RecurrencePause, is_valid_habit_quantity_unit,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    habits::{
        HabitDeltaChange, HabitDeltaPage, HabitIdempotency, HabitMutation, HabitOccurrence,
        HabitOccurrenceEvidence, HabitOutcome, HabitPause, HabitRepository, HabitRepositoryError,
        MAX_HABIT_QUANTITY, OccurrencePageCursor, OutcomeWrite, PauseCreate, PauseResume,
    },
    scheduling::ComposeScheduleResult,
};

use super::{DatabaseScope, lock_canonical_item_space};

const MAX_AUTHORITATIVE_MOVED_OCCURRENCES: usize = 10_000;
const MAX_RECURRENCE_IDENTITY_BYTES: usize = 4_096;

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
            StoredReceipt::Pause(_) => Err(HabitRepositoryError::IdempotencyConflict),
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
            StoredReceipt::Occurrence(_) => Err(HabitRepositoryError::IdempotencyConflict),
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
            "{OCCURRENCE_SELECT} WHERE evidence.workspace_id = $1 AND evidence.habit_id = $2 \
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
            changes
                .push(serde_json::from_value(payload).map_err(|_| HabitRepositoryError::Internal)?);
        }
        Ok(HabitDeltaPage {
            changes,
            watermark,
            has_more,
        })
    }
}

const OCCURRENCE_SELECT: &str = "SELECT evidence.id, evidence.habit_id, evidence.planner_occurrence_id, \
     evidence.source_schedule_revision_id, evidence.source_item_revision, \
     evidence.policy_fingerprint, evidence.recurrence_identity, evidence.nominal_start, \
     evidence.nominal_end, evidence.window_start, evidence.window_end, evidence.local_date, \
     evidence.timezone_name, evidence.expected_duration_seconds, evidence.expected_quantity, \
     evidence.expected_unit, outcome.revision AS outcome_revision, outcome.status AS outcome_status, \
     outcome.progress_basis_points, outcome.quantity, outcome.unit, outcome.actual_seconds, \
     outcome.note, outcome.occurred_at, outcome.updated_at \
     FROM habit_occurrence_evidence evidence LEFT JOIN habit_occurrence_outcomes outcome \
       ON outcome.workspace_id = evidence.workspace_id \
      AND outcome.occurrence_evidence_id = evidence.id";

async fn occurrence_row_for_update(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    habit_id: Uuid,
    occurrence_id: Uuid,
) -> Result<Option<sqlx::postgres::PgRow>, HabitRepositoryError> {
    sqlx::query(AssertSqlSafe(format!(
        "{OCCURRENCE_SELECT} WHERE evidence.workspace_id = $1 AND evidence.habit_id = $2 AND evidence.id = $3 FOR UPDATE OF evidence"
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
    let fingerprint: Vec<u8> = row.try_get("policy_fingerprint").map_err(storage)?;
    if fingerprint.len() != 32 {
        return Err(HabitRepositoryError::Internal);
    }
    let source_revision: i64 = row.try_get("source_item_revision").map_err(storage)?;
    let expected_duration: Option<i64> =
        row.try_get("expected_duration_seconds").map_err(storage)?;
    let outcome_revision: Option<i64> = row.try_get("outcome_revision").map_err(storage)?;
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
    Ok(HabitOccurrence { evidence, outcome })
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

async fn lock_habit_mutation_space(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), HabitRepositoryError> {
    lock_canonical_item_space(tx, workspace_id)
        .await
        .map_err(storage)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.habits.v1:' || $1::text, 0))",
    )
    .bind(workspace_id)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
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
    let rows = sqlx::query(
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
    serde_json::from_value(response)
        .map(Some)
        .map_err(|_| HabitRepositoryError::Internal)
}

async fn insert_receipt(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &HabitIdempotency,
    response: &StoredReceipt,
    now: DateTime<Utc>,
) -> Result<(), HabitRepositoryError> {
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

async fn insert_change(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    kind: &str,
    entity_id: Uuid,
    entity_revision: u64,
    change: &HabitDeltaChange,
    changed_at: DateTime<Utc>,
) -> Result<u64, HabitRepositoryError> {
    let payload = serde_json::to_value(change).map_err(|_| HabitRepositoryError::Internal)?;
    let sequence: i64 = sqlx::query_scalar(
        "INSERT INTO habit_changes (workspace_id, change_kind, entity_id, entity_revision, payload, changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING sequence",
    )
    .bind(workspace_id)
    .bind(kind)
    .bind(entity_id)
    .bind(to_i64(entity_revision)?)
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
    revision: u64,
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
    .bind(to_i64(revision)?)
    .bind(event_type)
    .bind(format!("habit:{entity_id}:{revision}:{sequence}"))
    .bind(json!({"entity_id":entity_id,"revision":revision,"change_sequence":sequence}))
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
    let rows = sqlx::query(
        "SELECT evidence.habit_id, evidence.planner_occurrence_id, \
                evidence.expected_duration_seconds, outcome.status, outcome.progress_basis_points \
         FROM habit_occurrence_evidence evidence JOIN habit_occurrence_outcomes outcome \
           ON outcome.workspace_id = evidence.workspace_id \
          AND outcome.occurrence_evidence_id = evidence.id \
         JOIN items item ON item.workspace_id = evidence.workspace_id AND item.id = evidence.habit_id \
         WHERE evidence.workspace_id = $1 AND item.kind = 'habit' AND item.trashed_at IS NULL \
           AND outcome.status IN ('partial', 'completed', 'skipped') \
           AND ((evidence.window_start < $3 AND evidence.window_end > $2) \
                OR evidence.planner_occurrence_id = ANY($4)) \
         ORDER BY evidence.habit_id, evidence.planner_occurrence_id",
    )
    .bind(workspace_id)
    .bind(horizon_start)
    .bind(horizon_end)
    .bind(moved_occurrence_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    let mut context = RecurrenceContext::default();
    for row in rows {
        let habit_id: Uuid = row
            .try_get("habit_id")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let occurrence_id: Uuid = row
            .try_get("planner_occurrence_id")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
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
                let expected_seconds: Option<i64> = row
                    .try_get("expected_duration_seconds")
                    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
                if let Some(expected_seconds) = expected_seconds {
                    let expected_seconds = u64::try_from(expected_seconds)
                        .map_err(|_| PublishedHabitEvidenceError::Invalid)?;
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
    let anchors = sqlx::query(
        "SELECT DISTINCT ON (evidence.habit_id) evidence.habit_id, outcome.occurred_at \
         FROM habit_occurrence_evidence evidence JOIN habit_occurrence_outcomes outcome \
           ON outcome.workspace_id = evidence.workspace_id \
          AND outcome.occurrence_evidence_id = evidence.id \
         JOIN items item ON item.workspace_id = evidence.workspace_id AND item.id = evidence.habit_id \
         WHERE evidence.workspace_id = $1 AND item.kind = 'habit' AND item.trashed_at IS NULL \
           AND outcome.status = 'completed' \
         ORDER BY evidence.habit_id, outcome.occurred_at DESC, evidence.id DESC",
    )
    .bind(workspace_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    for row in anchors {
        let habit_id: Uuid = row
            .try_get("habit_id")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        let occurred_at: DateTime<Utc> = row
            .try_get("occurred_at")
            .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
        context.completion_anchors.insert(
            ItemId(habit_id),
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
    if nominal_end <= nominal_start
        || window_end <= window_start
        || nominal_start < window_start
        || nominal_end > window_end
    {
        return Err(PublishedHabitEvidenceError::Invalid);
    }
    let evidence_id = Uuid::new_v4();
    let inserted = sqlx::query(
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
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    if inserted.rows_affected() == 1 {
        let evidence = HabitOccurrenceEvidence {
            id: evidence_id,
            habit_id: occurrence.series_item_id.0,
            planner_occurrence_id: occurrence.id.0,
            source_schedule_revision_id: schedule_revision_id,
            source_item_revision: policy.source_revision,
            policy_fingerprint: prefixed_hex(&policy.fingerprint),
            identity,
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
        let change = HabitDeltaChange::OccurrenceUpsert {
            occurrence: HabitOccurrence {
                evidence,
                outcome: None,
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
        return Ok(());
    }
    let matches: bool = sqlx::query_scalar(
        "SELECT policy_fingerprint = $4 AND recurrence_identity = $5 AND nominal_start = $6 \
         AND nominal_end = $7 AND window_start = $8 AND window_end = $9 AND local_date = $10 \
         AND timezone_name = $11 AND expected_duration_seconds IS NOT DISTINCT FROM $12 \
         AND expected_quantity IS NOT DISTINCT FROM $13 AND expected_unit IS NOT DISTINCT FROM $14 \
         FROM habit_occurrence_evidence WHERE workspace_id = $1 AND habit_id = $2 AND planner_occurrence_id = $3",
    )
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
    .bind(policy.expected_duration_seconds.map(i64::try_from).transpose().map_err(|_| PublishedHabitEvidenceError::Invalid)?)
    .bind(policy.expected_quantity)
    .bind(&policy.expected_unit)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| PublishedHabitEvidenceError::Unavailable)?;
    if !matches {
        return Err(PublishedHabitEvidenceError::Conflict);
    }
    // An exact re-publication is intentionally a storage no-op. In
    // particular, read/preview refreshes must not manufacture a habit change
    // or advance an observation timestamp merely because the schedule content
    // was revalidated. The first admitted publication remains the immutable
    // provenance for this occurrence.
    Ok(())
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
