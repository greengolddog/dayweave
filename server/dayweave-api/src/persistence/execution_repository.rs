use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::execution::{
    ExecutionCommand, ExecutionIdempotency, ExecutionMutation, ExecutionRepository,
    ExecutionRepositoryError, ExecutionSession, ExecutionSnapshot, ExecutionStatus,
    next_protocol_time,
};

use super::DatabaseScope;

const IDEMPOTENCY_NAMESPACE: &str = "execution.command";
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
            self.scope.workspace_id,
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

async fn apply_command_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    active_session_id: Option<Uuid>,
    command: &ExecutionCommand,
    transition_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
) -> Result<ExecutionSession, ExecutionRepositoryError> {
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
    let current = fetch_session_transaction(transaction, workspace_id, requested_id).await?;
    let updated = current.apply_with_protocol_time(command, transition_at, observed_at)?;
    update_session(transaction, workspace_id, &updated).await?;
    if matches!(command, ExecutionCommand::Defer(_)) {
        insert_defer_replacement_claim(transaction, workspace_id, &updated).await?;
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
        "SELECT revision, execution_epoch, status, trashed_at FROM items \
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
        || matches!(status.as_str(), "completed" | "skipped" | "cancelled")
    {
        Err(ExecutionRepositoryError::ItemNotExecutable)
    } else {
        Ok(execution_epoch)
    }
}

async fn insert_defer_replacement_claim(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    session: &ExecutionSession,
) -> Result<(), ExecutionRepositoryError> {
    let evidence = sqlx::query(
        "SELECT session.execution_epoch, origin.planned_duration_seconds \
         FROM execution_sessions AS session \
         LEFT JOIN execution_session_schedule_origins AS origin \
           ON origin.workspace_id = session.workspace_id \
          AND origin.execution_session_id = session.id \
         WHERE session.workspace_id = $1 AND session.id = $2",
    )
    .bind(workspace_id)
    .bind(session.id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    let execution_epoch: i64 = evidence.try_get("execution_epoch").map_err(internal)?;
    if execution_epoch <= 0 {
        return Err(ExecutionRepositoryError::Internal);
    }
    let origin_duration: Option<i64> = evidence
        .try_get("planned_duration_seconds")
        .map_err(internal)?;
    let move_start = session
        .move_start
        .ok_or(ExecutionRepositoryError::Internal)?;
    let move_end = session.move_end.ok_or(ExecutionRepositoryError::Internal)?;
    let move_duration = exact_window_seconds(move_start, move_end)?;
    let actual_seconds = seconds_to_i64(
        session
            .actual_seconds
            .ok_or(ExecutionRepositoryError::Internal)?,
    )?;
    let (planned_duration, duration_source, consumed_by_source) =
        if let Some(planned_duration) = origin_duration {
            if planned_duration <= 0 {
                return Err(ExecutionRepositoryError::Internal);
            }
            (
                planned_duration,
                "published_origin",
                actual_seconds.min(planned_duration),
            )
        } else {
            (move_duration, "legacy_move_window", 0)
        };
    let remaining_duration = planned_duration
        .checked_sub(consumed_by_source)
        .ok_or(ExecutionRepositoryError::Internal)?;
    if origin_duration.is_some() && (remaining_duration <= 0 || remaining_duration != move_duration)
    {
        return Err(ExecutionRepositoryError::DeferDurationConflict);
    }

    let replacement_session_index = next_replacement_session_index(
        transaction,
        workspace_id,
        session.item_id,
        session.occurrence_id,
    )
    .await?;

    let inserted = sqlx::query(
        "INSERT INTO execution_defer_replacement_claims (workspace_id, \
         source_deferred_session_id, item_id, source_item_revision, execution_epoch, \
         occurrence_id, source_session_index, replacement_session_index, \
         planned_duration_seconds, planned_duration_source, actionable, consumed_before_seconds, \
         consumed_by_source_seconds, remaining_duration_seconds, move_start, move_end, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, true, 0, $11, $12, $13, $14, $15)",
    )
    .bind(workspace_id)
    .bind(session.id)
    .bind(session.item_id)
    .bind(revision_to_i64(session.item_revision)?)
    .bind(execution_epoch)
    .bind(session.occurrence_id)
    .bind(i32::from(session.session_index))
    .bind(replacement_session_index)
    .bind(planned_duration)
    .bind(duration_source)
    .bind(consumed_by_source)
    .bind(remaining_duration)
    .bind(move_start)
    .bind(move_end)
    .bind(session.updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
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
