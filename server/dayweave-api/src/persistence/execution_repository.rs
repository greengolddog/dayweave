use std::{collections::BTreeMap, fmt::Write as _, sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dayweave_core::{
    DeferCandidateAssessmentInput, ExecutionPlanningContext, ItemId, OccurrenceId,
    PreviousAssignment, PreviousBlock, Scheduler,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    execution::{
        DeferAssessment, DeferAssessmentRequest, DeferExecution, ExecutionCommand,
        ExecutionDomainError, ExecutionIdempotency, ExecutionMutation, ExecutionRepository,
        ExecutionRepositoryError, ExecutionSession, ExecutionSnapshot, ExecutionStatus,
        next_protocol_time,
    },
    scheduling::{
        AuthoritativePlanningEvidence, ManualPlacementViolationOutput, PublishedPlanningPolicy,
        SchedulePublicationError, assert_current_calendar_projection, assert_current_item_snapshot,
        assert_current_planning_policy_tx, authoritative_planning_evidence_tx,
        has_postgres_timestamp_precision, lock_owner, map_manual_placement_violations,
        published_planning_policy_tx,
    },
};

use super::{DatabaseScope, lock_canonical_item_space};

const IDEMPOTENCY_NAMESPACE: &str = "execution.command";
const DEFER_ASSESSMENT_SCHEMA: &str = "dayweave-execution-defer-assessment/1";
const DEFER_ASSESSMENT_TTL: chrono::Duration = chrono::Duration::minutes(5);
const DEFER_ASSESSMENT_MAINTENANCE_INTERVAL: StdDuration = StdDuration::from_hours(1);
const MAX_ACTIVE_DEFER_ASSESSMENTS: i64 = 256;
const MAX_ACTUAL_SECONDS: u64 = 31 * 24 * 60 * 60;
const HISTORY_SELECT: &str = "SELECT id, item_id, item_revision, occurrence_id, session_index, \
    planned_block_id, source_device_id, state, revision, accumulated_seconds, actual_seconds, \
    started_at, running_since, observed_running_since, paused_at, pause_until, pause_reason, \
    move_start, move_end, ended_at, created_at, updated_at \
    FROM execution_sessions WHERE workspace_id = $1 \
    ORDER BY updated_at DESC, id DESC LIMIT $2 OFFSET $3";
const SESSION_BY_ID: &str = "SELECT id, item_id, item_revision, occurrence_id, session_index, \
    planned_block_id, source_device_id, state, revision, accumulated_seconds, actual_seconds, \
    started_at, running_since, observed_running_since, paused_at, pause_until, pause_reason, \
    move_start, move_end, ended_at, created_at, updated_at \
    FROM execution_sessions WHERE workspace_id = $1 AND id = $2";
const SESSION_BY_ID_FOR_UPDATE: &str = "SELECT id, item_id, item_revision, occurrence_id, \
    session_index, planned_block_id, source_device_id, state, revision, accumulated_seconds, \
    actual_seconds, started_at, running_since, observed_running_since, paused_at, pause_until, \
    pause_reason, move_start, move_end, ended_at, created_at, updated_at FROM execution_sessions \
    WHERE workspace_id = $1 AND id = $2 FOR UPDATE";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedDeferAssessmentContext {
    schema: String,
    planning_as_of: DateTime<Utc>,
    candidate: DeferCandidateAssessmentInput,
}

#[derive(Clone, Debug)]
struct DeferAuthorization {
    assessment_id: Uuid,
    authorized_by_user_id: Uuid,
    environment_digest: [u8; 32],
    assessment_digest: [u8; 32],
    approved_assessment_digest: Option<[u8; 32]>,
    assessment_expires_at: DateTime<Utc>,
    authorization_kind: &'static str,
    execution_epoch: i64,
    replacement_session_index: u16,
    planned_duration_seconds: u64,
    effective_actual_seconds: u64,
    credited_source_seconds: u64,
    remaining_duration_seconds: u64,
}

#[derive(Clone, Debug)]
struct StoredDeferAssessment {
    id: Uuid,
    execution_state_revision: u64,
    source_execution_session_id: Uuid,
    source_execution_session_revision: u64,
    source_schedule_revision_id: Uuid,
    source_block_id: Uuid,
    current_schedule_revision_id: Uuid,
    current_schedule_revision_number: u64,
    current_publication_hash: [u8; 32],
    item_id: Uuid,
    source_item_revision: u64,
    current_item_revision: u64,
    execution_epoch: i64,
    occurrence_id: Option<Uuid>,
    source_session_index: u16,
    replacement_session_index: u16,
    planned_duration_seconds: u64,
    credited_before_seconds: u64,
    effective_actual_seconds: u64,
    credited_after_seconds: u64,
    credited_source_seconds: u64,
    remaining_duration_seconds: u64,
    scheduler_slot_seconds: u64,
    target_start: DateTime<Utc>,
    target_end: DateTime<Utc>,
    environment_digest: [u8; 32],
    assessment_digest: [u8; 32],
    approval_required: bool,
    context: PersistedDeferAssessmentContext,
    violations: Vec<ManualPlacementViolationOutput>,
    assessed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct PostgresExecutionRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresExecutionRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }

    /// Removes expired assessments that were never consumed by a durable
    /// replacement claim. Applied evidence remains attached to execution
    /// history for audit and exact replay.
    ///
    /// # Errors
    ///
    /// Returns a redacted storage error if the scoped transaction cannot
    /// complete.
    pub async fn maintain_defer_assessment_retention(
        &self,
    ) -> Result<(), ExecutionRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(internal)?;
        prune_defer_assessments(&mut transaction, self.scope, now).await?;
        transaction.commit().await.map_err(internal)
    }

    pub(crate) fn spawn_defer_assessment_maintenance_worker(self: &Arc<Self>) {
        let repository = Arc::clone(self);
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + DEFER_ASSESSMENT_MAINTENANCE_INTERVAL;
            let mut interval =
                tokio::time::interval_at(start, DEFER_ASSESSMENT_MAINTENANCE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = repository.maintain_defer_assessment_retention().await {
                    tracing::warn!(%error, "defer assessment retention maintenance failed");
                }
            }
        });
    }
}

#[async_trait]
impl ExecutionRepository for PostgresExecutionRepository {
    async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionRepositoryError> {
        ensure_state_pool(&self.pool, self.scope.workspace_id).await?;
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let state = sqlx::query(
            "SELECT revision, active_session_id FROM execution_state \
             WHERE workspace_id = $1 FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let revision = positive_or_zero_revision(state.try_get("revision").map_err(internal)?)?;
        let active_session_id: Option<Uuid> =
            state.try_get("active_session_id").map_err(internal)?;
        let active_session = match active_session_id {
            Some(id) => Some(
                fetch_session_transaction_read(&mut transaction, self.scope.workspace_id, id)
                    .await?,
            ),
            None => None,
        };
        transaction.commit().await.map_err(internal)?;
        Ok(ExecutionSnapshot {
            revision,
            active_session,
        })
    }

    async fn replay(
        &self,
        now: DateTime<Utc>,
        idempotency: &ExecutionIdempotency,
    ) -> Result<Option<ExecutionMutation>, ExecutionRepositoryError> {
        let row = sqlx::query(
            "SELECT request_fingerprint, state, response_json FROM idempotency_keys \
             WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 AND expires_at > $4",
        )
        .bind(self.scope.workspace_id)
        .bind(IDEMPOTENCY_NAMESPACE)
        .bind(idempotency.key_hash.as_slice())
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.map(|row| replay_from_row(&row, idempotency))
            .transpose()
    }

    async fn assess_defer(
        &self,
        request: DeferAssessmentRequest,
        _now: DateTime<Utc>,
    ) -> Result<DeferAssessment, ExecutionRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let assessment = assess_defer_transaction(&mut transaction, self.scope, &request).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(assessment)
    }

    async fn apply(
        &self,
        expected_revision: u64,
        command: ExecutionCommand,
        now: DateTime<Utc>,
        idempotency: ExecutionIdempotency,
    ) -> Result<ExecutionMutation, ExecutionRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if let Some(replay) =
            reserve_idempotency(&mut transaction, self.scope, now, &idempotency).await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(replay);
        }

        ensure_state_transaction(&mut transaction, self.scope.workspace_id, now).await?;
        let state = sqlx::query(
            "SELECT revision, active_session_id, updated_at FROM execution_state \
             WHERE workspace_id = $1 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let current_revision =
            positive_or_zero_revision(state.try_get("revision").map_err(internal)?)?;
        if expected_revision != current_revision {
            return Err(ExecutionRepositoryError::RevisionConflict {
                expected: expected_revision,
                actual: current_revision,
            });
        }
        let active_session_id: Option<Uuid> =
            state.try_get("active_session_id").map_err(internal)?;
        let protocol_updated_at: DateTime<Utc> = state.try_get("updated_at").map_err(internal)?;
        let transition_at =
            next_protocol_time(now, (current_revision > 0).then_some(protocol_updated_at))?;

        let changed_session = apply_command_transaction(
            &mut transaction,
            self.scope,
            current_revision,
            active_session_id,
            &command,
            transition_at,
            now,
        )
        .await?;

        let revision = current_revision
            .checked_add(1)
            .ok_or(ExecutionRepositoryError::Internal)?;
        let revision_i64 = revision_to_i64(revision)?;
        let next_active_id = changed_session
            .status
            .is_open()
            .then_some(changed_session.id);
        let updated = sqlx::query(
            "UPDATE execution_state SET revision = $2, active_session_id = $3, updated_at = $4 \
             WHERE workspace_id = $1 AND revision = $5",
        )
        .bind(self.scope.workspace_id)
        .bind(revision_i64)
        .bind(next_active_id)
        .bind(transition_at)
        .bind(revision_to_i64(current_revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated != 1 {
            return Err(ExecutionRepositoryError::Internal);
        }

        let mutation = ExecutionMutation {
            revision,
            active_session: next_active_id.map(|_| changed_session.clone()),
            changed_session,
            replayed: false,
        };
        record_outbox(
            &mut transaction,
            self.scope.workspace_id,
            &command,
            &mutation,
        )
        .await?;
        complete_idempotency(&mut transaction, self.scope, &idempotency, now, &mutation).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(mutation)
    }

    async fn history(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ExecutionSession>, ExecutionRepositoryError> {
        let limit = i64::try_from(limit).map_err(|_| ExecutionRepositoryError::Internal)?;
        let offset = i64::try_from(offset).map_err(|_| ExecutionRepositoryError::Internal)?;
        sqlx::query(HISTORY_SELECT)
            .bind(self.scope.workspace_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?
            .iter()
            .map(session_from_row)
            .collect()
    }
}

#[allow(clippy::too_many_lines)] // Assessment must bind one ordered transactional snapshot.
async fn assess_defer_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    request: &DeferAssessmentRequest,
) -> Result<DeferAssessment, ExecutionRepositoryError> {
    if request.session_id.is_nil()
        || !has_postgres_timestamp_precision(request.move_start)
        || request.move_start.timestamp_subsec_nanos() != 0
        || request
            .actual_seconds
            .is_some_and(|seconds| seconds > MAX_ACTUAL_SECONDS)
    {
        return Err(ExecutionRepositoryError::InvalidCommand(
            ExecutionDomainError::InvalidDefer,
        ));
    }

    let initialized_at: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
    ensure_state_transaction(transaction, scope.workspace_id, initialized_at).await?;
    let state = sqlx::query(
        "SELECT revision, active_session_id, updated_at FROM execution_state \
         WHERE workspace_id = $1 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    let execution_revision =
        positive_or_zero_revision(state.try_get("revision").map_err(internal)?)?;
    if request.expected_revision != execution_revision {
        return Err(ExecutionRepositoryError::RevisionConflict {
            expected: request.expected_revision,
            actual: execution_revision,
        });
    }
    let active_session_id: Option<Uuid> = state.try_get("active_session_id").map_err(internal)?;
    let protocol_updated_at: DateTime<Utc> = state.try_get("updated_at").map_err(internal)?;
    if active_session_id != Some(request.session_id) {
        return Err(ExecutionRepositoryError::SessionNotFound(
            request.session_id,
        ));
    }

    lock_canonical_item_space(transaction, scope.workspace_id)
        .await
        .map_err(internal)?;
    lock_owner(transaction, scope).await.map_err(internal)?;
    let policy = published_planning_policy_tx(transaction, scope)
        .await
        .map_err(map_schedule_assessment_error)?;
    assert_current_item_snapshot(transaction, scope, &policy.source_item_revisions)
        .await
        .map_err(map_schedule_assessment_error)?;
    assert_current_calendar_projection(
        transaction,
        scope,
        offset_to_chrono(policy.planning_request.horizon_start)?,
        offset_to_chrono(policy.planning_request.horizon_end)?,
        &policy.calendar_projection_stamps,
    )
    .await
    .map_err(map_schedule_assessment_error)?;
    assert_current_planning_policy_tx(transaction, scope, policy.revision_id)
        .await
        .map_err(map_schedule_assessment_error)?;

    let session =
        fetch_session_transaction(transaction, scope.workspace_id, request.session_id).await?;
    if session.status != ExecutionStatus::Paused {
        return Err(ExecutionRepositoryError::DeferRequiresPause);
    }
    let origin = defer_source_origin(transaction, scope.workspace_id, &session).await?;
    let current_item_revision = policy
        .source_item_revisions
        .get(&session.item_id)
        .copied()
        .filter(|revision| *revision == session.item_revision)
        .ok_or(ExecutionRepositoryError::ScheduleStale)?;
    let evidence = authoritative_planning_evidence_tx(transaction, scope.workspace_id)
        .await
        .map_err(|_| ExecutionRepositoryError::DeferAssessmentUnavailable)?;
    if evidence.execution.snapshot_revision != execution_revision
        || evidence.published_revision_id != Some(policy.revision_id)
    {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }

    let credited_before_seconds = exact_work_unit_credit(&evidence.execution, &session)?;
    let actual_seconds = request
        .actual_seconds
        .unwrap_or(session.accumulated_seconds);
    let credited_after_seconds = credited_before_seconds
        .checked_add(actual_seconds)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;
    let planned_duration_seconds = origin.planned_duration_seconds;
    let credited_source_seconds = normalized_source_credit_seconds(
        credited_before_seconds,
        credited_after_seconds,
        planned_duration_seconds,
    )?;
    let remaining_duration_seconds = planned_duration_seconds
        .checked_sub(credited_source_seconds)
        .filter(|seconds| *seconds > 0 && seconds % 60 == 0 && *seconds <= 24 * 60 * 60)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;
    let replacement_session_index = u16::try_from(
        next_replacement_session_index(
            transaction,
            scope.workspace_id,
            session.item_id,
            session.occurrence_id,
        )
        .await?,
    )
    .map_err(|_| ExecutionRepositoryError::IndexExhausted)?;

    let assessed_at: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
    let expires_at = assessed_at + DEFER_ASSESSMENT_TTL;
    let earliest_transition_at = next_protocol_time(
        assessed_at,
        (execution_revision > 0).then_some(protocol_updated_at),
    )?;
    let scheduler_slot_seconds = i64::from(policy.planning_request.config.slot_granularity.get())
        .checked_mul(60)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;
    if request.move_start <= expires_at
        || request.move_start <= earliest_transition_at
        || request
            .move_start
            .timestamp()
            .rem_euclid(scheduler_slot_seconds)
            != 0
    {
        return Err(ExecutionRepositoryError::InvalidCommand(
            ExecutionDomainError::InvalidDefer,
        ));
    }
    let remaining_i64 = i64::try_from(remaining_duration_seconds)
        .map_err(|_| ExecutionRepositoryError::DeferDurationConflict)?;
    let move_end = request
        .move_start
        .checked_add_signed(chrono::Duration::seconds(remaining_i64))
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;

    prune_defer_assessments(transaction, scope, assessed_at).await?;
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM execution_defer_assessments \
         WHERE workspace_id = $1 AND user_id = $2 AND expires_at > $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(assessed_at)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if active_count >= MAX_ACTIVE_DEFER_ASSESSMENTS {
        return Err(ExecutionRepositoryError::DeferAssessmentUnavailable);
    }

    let assessment_id = Uuid::new_v4();
    let mut planning_request = policy.planning_request.clone();
    planning_request.as_of = chrono_to_offset(expires_at)?;
    planning_request.previous_assignments = core_previous_assignments(&evidence)?;
    let candidate = DeferCandidateAssessmentInput {
        placement_id: assessment_id,
        item_id: ItemId(session.item_id),
        occurrence_id: session.occurrence_id.map(OccurrenceId),
        source_session_index: session.session_index,
        replacement_session_index,
        credited_seconds_after_source: credited_after_seconds,
        move_start: chrono_to_offset(request.move_start)?,
        move_end: chrono_to_offset(move_end)?,
    };
    let core = Scheduler
        .assess_defer_candidate(&planning_request, &evidence.execution, &candidate)
        .map_err(|_| ExecutionRepositoryError::DeferDurationConflict)?;
    let violations = map_manual_placement_violations(&core.assessment.violations)
        .map_err(|_| ExecutionRepositoryError::Internal)?;
    let approval_required = !violations.is_empty();
    let environment_digest = core.assessment.environment_digest;
    let context = PersistedDeferAssessmentContext {
        schema: DEFER_ASSESSMENT_SCHEMA.to_owned(),
        planning_as_of: expires_at,
        candidate,
    };
    let assessment_digest = defer_assessment_digest(
        scope,
        &policy,
        &session,
        &origin,
        execution_revision,
        current_item_revision,
        replacement_session_index,
        credited_before_seconds,
        actual_seconds,
        credited_after_seconds,
        credited_source_seconds,
        remaining_duration_seconds,
        request.move_start,
        move_end,
        assessed_at,
        expires_at,
        &environment_digest,
        &violations,
        &context,
    )?;

    let private_context =
        serde_json::to_value(&context).map_err(|_| ExecutionRepositoryError::Internal)?;
    let violations_json =
        serde_json::to_value(&violations).map_err(|_| ExecutionRepositoryError::Internal)?;
    let insert_at: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
    if insert_at >= expires_at {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }
    sqlx::query(
        "INSERT INTO execution_defer_assessments (id, workspace_id, user_id, schema_version, \
           execution_state_revision, source_execution_session_id, \
           source_execution_session_revision, source_schedule_revision_id, source_block_id, \
           current_schedule_revision_id, current_schedule_revision_number, \
           current_publication_hash, item_id, source_item_revision, current_item_revision, \
           execution_epoch, occurrence_id, source_session_index, replacement_session_index, \
           planned_duration_seconds, credited_before_seconds, effective_actual_seconds, \
           credited_after_seconds, credited_source_seconds, remaining_duration_seconds, \
           scheduler_slot_seconds, target_start, target_end, environment_digest, \
           assessment_digest, approval_required, private_context, violations, assessed_at, \
           expires_at) VALUES ($1, $2, $3, 1, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
           $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, \
           $30, $31, $32, $33, $34)",
    )
    .bind(assessment_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(revision_to_i64(execution_revision)?)
    .bind(session.id)
    .bind(revision_to_i64(session.revision)?)
    .bind(origin.schedule_revision_id)
    .bind(origin.source_block_id)
    .bind(policy.revision_id)
    .bind(revision_to_i64(policy.revision_number)?)
    .bind(policy.publication_hash.as_slice())
    .bind(session.item_id)
    .bind(revision_to_i64(session.item_revision)?)
    .bind(revision_to_i64(current_item_revision)?)
    .bind(origin.execution_epoch)
    .bind(session.occurrence_id)
    .bind(i32::from(session.session_index))
    .bind(i32::from(replacement_session_index))
    .bind(seconds_to_i64(planned_duration_seconds)?)
    .bind(seconds_to_i64(credited_before_seconds)?)
    .bind(seconds_to_i64(actual_seconds)?)
    .bind(seconds_to_i64(credited_after_seconds)?)
    .bind(seconds_to_i64(credited_source_seconds)?)
    .bind(seconds_to_i64(remaining_duration_seconds)?)
    .bind(i32::try_from(scheduler_slot_seconds).map_err(|_| ExecutionRepositoryError::Internal)?)
    .bind(request.move_start)
    .bind(move_end)
    .bind(environment_digest.as_slice())
    .bind(assessment_digest.as_slice())
    .bind(approval_required)
    .bind(private_context)
    .bind(violations_json)
    .bind(assessed_at)
    .bind(expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_defer_assessment_insert(&error))?;

    Ok(DeferAssessment {
        session_id: session.id,
        execution_revision,
        session_revision: session.revision,
        item_id: session.item_id,
        item_revision: current_item_revision,
        occurrence_id: session.occurrence_id,
        source_session_index: session.session_index,
        replacement_session_index,
        source_schedule_revision_id: origin.schedule_revision_id,
        source_block_id: origin.source_block_id,
        actual_seconds,
        credited_source_seconds,
        planned_duration_seconds,
        remaining_duration_seconds,
        move_start: request.move_start,
        move_end,
        environment_digest: encode_prefixed_sha256(&environment_digest),
        assessment_digest: encode_prefixed_sha256(&assessment_digest),
        approval_required,
        violations,
        expires_at,
    })
}

#[derive(Clone, Copy, Debug)]
struct DeferSourceOrigin {
    schedule_revision_id: Uuid,
    source_block_id: Uuid,
    execution_epoch: i64,
    planned_duration_seconds: u64,
}

async fn defer_source_origin(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    session: &ExecutionSession,
) -> Result<DeferSourceOrigin, ExecutionRepositoryError> {
    let row = sqlx::query(
        "SELECT origin.schedule_revision_id, origin.source_block_id, origin.item_id, \
           origin.item_revision, origin.execution_epoch, origin.occurrence_id, \
           origin.session_index, origin.planned_duration_seconds \
         FROM execution_session_schedule_origins AS origin \
         WHERE origin.workspace_id = $1 AND origin.execution_session_id = $2 FOR SHARE",
    )
    .bind(workspace_id)
    .bind(session.id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ExecutionRepositoryError::ScheduleStale)?;
    let item_id: Uuid = row.try_get("item_id").map_err(internal)?;
    let item_revision = positive_revision(row.try_get("item_revision").map_err(internal)?)?;
    let execution_epoch: i64 = row.try_get("execution_epoch").map_err(internal)?;
    let occurrence_id: Option<Uuid> = row.try_get("occurrence_id").map_err(internal)?;
    let session_index: i32 = row.try_get("session_index").map_err(internal)?;
    let planned_duration_seconds =
        nonnegative_seconds(row.try_get("planned_duration_seconds").map_err(internal)?)?;
    if item_id != session.item_id
        || item_revision != session.item_revision
        || execution_epoch <= 0
        || occurrence_id != session.occurrence_id
        || u16::try_from(session_index).ok() != Some(session.session_index)
        || session.planned_block_id != Some(row.try_get("source_block_id").map_err(internal)?)
        || planned_duration_seconds == 0
        || planned_duration_seconds % 60 != 0
    {
        return Err(ExecutionRepositoryError::ScheduleStale);
    }
    Ok(DeferSourceOrigin {
        schedule_revision_id: row.try_get("schedule_revision_id").map_err(internal)?,
        source_block_id: row.try_get("source_block_id").map_err(internal)?,
        execution_epoch,
        planned_duration_seconds,
    })
}

fn exact_work_unit_credit(
    execution: &ExecutionPlanningContext,
    session: &ExecutionSession,
) -> Result<u64, ExecutionRepositoryError> {
    let mut matching = execution.work_units.iter().filter(|unit| {
        unit.item_id.0 == session.item_id
            && unit.occurrence_id.map(|occurrence| occurrence.0) == session.occurrence_id
    });
    let unit = matching
        .next()
        .ok_or(ExecutionRepositoryError::DeferAssessmentStale)?;
    if matching.next().is_some()
        || !unit.used_session_indices.contains(&session.session_index)
        || unit
            .reservations
            .iter()
            .filter(|reservation| {
                reservation.session_index == session.session_index
                    && matches!(
                        reservation.kind,
                        dayweave_core::ExecutionReservationKind::InFlight
                    )
            })
            .count()
            != 1
    {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }
    Ok(unit.credited_seconds)
}

fn normalized_source_credit_seconds(
    credited_before_seconds: u64,
    credited_after_seconds: u64,
    planned_duration_seconds: u64,
) -> Result<u64, ExecutionRepositoryError> {
    let before_minutes =
        credited_before_seconds / 60 + u64::from(!credited_before_seconds.is_multiple_of(60));
    let after_minutes =
        credited_after_seconds / 60 + u64::from(!credited_after_seconds.is_multiple_of(60));
    let source_minutes = after_minutes
        .checked_sub(before_minutes)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;
    Ok(source_minutes
        .checked_mul(60)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?
        .min(planned_duration_seconds))
}

fn core_previous_assignments(
    evidence: &AuthoritativePlanningEvidence,
) -> Result<Vec<PreviousAssignment>, ExecutionRepositoryError> {
    let mut manual_ids = BTreeMap::new();
    for retained in &evidence.retained_manual_placements {
        for assignment in &retained.placement.assignments {
            if manual_ids
                .insert(
                    (assignment.item_id, assignment.occurrence_id),
                    retained.placement.id,
                )
                .is_some()
            {
                return Err(ExecutionRepositoryError::DeferAssessmentStale);
            }
        }
    }
    evidence
        .previous_assignments
        .iter()
        .map(|assignment| {
            Ok(PreviousAssignment {
                item_id: ItemId(assignment.item_id),
                occurrence_id: assignment.occurrence_id.map(OccurrenceId),
                blocks: assignment
                    .blocks
                    .iter()
                    .map(|block| {
                        Ok(PreviousBlock {
                            start: chrono_to_offset(block.start)?,
                            end: chrono_to_offset(block.end)?,
                            session_index: block.session_index,
                        })
                    })
                    .collect::<Result<_, ExecutionRepositoryError>>()?,
                pinned: assignment.pinned,
                manual_placement_id: assignment
                    .pinned
                    .then(|| {
                        manual_ids
                            .get(&(assignment.item_id, assignment.occurrence_id))
                            .copied()
                    })
                    .flatten(),
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // Every field is approval-bound explicitly.
fn defer_assessment_digest(
    scope: DatabaseScope,
    policy: &PublishedPlanningPolicy,
    session: &ExecutionSession,
    origin: &DeferSourceOrigin,
    execution_revision: u64,
    current_item_revision: u64,
    replacement_session_index: u16,
    credited_before_seconds: u64,
    actual_seconds: u64,
    credited_after_seconds: u64,
    credited_source_seconds: u64,
    remaining_duration_seconds: u64,
    move_start: DateTime<Utc>,
    move_end: DateTime<Utc>,
    assessed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    environment_digest: &[u8; 32],
    violations: &[ManualPlacementViolationOutput],
    context: &PersistedDeferAssessmentContext,
) -> Result<[u8; 32], ExecutionRepositoryError> {
    #[derive(Serialize)]
    struct Evidence<'a> {
        schema: &'static str,
        assessment_id: Uuid,
        workspace_id: Uuid,
        user_id: Uuid,
        execution_revision: u64,
        source_session_id: Uuid,
        source_session_revision: u64,
        source_schedule_revision_id: Uuid,
        source_block_id: Uuid,
        current_schedule_revision_id: Uuid,
        current_schedule_revision_number: u64,
        current_publication_hash: &'a [u8; 32],
        item_id: Uuid,
        source_item_revision: u64,
        current_item_revision: u64,
        execution_epoch: i64,
        occurrence_id: Option<Uuid>,
        source_session_index: u16,
        replacement_session_index: u16,
        planned_duration_seconds: u64,
        credited_before_seconds: u64,
        effective_actual_seconds: u64,
        credited_after_seconds: u64,
        credited_source_seconds: u64,
        remaining_duration_seconds: u64,
        scheduler_slot_seconds: u64,
        move_start: DateTime<Utc>,
        move_end: DateTime<Utc>,
        assessed_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        environment_digest: &'a [u8; 32],
        approval_required: bool,
        violations: &'a [ManualPlacementViolationOutput],
        context: &'a PersistedDeferAssessmentContext,
    }
    let scheduler_slot_seconds = u64::from(policy.planning_request.config.slot_granularity.get())
        .checked_mul(60)
        .ok_or(ExecutionRepositoryError::Internal)?;
    let encoded = serde_json::to_vec(&Evidence {
        schema: DEFER_ASSESSMENT_SCHEMA,
        assessment_id: context.candidate.placement_id,
        workspace_id: scope.workspace_id,
        user_id: scope.user_id,
        execution_revision,
        source_session_id: session.id,
        source_session_revision: session.revision,
        source_schedule_revision_id: origin.schedule_revision_id,
        source_block_id: origin.source_block_id,
        current_schedule_revision_id: policy.revision_id,
        current_schedule_revision_number: policy.revision_number,
        current_publication_hash: &policy.publication_hash,
        item_id: session.item_id,
        source_item_revision: session.item_revision,
        current_item_revision,
        execution_epoch: origin.execution_epoch,
        occurrence_id: session.occurrence_id,
        source_session_index: session.session_index,
        replacement_session_index,
        planned_duration_seconds: origin.planned_duration_seconds,
        credited_before_seconds,
        effective_actual_seconds: actual_seconds,
        credited_after_seconds,
        credited_source_seconds,
        remaining_duration_seconds,
        scheduler_slot_seconds,
        move_start,
        move_end,
        assessed_at,
        expires_at,
        environment_digest,
        approval_required: !violations.is_empty(),
        violations,
        context,
    })
    .map_err(|_| ExecutionRepositoryError::Internal)?;
    let mut digest = Sha256::new();
    digest.update(b"dayweave.execution-defer-assessment.v1\0");
    digest.update(encoded);
    Ok(digest.finalize().into())
}

async fn prune_defer_assessments(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), ExecutionRepositoryError> {
    sqlx::query(
        "DELETE FROM execution_defer_assessments AS assessment \
         WHERE assessment.workspace_id = $1 AND assessment.user_id = $2 \
           AND assessment.expires_at <= $3 \
           AND NOT EXISTS (SELECT 1 FROM execution_defer_replacement_claims AS claim \
             WHERE claim.workspace_id = assessment.workspace_id \
               AND claim.assessment_id = assessment.id)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

fn chrono_to_offset(
    value: DateTime<Utc>,
) -> Result<time::OffsetDateTime, ExecutionRepositoryError> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(value.timestamp_micros()) * 1_000)
        .map_err(|_| ExecutionRepositoryError::Internal)
}

fn offset_to_chrono(
    value: time::OffsetDateTime,
) -> Result<DateTime<Utc>, ExecutionRepositoryError> {
    let nanoseconds = value.unix_timestamp_nanos();
    if nanoseconds % 1_000 != 0 {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }
    let microseconds =
        i64::try_from(nanoseconds / 1_000).map_err(|_| ExecutionRepositoryError::Internal)?;
    DateTime::from_timestamp_micros(microseconds).ok_or(ExecutionRepositoryError::Internal)
}

fn encode_prefixed_sha256(value: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in value {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn decode_prefixed_sha256(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix("sha256:")?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Some((high << 4) | low)
        })
        .collect::<Option<Vec<_>>>()?;
    bytes.try_into().ok()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn map_schedule_assessment_error(error: SchedulePublicationError) -> ExecutionRepositoryError {
    match error {
        SchedulePublicationError::StaleComposition
        | SchedulePublicationError::DeferredPlacementRequired
        | SchedulePublicationError::InvalidPayload => ExecutionRepositoryError::ScheduleStale,
        SchedulePublicationError::AccessDenied
        | SchedulePublicationError::IdempotencyConflict
        | SchedulePublicationError::Unavailable => {
            ExecutionRepositoryError::DeferAssessmentUnavailable
        }
    }
}

fn map_schedule_authorization_error(error: SchedulePublicationError) -> ExecutionRepositoryError {
    match error {
        SchedulePublicationError::StaleComposition
        | SchedulePublicationError::DeferredPlacementRequired
        | SchedulePublicationError::InvalidPayload => {
            ExecutionRepositoryError::DeferAssessmentStale
        }
        SchedulePublicationError::AccessDenied
        | SchedulePublicationError::IdempotencyConflict
        | SchedulePublicationError::Unavailable => {
            ExecutionRepositoryError::DeferAssessmentUnavailable
        }
    }
}

#[allow(clippy::too_many_lines)] // Every approval-bound fact is revalidated in one transaction.
async fn authorize_defer_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    execution_revision: u64,
    input: &DeferExecution,
) -> Result<(ExecutionSession, DeferAuthorization), ExecutionRepositoryError> {
    let assessment_digest = input
        .assessment_digest
        .as_deref()
        .and_then(decode_prefixed_sha256)
        .ok_or(ExecutionRepositoryError::DeferAssessmentStale)?;
    let approved_assessment_digest = match input.approved_assessment_digest.as_deref() {
        Some(digest) => Some(
            decode_prefixed_sha256(digest).ok_or(ExecutionRepositoryError::DeferApprovalInvalid)?,
        ),
        None => None,
    };
    let actual_seconds = input
        .actual_seconds
        .ok_or(ExecutionRepositoryError::DeferAssessmentStale)?;
    let cheaply_available: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM execution_defer_assessments \
         WHERE workspace_id = $1 AND user_id = $2 AND assessment_digest = $3 \
           AND expires_at > statement_timestamp())",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(assessment_digest.as_slice())
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if !cheaply_available {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }

    lock_canonical_item_space(transaction, scope.workspace_id)
        .await
        .map_err(internal)?;
    lock_owner(transaction, scope).await.map_err(internal)?;
    let policy = published_planning_policy_tx(transaction, scope)
        .await
        .map_err(map_schedule_authorization_error)?;
    assert_current_item_snapshot(transaction, scope, &policy.source_item_revisions)
        .await
        .map_err(map_schedule_authorization_error)?;
    assert_current_calendar_projection(
        transaction,
        scope,
        offset_to_chrono(policy.planning_request.horizon_start)?,
        offset_to_chrono(policy.planning_request.horizon_end)?,
        &policy.calendar_projection_stamps,
    )
    .await
    .map_err(map_schedule_authorization_error)?;
    assert_current_planning_policy_tx(transaction, scope, policy.revision_id)
        .await
        .map_err(map_schedule_authorization_error)?;

    let session =
        fetch_session_transaction(transaction, scope.workspace_id, input.session_id).await?;
    if session.status != ExecutionStatus::Paused {
        return Err(ExecutionRepositoryError::DeferRequiresPause);
    }
    let origin = defer_source_origin(transaction, scope.workspace_id, &session).await?;
    let current_item_revision = policy
        .source_item_revisions
        .get(&session.item_id)
        .copied()
        .filter(|revision| *revision == session.item_revision)
        .ok_or(ExecutionRepositoryError::ScheduleStale)?;
    let evidence = authoritative_planning_evidence_tx(transaction, scope.workspace_id)
        .await
        .map_err(|_| ExecutionRepositoryError::DeferAssessmentUnavailable)?;
    if evidence.execution.snapshot_revision != execution_revision
        || evidence.published_revision_id != Some(policy.revision_id)
    {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }

    let row = sqlx::query(
        "SELECT id, execution_state_revision, source_execution_session_id, \
           source_execution_session_revision, source_schedule_revision_id, source_block_id, \
           current_schedule_revision_id, current_schedule_revision_number, \
           current_publication_hash, item_id, source_item_revision, current_item_revision, \
           execution_epoch, occurrence_id, source_session_index, replacement_session_index, \
           planned_duration_seconds, credited_before_seconds, effective_actual_seconds, \
           credited_after_seconds, credited_source_seconds, remaining_duration_seconds, \
           scheduler_slot_seconds, target_start, target_end, environment_digest, \
           assessment_digest, approval_required, private_context, violations, assessed_at, \
           expires_at FROM execution_defer_assessments \
         WHERE workspace_id = $1 AND user_id = $2 AND assessment_digest = $3 FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(assessment_digest.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ExecutionRepositoryError::DeferAssessmentStale)?;
    let stored = stored_defer_assessment_from_row(&row)?;
    let database_now: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;

    let scheduler_slot_seconds = u64::from(policy.planning_request.config.slot_granularity.get())
        .checked_mul(60)
        .ok_or(ExecutionRepositoryError::DeferAssessmentStale)?;
    let credited_before_seconds = exact_work_unit_credit(&evidence.execution, &session)?;
    let credited_after_seconds = credited_before_seconds
        .checked_add(actual_seconds)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;
    let credited_source_seconds = normalized_source_credit_seconds(
        credited_before_seconds,
        credited_after_seconds,
        origin.planned_duration_seconds,
    )?;
    let remaining_duration_seconds = origin
        .planned_duration_seconds
        .checked_sub(credited_source_seconds)
        .filter(|seconds| *seconds > 0 && seconds % 60 == 0 && *seconds <= 24 * 60 * 60)
        .ok_or(ExecutionRepositoryError::DeferDurationConflict)?;
    let replacement_session_index = u16::try_from(
        next_replacement_session_index(
            transaction,
            scope.workspace_id,
            session.item_id,
            session.occurrence_id,
        )
        .await?,
    )
    .map_err(|_| ExecutionRepositoryError::IndexExhausted)?;

    if stored.assessment_digest != assessment_digest
        || stored.execution_state_revision != execution_revision
        || stored.source_execution_session_id != session.id
        || stored.source_execution_session_revision != session.revision
        || stored.source_schedule_revision_id != origin.schedule_revision_id
        || stored.source_block_id != origin.source_block_id
        || stored.current_schedule_revision_id != policy.revision_id
        || stored.current_schedule_revision_number != policy.revision_number
        || stored.current_publication_hash != policy.publication_hash
        || stored.item_id != session.item_id
        || stored.source_item_revision != session.item_revision
        || stored.current_item_revision != current_item_revision
        || stored.execution_epoch != origin.execution_epoch
        || stored.occurrence_id != session.occurrence_id
        || stored.source_session_index != session.session_index
        || stored.replacement_session_index != replacement_session_index
        || stored.planned_duration_seconds != origin.planned_duration_seconds
        || stored.credited_before_seconds != credited_before_seconds
        || stored.effective_actual_seconds != actual_seconds
        || stored.credited_after_seconds != credited_after_seconds
        || stored.credited_source_seconds != credited_source_seconds
        || stored.remaining_duration_seconds != remaining_duration_seconds
        || stored.scheduler_slot_seconds != scheduler_slot_seconds
        || stored.target_start != input.move_start
        || stored.target_end != input.move_end
        || stored.context.schema != DEFER_ASSESSMENT_SCHEMA
        || stored.context.planning_as_of != stored.expires_at
        || stored.assessed_at > database_now
        || stored.expires_at <= database_now
    {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }

    let expected_candidate = DeferCandidateAssessmentInput {
        placement_id: stored.id,
        item_id: ItemId(session.item_id),
        occurrence_id: session.occurrence_id.map(OccurrenceId),
        source_session_index: session.session_index,
        replacement_session_index,
        credited_seconds_after_source: credited_after_seconds,
        move_start: chrono_to_offset(input.move_start)?,
        move_end: chrono_to_offset(input.move_end)?,
    };
    if stored.context.candidate != expected_candidate {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }
    let mut planning_request = policy.planning_request.clone();
    planning_request.as_of = chrono_to_offset(stored.context.planning_as_of)?;
    planning_request.previous_assignments = core_previous_assignments(&evidence)?;
    let core = Scheduler
        .assess_defer_candidate(&planning_request, &evidence.execution, &expected_candidate)
        .map_err(|_| ExecutionRepositoryError::DeferAssessmentStale)?;
    let violations = map_manual_placement_violations(&core.assessment.violations)
        .map_err(|_| ExecutionRepositoryError::Internal)?;
    if core.assessment.environment_digest != stored.environment_digest
        || violations != stored.violations
        || stored.approval_required == violations.is_empty()
    {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }
    let recomputed_digest = defer_assessment_digest(
        scope,
        &policy,
        &session,
        &origin,
        execution_revision,
        current_item_revision,
        replacement_session_index,
        credited_before_seconds,
        actual_seconds,
        credited_after_seconds,
        credited_source_seconds,
        remaining_duration_seconds,
        input.move_start,
        input.move_end,
        stored.assessed_at,
        stored.expires_at,
        &stored.environment_digest,
        &violations,
        &stored.context,
    )?;
    if recomputed_digest != stored.assessment_digest {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }

    let authorization_kind = if stored.approval_required {
        match approved_assessment_digest {
            None => return Err(ExecutionRepositoryError::DeferApprovalRequired),
            Some(approved) if approved == stored.assessment_digest => "explicit_approval",
            Some(_) => return Err(ExecutionRepositoryError::DeferApprovalInvalid),
        }
    } else if approved_assessment_digest.is_some() {
        return Err(ExecutionRepositoryError::DeferApprovalInvalid);
    } else {
        "conflict_free"
    };

    Ok((
        session,
        DeferAuthorization {
            assessment_id: stored.id,
            authorized_by_user_id: scope.user_id,
            environment_digest: stored.environment_digest,
            assessment_digest: stored.assessment_digest,
            approved_assessment_digest,
            assessment_expires_at: stored.expires_at,
            authorization_kind,
            execution_epoch: stored.execution_epoch,
            replacement_session_index,
            planned_duration_seconds: stored.planned_duration_seconds,
            effective_actual_seconds: stored.effective_actual_seconds,
            credited_source_seconds: stored.credited_source_seconds,
            remaining_duration_seconds: stored.remaining_duration_seconds,
        },
    ))
}

fn stored_defer_assessment_from_row(
    row: &PgRow,
) -> Result<StoredDeferAssessment, ExecutionRepositoryError> {
    let private_context: Value = row.try_get("private_context").map_err(internal)?;
    let violations: Value = row.try_get("violations").map_err(internal)?;
    Ok(StoredDeferAssessment {
        id: row.try_get("id").map_err(internal)?,
        execution_state_revision: positive_revision(
            row.try_get("execution_state_revision").map_err(internal)?,
        )?,
        source_execution_session_id: row
            .try_get("source_execution_session_id")
            .map_err(internal)?,
        source_execution_session_revision: positive_revision(
            row.try_get("source_execution_session_revision")
                .map_err(internal)?,
        )?,
        source_schedule_revision_id: row
            .try_get("source_schedule_revision_id")
            .map_err(internal)?,
        source_block_id: row.try_get("source_block_id").map_err(internal)?,
        current_schedule_revision_id: row
            .try_get("current_schedule_revision_id")
            .map_err(internal)?,
        current_schedule_revision_number: positive_revision(
            row.try_get("current_schedule_revision_number")
                .map_err(internal)?,
        )?,
        current_publication_hash: fixed_sha256_from_row(row, "current_publication_hash")?,
        item_id: row.try_get("item_id").map_err(internal)?,
        source_item_revision: positive_revision(
            row.try_get("source_item_revision").map_err(internal)?,
        )?,
        current_item_revision: positive_revision(
            row.try_get("current_item_revision").map_err(internal)?,
        )?,
        execution_epoch: row.try_get("execution_epoch").map_err(internal)?,
        occurrence_id: row.try_get("occurrence_id").map_err(internal)?,
        source_session_index: bounded_session_index(
            row.try_get("source_session_index").map_err(internal)?,
        )?,
        replacement_session_index: bounded_session_index(
            row.try_get("replacement_session_index").map_err(internal)?,
        )?,
        planned_duration_seconds: nonnegative_seconds(
            row.try_get("planned_duration_seconds").map_err(internal)?,
        )?,
        credited_before_seconds: nonnegative_seconds(
            row.try_get("credited_before_seconds").map_err(internal)?,
        )?,
        effective_actual_seconds: nonnegative_seconds(
            row.try_get("effective_actual_seconds").map_err(internal)?,
        )?,
        credited_after_seconds: nonnegative_seconds(
            row.try_get("credited_after_seconds").map_err(internal)?,
        )?,
        credited_source_seconds: nonnegative_seconds(
            row.try_get("credited_source_seconds").map_err(internal)?,
        )?,
        remaining_duration_seconds: nonnegative_seconds(
            row.try_get("remaining_duration_seconds")
                .map_err(internal)?,
        )?,
        scheduler_slot_seconds: u64::try_from(
            row.try_get::<i32, _>("scheduler_slot_seconds")
                .map_err(internal)?,
        )
        .map_err(|_| ExecutionRepositoryError::Internal)?,
        target_start: row.try_get("target_start").map_err(internal)?,
        target_end: row.try_get("target_end").map_err(internal)?,
        environment_digest: fixed_sha256_from_row(row, "environment_digest")?,
        assessment_digest: fixed_sha256_from_row(row, "assessment_digest")?,
        approval_required: row.try_get("approval_required").map_err(internal)?,
        context: serde_json::from_value(private_context)
            .map_err(|_| ExecutionRepositoryError::DeferAssessmentStale)?,
        violations: serde_json::from_value(violations)
            .map_err(|_| ExecutionRepositoryError::DeferAssessmentStale)?,
        assessed_at: row.try_get("assessed_at").map_err(internal)?,
        expires_at: row.try_get("expires_at").map_err(internal)?,
    })
}

fn fixed_sha256_from_row(row: &PgRow, column: &str) -> Result<[u8; 32], ExecutionRepositoryError> {
    row.try_get::<Vec<u8>, _>(column)
        .map_err(internal)?
        .try_into()
        .map_err(|_| ExecutionRepositoryError::Internal)
}

fn bounded_session_index(value: i32) -> Result<u16, ExecutionRepositoryError> {
    u16::try_from(value).map_err(|_| ExecutionRepositoryError::Internal)
}

async fn apply_command_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    execution_revision: u64,
    active_session_id: Option<Uuid>,
    command: &ExecutionCommand,
    transition_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
) -> Result<ExecutionSession, ExecutionRepositoryError> {
    let workspace_id = scope.workspace_id;
    if let ExecutionCommand::Start(input) = command {
        if active_session_id.is_some() {
            return Err(ExecutionRepositoryError::ActiveSessionConflict);
        }
        lock_workspace_items(transaction, workspace_id).await?;
        let execution_epoch = validate_start_item(
            transaction,
            workspace_id,
            input.item_id,
            input.item_revision,
        )
        .await?;
        if session_exists(transaction, input.session_id).await? {
            return Err(ExecutionRepositoryError::DuplicateSession(input.session_id));
        }
        validate_start_schedule(transaction, workspace_id, execution_epoch, input).await?;
        let session = ExecutionSession::start_with_protocol_time(input, transition_at, observed_at);
        insert_session(transaction, workspace_id, execution_epoch, &session).await?;
        return Ok(session);
    }

    let requested_id = command.session_id();
    if active_session_id != Some(requested_id) {
        return Err(ExecutionRepositoryError::SessionNotFound(requested_id));
    }
    let (current, defer_authorization) = if let ExecutionCommand::Defer(input) = command {
        let (session, authorization) =
            authorize_defer_transaction(transaction, scope, execution_revision, input).await?;
        (session, Some(authorization))
    } else {
        (
            fetch_session_transaction(transaction, workspace_id, requested_id).await?,
            None,
        )
    };
    let updated = current.apply_with_protocol_time(command, transition_at, observed_at)?;
    update_session(transaction, workspace_id, &updated).await?;
    if let Some(authorization) = defer_authorization {
        insert_defer_replacement_claim(transaction, scope, &updated, &authorization).await?;
    }
    Ok(updated)
}

async fn validate_start_schedule(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    execution_epoch: i64,
    input: &crate::execution::StartExecution,
) -> Result<(), ExecutionRepositoryError> {
    let physical = sqlx::query(
        "SELECT reservation_kind, source_deferred_session_id \
         FROM execution_physical_indices WHERE workspace_id = $1 AND item_id = $2 \
         AND occurrence_id IS NOT DISTINCT FROM $3 AND session_index = $4 FOR SHARE",
    )
    .bind(workspace_id)
    .bind(input.item_id)
    .bind(input.occurrence_id)
    .bind(i32::from(input.session_index))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(physical) = physical else {
        // Passive compatibility is restricted to a genuinely unused physical
        // index. The Start trigger reserves it before the transaction commits.
        return Ok(());
    };
    let reservation_kind: String = physical.try_get("reservation_kind").map_err(internal)?;
    let source_deferred_session_id: Option<Uuid> = physical
        .try_get("source_deferred_session_id")
        .map_err(internal)?;
    if reservation_kind != "defer_replacement" || source_deferred_session_id.is_none() {
        return Err(ExecutionRepositoryError::ScheduleStale);
    }
    let Some(planned_block_id) = input.planned_block_id else {
        return Err(ExecutionRepositoryError::ScheduleStale);
    };
    let source_deferred_session_id =
        source_deferred_session_id.ok_or(ExecutionRepositoryError::Internal)?;
    let attested: bool = sqlx::query_scalar(
        "SELECT EXISTS ( \
           SELECT 1 FROM execution_defer_replacement_claims AS claim \
           JOIN schedule_defer_replacement_placements AS placement \
             ON placement.workspace_id = claim.workspace_id \
            AND placement.source_deferred_session_id = claim.source_deferred_session_id \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = placement.workspace_id \
            AND revision.id = placement.schedule_revision_id \
           JOIN schedule_blocks AS block \
             ON block.workspace_id = placement.workspace_id \
            AND block.schedule_revision_id = placement.schedule_revision_id \
            AND block.source_block_id = placement.source_block_id \
           WHERE claim.workspace_id = $1 AND claim.source_deferred_session_id = $2 \
             AND claim.actionable AND claim.item_id = $3 AND claim.execution_epoch = $4 \
             AND claim.occurrence_id IS NOT DISTINCT FROM $5 \
             AND claim.replacement_session_index = $6 \
             AND placement.source_block_id = $7 AND placement.item_id = $3 \
             AND placement.item_revision = $8 AND placement.execution_epoch = $4 \
             AND placement.occurrence_id IS NOT DISTINCT FROM $5 \
             AND placement.replacement_session_index = $6 \
             AND placement.remaining_duration_seconds = claim.remaining_duration_seconds \
             AND placement.move_start = claim.move_start AND placement.move_end = claim.move_end \
             AND revision.state = 'published' AND block.item_id = $3 \
             AND block.starts_at = claim.move_start AND block.ends_at = claim.move_end \
             AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at)) \
                   = claim.remaining_duration_seconds::numeric \
             AND NOT EXISTS (SELECT 1 FROM execution_defer_replacement_consumptions AS consumed \
               WHERE consumed.workspace_id = claim.workspace_id \
                 AND consumed.source_deferred_session_id = claim.source_deferred_session_id) \
         )",
    )
    .bind(workspace_id)
    .bind(source_deferred_session_id)
    .bind(input.item_id)
    .bind(execution_epoch)
    .bind(input.occurrence_id)
    .bind(i32::from(input.session_index))
    .bind(planned_block_id)
    .bind(revision_to_i64(input.item_revision)?)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if attested {
        Ok(())
    } else {
        Err(ExecutionRepositoryError::ScheduleStale)
    }
}

async fn ensure_state_pool(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<(), ExecutionRepositoryError> {
    sqlx::query("INSERT INTO execution_state (workspace_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(workspace_id)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(())
}

async fn ensure_state_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), ExecutionRepositoryError> {
    sqlx::query(
        "INSERT INTO execution_state (workspace_id, updated_at) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(workspace_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn reserve_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
    idempotency: &ExecutionIdempotency,
) -> Result<Option<ExecutionMutation>, ExecutionRepositoryError> {
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
         AND key_hash = $3 AND expires_at <= $4",
    )
    .bind(scope.workspace_id)
    .bind(IDEMPOTENCY_NAMESPACE)
    .bind(idempotency.key_hash.as_slice())
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    let inserted = sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         created_at, updated_at, expires_at) VALUES ($1, $2, $3, $4, $5, $5, $6) \
         ON CONFLICT (workspace_id, namespace, key_hash) DO NOTHING",
    )
    .bind(scope.workspace_id)
    .bind(IDEMPOTENCY_NAMESPACE)
    .bind(idempotency.key_hash.as_slice())
    .bind(idempotency.fingerprint.as_slice())
    .bind(now)
    .bind(idempotency.expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if inserted == 1 {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT request_fingerprint, state, response_json FROM idempotency_keys \
         WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(IDEMPOTENCY_NAMESPACE)
    .bind(idempotency.key_hash.as_slice())
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(Some(replay_from_row(&row, idempotency)?))
}

fn replay_from_row(
    row: &PgRow,
    idempotency: &ExecutionIdempotency,
) -> Result<ExecutionMutation, ExecutionRepositoryError> {
    let fingerprint: Vec<u8> = row.try_get("request_fingerprint").map_err(internal)?;
    if fingerprint != idempotency.fingerprint {
        return Err(ExecutionRepositoryError::IdempotencyConflict);
    }
    let state: String = row.try_get("state").map_err(internal)?;
    if state != "completed" {
        return Err(ExecutionRepositoryError::Internal);
    }
    let response: Value = row
        .try_get::<Option<Value>, _>("response_json")
        .map_err(internal)?
        .ok_or(ExecutionRepositoryError::Internal)?;
    let mutation: ExecutionMutation =
        serde_json::from_value(response).map_err(|_| ExecutionRepositoryError::Internal)?;
    Ok(ExecutionMutation {
        replayed: true,
        ..mutation
    })
}

async fn complete_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &ExecutionIdempotency,
    now: DateTime<Utc>,
    mutation: &ExecutionMutation,
) -> Result<(), ExecutionRepositoryError> {
    let response =
        serde_json::to_value(mutation).map_err(|_| ExecutionRepositoryError::Internal)?;
    let updated = sqlx::query(
        "UPDATE idempotency_keys SET state = 'completed', resource_type = 'execution_session', \
         resource_id = $4, response_json = $5, updated_at = $6 \
         WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 \
         AND request_fingerprint = $7 AND state = 'in_progress'",
    )
    .bind(scope.workspace_id)
    .bind(IDEMPOTENCY_NAMESPACE)
    .bind(idempotency.key_hash.as_slice())
    .bind(mutation.changed_session.id)
    .bind(response)
    .bind(now)
    .bind(idempotency.fingerprint.as_slice())
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(ExecutionRepositoryError::Internal)
    }
}

async fn lock_workspace_items(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), ExecutionRepositoryError> {
    sqlx::query("SELECT id FROM items WHERE workspace_id = $1 ORDER BY id FOR UPDATE")
        .bind(workspace_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(internal)?;
    Ok(())
}

async fn validate_start_item(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
    expected_revision: u64,
) -> Result<i64, ExecutionRepositoryError> {
    let row = sqlx::query(
        "SELECT revision, execution_epoch, status, kind, has_own_effort, trashed_at FROM items \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ExecutionRepositoryError::ItemNotExecutable)?;
    let revision = positive_revision(row.try_get("revision").map_err(internal)?)?;
    let execution_epoch: i64 = row.try_get("execution_epoch").map_err(internal)?;
    if execution_epoch <= 0 {
        return Err(ExecutionRepositoryError::Internal);
    }
    if revision != expected_revision {
        return Err(ExecutionRepositoryError::ItemRevisionConflict);
    }
    let status: String = row.try_get("status").map_err(internal)?;
    let kind: String = row.try_get("kind").map_err(internal)?;
    let has_own_effort: bool = row.try_get("has_own_effort").map_err(internal)?;
    let trashed_at: Option<DateTime<Utc>> = row.try_get("trashed_at").map_err(internal)?;
    let has_children: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM item_hierarchy AS edge JOIN items AS child \
         ON child.workspace_id = edge.workspace_id AND child.id = edge.child_item_id \
         WHERE edge.workspace_id = $1 AND edge.parent_item_id = $2 \
         AND child.trashed_at IS NULL)",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if trashed_at.is_some()
        || has_children
        || (matches!(kind.as_str(), "project" | "goal" | "routine") && !has_own_effort)
        || matches!(
            status.as_str(),
            "completed" | "skipped" | "cancelled" | "blocked"
        )
    {
        Err(ExecutionRepositoryError::ItemNotExecutable)
    } else {
        Ok(execution_epoch)
    }
}

async fn insert_defer_replacement_claim(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    session: &ExecutionSession,
    authorization: &DeferAuthorization,
) -> Result<(), ExecutionRepositoryError> {
    let move_start = session
        .move_start
        .ok_or(ExecutionRepositoryError::Internal)?;
    let move_end = session.move_end.ok_or(ExecutionRepositoryError::Internal)?;
    if session.actual_seconds != Some(authorization.effective_actual_seconds)
        || exact_window_seconds(move_start, move_end)?
            != seconds_to_i64(authorization.remaining_duration_seconds)?
        || authorization.execution_epoch <= 0
    {
        return Err(ExecutionRepositoryError::DeferDurationConflict);
    }
    let claim_insert_at: DateTime<Utc> = sqlx::query_scalar("SELECT statement_timestamp()")
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
    if claim_insert_at >= authorization.assessment_expires_at {
        return Err(ExecutionRepositoryError::DeferAssessmentStale);
    }

    let inserted = sqlx::query(
        "INSERT INTO execution_defer_replacement_claims (workspace_id, \
         source_deferred_session_id, item_id, source_item_revision, execution_epoch, \
         occurrence_id, source_session_index, replacement_session_index, \
         planned_duration_seconds, planned_duration_source, actionable, consumed_before_seconds, \
         consumed_by_source_seconds, remaining_duration_seconds, move_start, move_end, created_at, \
         authorization_schema_version, authorization_kind, assessment_id, authorized_by_user_id, \
         environment_digest, assessment_digest, approved_assessment_digest, assessment_expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'published_origin', true, 0, $10, \
         $11, $12, $13, $14, 1, $15, $16, $17, $18, $19, $20, $21)",
    )
    .bind(scope.workspace_id)
    .bind(session.id)
    .bind(session.item_id)
    .bind(revision_to_i64(session.item_revision)?)
    .bind(authorization.execution_epoch)
    .bind(session.occurrence_id)
    .bind(i32::from(session.session_index))
    .bind(i32::from(authorization.replacement_session_index))
    .bind(seconds_to_i64(authorization.planned_duration_seconds)?)
    .bind(seconds_to_i64(authorization.credited_source_seconds)?)
    .bind(seconds_to_i64(authorization.remaining_duration_seconds)?)
    .bind(move_start)
    .bind(move_end)
    .bind(session.updated_at)
    .bind(authorization.authorization_kind)
    .bind(authorization.assessment_id)
    .bind(authorization.authorized_by_user_id)
    .bind(authorization.environment_digest.as_slice())
    .bind(authorization.assessment_digest.as_slice())
    .bind(
        authorization
            .approved_assessment_digest
            .as_ref()
            .map(<[u8; 32]>::as_slice),
    )
    .bind(authorization.assessment_expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_defer_claim_insert(&error))?
    .rows_affected();
    if inserted == 1 {
        Ok(())
    } else {
        Err(ExecutionRepositoryError::Internal)
    }
}

async fn next_replacement_session_index(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
    occurrence_id: Option<Uuid>,
) -> Result<i32, ExecutionRepositoryError> {
    let high_water: i32 = sqlx::query_scalar(
        "WITH current_published_block_indices AS ( \
           SELECT CASE \
             WHEN block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$' \
              AND (block.constraint_snapshot ->> 'session_index')::numeric \
                    BETWEEN 0 AND 65535 \
             THEN (block.constraint_snapshot ->> 'session_index')::integer \
           END AS session_index \
           FROM schedule_blocks AS block \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = block.workspace_id \
            AND revision.id = block.schedule_revision_id \
           WHERE revision.workspace_id = $1 AND revision.state = 'published' \
             AND block.item_id = $2 \
             AND block.constraint_snapshot ->> 'occurrence_id' \
                 IS NOT DISTINCT FROM $3::uuid::text \
         ) \
         SELECT GREATEST( \
           COALESCE((SELECT MAX(session_index) FROM execution_physical_indices \
                     WHERE workspace_id = $1 AND item_id = $2 \
                       AND occurrence_id IS NOT DISTINCT FROM $3), -1), \
           COALESCE((SELECT MAX(session_index) \
                     FROM current_published_block_indices), -1) \
         )",
    )
    .bind(workspace_id)
    .bind(item_id)
    .bind(occurrence_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if high_water >= i32::from(u16::MAX) {
        return Err(ExecutionRepositoryError::IndexExhausted);
    }
    high_water
        .checked_add(1)
        .ok_or(ExecutionRepositoryError::IndexExhausted)
}

fn exact_window_seconds(
    move_start: DateTime<Utc>,
    move_end: DateTime<Utc>,
) -> Result<i64, ExecutionRepositoryError> {
    let microseconds = move_end
        .signed_duration_since(move_start)
        .num_microseconds()
        .ok_or(ExecutionRepositoryError::Internal)?;
    if microseconds <= 0 {
        return Err(ExecutionRepositoryError::Internal);
    }
    if microseconds % 1_000_000 != 0 {
        return Err(ExecutionRepositoryError::DeferDurationConflict);
    }
    microseconds
        .checked_div(1_000_000)
        .filter(|seconds| *seconds > 0)
        .ok_or(ExecutionRepositoryError::Internal)
}

async fn session_exists(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<bool, ExecutionRepositoryError> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM execution_sessions WHERE id = $1)")
        .bind(session_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)
}

async fn insert_session(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    execution_epoch: i64,
    session: &ExecutionSession,
) -> Result<(), ExecutionRepositoryError> {
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         execution_epoch, occurrence_id, session_index, planned_block_id, source_device_id, state, \
         revision, accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
         $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24)",
    )
    .bind(session.id)
    .bind(workspace_id)
    .bind(session.item_id)
    .bind(revision_to_i64(session.item_revision)?)
    .bind(execution_epoch)
    .bind(session.occurrence_id)
    .bind(i32::from(session.session_index))
    .bind(session.planned_block_id)
    .bind(session.source_device_id)
    .bind(status_name(session.status))
    .bind(revision_to_i64(session.revision)?)
    .bind(seconds_to_i64(session.accumulated_seconds)?)
    .bind(session.actual_seconds.map(seconds_to_i64).transpose()?)
    .bind(session.started_at)
    .bind(session.running_since)
    .bind(session.observed_running_since)
    .bind(session.paused_at)
    .bind(session.pause_until)
    .bind(&session.pause_reason)
    .bind(session.move_start)
    .bind(session.move_end)
    .bind(session.ended_at)
    .bind(session.created_at)
    .bind(session.updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn update_session(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    session: &ExecutionSession,
) -> Result<(), ExecutionRepositoryError> {
    let updated = sqlx::query(
        "UPDATE execution_sessions SET state = $3, revision = $4, accumulated_seconds = $5, \
         actual_seconds = $6, running_since = $7, observed_running_since = $8, paused_at = $9, \
         pause_until = $10, pause_reason = $11, move_start = $12, move_end = $13, ended_at = $14, \
         updated_at = $15 WHERE workspace_id = $1 AND id = $2 AND revision = $16",
    )
    .bind(workspace_id)
    .bind(session.id)
    .bind(status_name(session.status))
    .bind(revision_to_i64(session.revision)?)
    .bind(seconds_to_i64(session.accumulated_seconds)?)
    .bind(session.actual_seconds.map(seconds_to_i64).transpose()?)
    .bind(session.running_since)
    .bind(session.observed_running_since)
    .bind(session.paused_at)
    .bind(session.pause_until)
    .bind(&session.pause_reason)
    .bind(session.move_start)
    .bind(session.move_end)
    .bind(session.ended_at)
    .bind(session.updated_at)
    .bind(revision_to_i64(session.revision - 1)?)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(ExecutionRepositoryError::Internal)
    }
}

async fn fetch_session_transaction_read(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<ExecutionSession, ExecutionRepositoryError> {
    let row = sqlx::query(SESSION_BY_ID)
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(internal)?
        .ok_or(ExecutionRepositoryError::SessionNotFound(id))?;
    session_from_row(&row)
}

async fn fetch_session_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<ExecutionSession, ExecutionRepositoryError> {
    let row = sqlx::query(SESSION_BY_ID_FOR_UPDATE)
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(internal)?
        .ok_or(ExecutionRepositoryError::SessionNotFound(id))?;
    session_from_row(&row)
}

fn session_from_row(row: &PgRow) -> Result<ExecutionSession, ExecutionRepositoryError> {
    let state: String = row.try_get("state").map_err(internal)?;
    let session_index: i32 = row.try_get("session_index").map_err(internal)?;
    Ok(ExecutionSession {
        id: row.try_get("id").map_err(internal)?,
        item_id: row.try_get("item_id").map_err(internal)?,
        item_revision: positive_revision(row.try_get("item_revision").map_err(internal)?)?,
        occurrence_id: row.try_get("occurrence_id").map_err(internal)?,
        session_index: u16::try_from(session_index)
            .map_err(|_| ExecutionRepositoryError::Internal)?,
        planned_block_id: row.try_get("planned_block_id").map_err(internal)?,
        source_device_id: row.try_get("source_device_id").map_err(internal)?,
        status: parse_status(&state)?,
        revision: positive_revision(row.try_get("revision").map_err(internal)?)?,
        accumulated_seconds: nonnegative_seconds(
            row.try_get("accumulated_seconds").map_err(internal)?,
        )?,
        actual_seconds: row
            .try_get::<Option<i64>, _>("actual_seconds")
            .map_err(internal)?
            .map(nonnegative_seconds)
            .transpose()?,
        started_at: row.try_get("started_at").map_err(internal)?,
        running_since: row.try_get("running_since").map_err(internal)?,
        observed_running_since: row.try_get("observed_running_since").map_err(internal)?,
        paused_at: row.try_get("paused_at").map_err(internal)?,
        pause_until: row.try_get("pause_until").map_err(internal)?,
        pause_reason: row.try_get("pause_reason").map_err(internal)?,
        move_start: row.try_get("move_start").map_err(internal)?,
        move_end: row.try_get("move_end").map_err(internal)?,
        ended_at: row.try_get("ended_at").map_err(internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
    })
}

async fn record_outbox(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    command: &ExecutionCommand,
    mutation: &ExecutionMutation,
) -> Result<(), ExecutionRepositoryError> {
    let operation = match command {
        ExecutionCommand::Start(_) => "execution.started",
        ExecutionCommand::Pause(_) => "execution.paused",
        ExecutionCommand::Resume(_) => "execution.resumed",
        ExecutionCommand::Complete(_) => "execution.completed",
        ExecutionCommand::Skip(_) => "execution.skipped",
        ExecutionCommand::Defer(_) => "execution.deferred",
    };
    let payload = serde_json::to_value(mutation).map_err(|_| ExecutionRepositoryError::Internal)?;
    sqlx::query(
        "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, \
         aggregate_revision, event_type, deduplication_key, payload) \
         VALUES ($1, $2, 'execution_session', $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(workspace_id)
    .bind(mutation.changed_session.id)
    .bind(revision_to_i64(mutation.changed_session.revision)?)
    .bind(operation)
    .bind(format!(
        "{operation}:{}:{}",
        mutation.changed_session.id, mutation.changed_session.revision
    ))
    .bind(json!(payload))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

const fn status_name(status: ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Active => "active",
        ExecutionStatus::Paused => "paused",
        ExecutionStatus::Completed => "completed",
        ExecutionStatus::Skipped => "skipped",
        ExecutionStatus::Deferred => "deferred",
    }
}

fn parse_status(value: &str) -> Result<ExecutionStatus, ExecutionRepositoryError> {
    match value {
        "active" => Ok(ExecutionStatus::Active),
        "paused" => Ok(ExecutionStatus::Paused),
        "completed" => Ok(ExecutionStatus::Completed),
        "skipped" => Ok(ExecutionStatus::Skipped),
        "deferred" => Ok(ExecutionStatus::Deferred),
        _ => Err(ExecutionRepositoryError::Internal),
    }
}

fn positive_or_zero_revision(value: i64) -> Result<u64, ExecutionRepositoryError> {
    u64::try_from(value).map_err(|_| ExecutionRepositoryError::Internal)
}

fn positive_revision(value: i64) -> Result<u64, ExecutionRepositoryError> {
    let value = positive_or_zero_revision(value)?;
    if value == 0 {
        Err(ExecutionRepositoryError::Internal)
    } else {
        Ok(value)
    }
}

fn nonnegative_seconds(value: i64) -> Result<u64, ExecutionRepositoryError> {
    u64::try_from(value).map_err(|_| ExecutionRepositoryError::Internal)
}

fn revision_to_i64(value: u64) -> Result<i64, ExecutionRepositoryError> {
    i64::try_from(value).map_err(|_| ExecutionRepositoryError::Internal)
}

fn seconds_to_i64(value: u64) -> Result<i64, ExecutionRepositoryError> {
    i64::try_from(value).map_err(|_| ExecutionRepositoryError::Internal)
}

fn internal(_: sqlx::Error) -> ExecutionRepositoryError {
    ExecutionRepositoryError::Internal
}

fn map_defer_assessment_insert(error: &sqlx::Error) -> ExecutionRepositoryError {
    if has_sqlstate(error, "DW001") {
        ExecutionRepositoryError::DeferAssessmentStale
    } else {
        ExecutionRepositoryError::Internal
    }
}

fn map_defer_claim_insert(error: &sqlx::Error) -> ExecutionRepositoryError {
    if has_sqlstate(error, "DW002") {
        ExecutionRepositoryError::DeferAssessmentStale
    } else {
        ExecutionRepositoryError::Internal
    }
}

fn has_sqlstate(error: &sqlx::Error, expected: &str) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::{
        map_schedule_assessment_error, map_schedule_authorization_error,
        normalized_source_credit_seconds,
    };
    use crate::{execution::ExecutionRepositoryError, scheduling::SchedulePublicationError};

    #[test]
    fn defer_credit_rounds_the_aggregate_once_across_fractional_history() {
        assert_eq!(
            normalized_source_credit_seconds(61, 61 + 59, 3_600).unwrap(),
            0,
            "61s and then 59s are two aggregate minutes, not three separately rounded minutes",
        );
        assert_eq!(
            normalized_source_credit_seconds(59, 59 + 2, 3_600).unwrap(),
            60,
            "crossing one aggregate minute boundary credits exactly one minute",
        );
        assert_eq!(
            normalized_source_credit_seconds(0, 601, 3_600).unwrap(),
            660,
        );
    }

    #[test]
    fn changed_policy_evidence_is_stale_only_after_an_assessment_exists() {
        for error in [
            SchedulePublicationError::StaleComposition,
            SchedulePublicationError::DeferredPlacementRequired,
            SchedulePublicationError::InvalidPayload,
        ] {
            assert_eq!(
                map_schedule_assessment_error(error),
                ExecutionRepositoryError::ScheduleStale,
            );
            assert_eq!(
                map_schedule_authorization_error(error),
                ExecutionRepositoryError::DeferAssessmentStale,
            );
        }
    }
}
