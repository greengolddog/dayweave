use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::execution::{
    ExecutionCommand, ExecutionIdempotency, ExecutionMutation, ExecutionRepository,
    ExecutionRepositoryError, ExecutionSession, ExecutionSnapshot, ExecutionStatus,
};

use super::DatabaseScope;

const IDEMPOTENCY_NAMESPACE: &str = "execution.command";
const HISTORY_SELECT: &str = "SELECT id, item_id, item_revision, occurrence_id, session_index, \
    planned_block_id, source_device_id, state, revision, accumulated_seconds, actual_seconds, \
    started_at, running_since, paused_at, pause_until, pause_reason, ended_at, created_at, updated_at \
    FROM execution_sessions WHERE workspace_id = $1 \
    ORDER BY updated_at DESC, id DESC LIMIT $2 OFFSET $3";
const SESSION_BY_ID: &str = "SELECT id, item_id, item_revision, occurrence_id, session_index, \
    planned_block_id, source_device_id, state, revision, accumulated_seconds, actual_seconds, \
    started_at, running_since, paused_at, pause_until, pause_reason, ended_at, created_at, updated_at \
    FROM execution_sessions WHERE workspace_id = $1 AND id = $2";
const SESSION_BY_ID_FOR_UPDATE: &str = "SELECT id, item_id, item_revision, occurrence_id, \
    session_index, planned_block_id, source_device_id, state, revision, accumulated_seconds, \
    actual_seconds, started_at, running_since, paused_at, pause_until, pause_reason, ended_at, \
    created_at, updated_at FROM execution_sessions \
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
            "SELECT revision, active_session_id FROM execution_state \
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

        let changed_session = apply_command_transaction(
            &mut transaction,
            self.scope.workspace_id,
            active_session_id,
            &command,
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
        .bind(now)
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
    now: DateTime<Utc>,
) -> Result<ExecutionSession, ExecutionRepositoryError> {
    if let ExecutionCommand::Start(input) = command {
        if active_session_id.is_some() {
            return Err(ExecutionRepositoryError::ActiveSessionConflict);
        }
        lock_workspace_items(transaction, workspace_id).await?;
        validate_start_item(
            transaction,
            workspace_id,
            input.item_id,
            input.item_revision,
        )
        .await?;
        if session_exists(transaction, input.session_id).await? {
            return Err(ExecutionRepositoryError::DuplicateSession(input.session_id));
        }
        let session = ExecutionSession::start(input, now);
        insert_session(transaction, workspace_id, &session).await?;
        return Ok(session);
    }

    let requested_id = command.session_id();
    if active_session_id != Some(requested_id) {
        return Err(ExecutionRepositoryError::SessionNotFound(requested_id));
    }
    let current = fetch_session_transaction(transaction, workspace_id, requested_id).await?;
    let updated = current.apply(command, now)?;
    update_session(transaction, workspace_id, &updated).await?;
    Ok(updated)
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
) -> Result<(), ExecutionRepositoryError> {
    let row = sqlx::query(
        "SELECT revision, status, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ExecutionRepositoryError::ItemNotExecutable)?;
    let revision = positive_revision(row.try_get("revision").map_err(internal)?)?;
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
        Ok(())
    }
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
    session: &ExecutionSession,
) -> Result<(), ExecutionRepositoryError> {
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, occurrence_id, \
         session_index, planned_block_id, source_device_id, state, revision, accumulated_seconds, \
         actual_seconds, started_at, running_since, paused_at, pause_until, pause_reason, ended_at, \
         created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, \
         $13, $14, $15, $16, $17, $18, $19, $20)",
    )
    .bind(session.id)
    .bind(workspace_id)
    .bind(session.item_id)
    .bind(revision_to_i64(session.item_revision)?)
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
    .bind(session.paused_at)
    .bind(session.pause_until)
    .bind(&session.pause_reason)
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
         actual_seconds = $6, running_since = $7, paused_at = $8, pause_until = $9, \
         pause_reason = $10, ended_at = $11, updated_at = $12 \
         WHERE workspace_id = $1 AND id = $2 AND revision = $13",
    )
    .bind(workspace_id)
    .bind(session.id)
    .bind(status_name(session.status))
    .bind(revision_to_i64(session.revision)?)
    .bind(seconds_to_i64(session.accumulated_seconds)?)
    .bind(session.actual_seconds.map(seconds_to_i64).transpose()?)
    .bind(session.running_since)
    .bind(session.paused_at)
    .bind(session.pause_until)
    .bind(&session.pause_reason)
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
        paused_at: row.try_get("paused_at").map_err(internal)?,
        pause_until: row.try_get("pause_until").map_err(internal)?,
        pause_reason: row.try_get("pause_reason").map_err(internal)?,
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
    }
}

fn parse_status(value: &str) -> Result<ExecutionStatus, ExecutionRepositoryError> {
    match value {
        "active" => Ok(ExecutionStatus::Active),
        "paused" => Ok(ExecutionStatus::Paused),
        "completed" => Ok(ExecutionStatus::Completed),
        "skipped" => Ok(ExecutionStatus::Skipped),
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
