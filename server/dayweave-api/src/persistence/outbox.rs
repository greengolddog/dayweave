use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};
use thiserror::Error;
use uuid::Uuid;

use super::DatabaseScope;

#[derive(Clone, Debug)]
pub struct NewOutboxMessage {
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub aggregate_revision: Option<u64>,
    pub event_type: String,
    pub deduplication_key: Option<String>,
    pub payload: Value,
    pub headers: Value,
    pub available_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxMessage {
    pub id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub aggregate_revision: Option<u64>,
    pub event_type: String,
    pub payload: Value,
    pub headers: Value,
    pub attempts: u32,
    pub available_at: DateTime<Utc>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct PostgresOutboxRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresOutboxRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }

    /// Enqueues one event. Callers requiring atomic aggregate persistence should
    /// insert through the aggregate repository, which writes its outbox event in
    /// the same transaction.
    ///
    /// # Errors
    ///
    /// Returns a validation, duplicate, or redacted storage error.
    pub async fn enqueue(&self, message: NewOutboxMessage) -> Result<Uuid, OutboxError> {
        validate_message(&message)?;
        let id = Uuid::new_v4();
        let revision = message
            .aggregate_revision
            .map(i64::try_from)
            .transpose()
            .map_err(|_| OutboxError::InvalidInput)?;
        let result = sqlx::query(
            "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, \
             aggregate_revision, event_type, deduplication_key, payload, headers, available_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(self.scope.workspace_id)
        .bind(&message.aggregate_type)
        .bind(message.aggregate_id)
        .bind(revision)
        .bind(&message.event_type)
        .bind(&message.deduplication_key)
        .bind(&message.payload)
        .bind(&message.headers)
        .bind(message.available_at)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(id),
            Err(error) if is_unique_violation(&error) => Err(OutboxError::Duplicate),
            Err(_) => Err(OutboxError::Storage),
        }
    }

    /// Claims a bounded delivery batch using `FOR UPDATE SKIP LOCKED`.
    ///
    /// # Errors
    ///
    /// Returns a validation or redacted storage error.
    pub async fn claim_batch(
        &self,
        worker_id: &str,
        limit: u32,
        lease: Duration,
    ) -> Result<Vec<OutboxMessage>, OutboxError> {
        if worker_id.trim().is_empty()
            || worker_id.chars().count() > 200
            || limit == 0
            || limit > 1_000
            || lease.is_zero()
            || lease > Duration::from_hours(1)
        {
            return Err(OutboxError::InvalidInput);
        }
        let lease = chrono::Duration::from_std(lease).map_err(|_| OutboxError::InvalidInput)?;
        let stale_before = Utc::now() - lease;
        let rows = sqlx::query(
            "WITH candidates AS ( \
                SELECT id FROM outbox_messages \
                WHERE workspace_id = $1 AND published_at IS NULL \
                  AND available_at <= clock_timestamp() \
                  AND (claimed_at IS NULL OR claimed_at < $2) \
                ORDER BY available_at, created_at, id \
                FOR UPDATE SKIP LOCKED LIMIT $3 \
             ) \
             UPDATE outbox_messages AS messages \
             SET claimed_by = $4, claimed_at = clock_timestamp(), attempts = attempts + 1, \
                 updated_at = clock_timestamp() \
             FROM candidates WHERE messages.id = candidates.id \
             RETURNING messages.id, messages.aggregate_type, messages.aggregate_id, \
                 messages.aggregate_revision, messages.event_type, messages.payload, \
                 messages.headers, messages.attempts, messages.available_at, \
                 messages.claimed_by, messages.claimed_at, messages.created_at",
        )
        .bind(self.scope.workspace_id)
        .bind(stale_before)
        .bind(i64::from(limit))
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(message_from_row).collect()
    }

    /// Marks a message delivered by the worker holding its current lease.
    ///
    /// # Errors
    ///
    /// Returns `LeaseLost` if another worker owns the message.
    pub async fn mark_published(&self, id: Uuid, worker_id: &str) -> Result<(), OutboxError> {
        if worker_id.trim().is_empty() {
            return Err(OutboxError::InvalidInput);
        }
        let affected = sqlx::query(
            "UPDATE outbox_messages SET published_at = clock_timestamp(), \
             updated_at = clock_timestamp(), last_error_code = NULL \
             WHERE workspace_id = $1 AND id = $2 AND claimed_by = $3 AND published_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(id)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(storage)?
        .rows_affected();
        if affected == 1 {
            Ok(())
        } else {
            Err(OutboxError::LeaseLost)
        }
    }

    /// Releases a failed delivery for a later retry while storing only a
    /// bounded error code, never provider response content.
    ///
    /// # Errors
    ///
    /// Returns validation, lease, or redacted storage errors.
    pub async fn mark_failed(
        &self,
        id: Uuid,
        worker_id: &str,
        error_code: &str,
        retry_at: DateTime<Utc>,
    ) -> Result<(), OutboxError> {
        if worker_id.trim().is_empty()
            || error_code.trim().is_empty()
            || error_code.chars().count() > 100
            || retry_at <= Utc::now()
        {
            return Err(OutboxError::InvalidInput);
        }
        let affected = sqlx::query(
            "UPDATE outbox_messages SET claimed_by = NULL, claimed_at = NULL, \
             available_at = $4, last_error_code = $5, updated_at = clock_timestamp() \
             WHERE workspace_id = $1 AND id = $2 AND claimed_by = $3 AND published_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(id)
        .bind(worker_id)
        .bind(retry_at)
        .bind(error_code)
        .execute(&self.pool)
        .await
        .map_err(storage)?
        .rows_affected();
        if affected == 1 {
            Ok(())
        } else {
            Err(OutboxError::LeaseLost)
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum OutboxError {
    #[error("invalid outbox input")]
    InvalidInput,
    #[error("outbox deduplication key already exists")]
    Duplicate,
    #[error("outbox message lease was lost")]
    LeaseLost,
    #[error("outbox storage operation failed")]
    Storage,
}

fn validate_message(message: &NewOutboxMessage) -> Result<(), OutboxError> {
    if message.aggregate_type.trim().is_empty()
        || message.aggregate_type.chars().count() > 100
        || message.event_type.trim().is_empty()
        || message.event_type.chars().count() > 150
        || message
            .deduplication_key
            .as_ref()
            .is_some_and(|key| key.is_empty() || key.chars().count() > 500)
        || !message.payload.is_object()
        || !message.headers.is_object()
    {
        Err(OutboxError::InvalidInput)
    } else {
        Ok(())
    }
}

fn message_from_row(row: &PgRow) -> Result<OutboxMessage, OutboxError> {
    let revision: Option<i64> = row.try_get("aggregate_revision").map_err(storage)?;
    let attempts: i32 = row.try_get("attempts").map_err(storage)?;
    Ok(OutboxMessage {
        id: row.try_get("id").map_err(storage)?,
        aggregate_type: row.try_get("aggregate_type").map_err(storage)?,
        aggregate_id: row.try_get("aggregate_id").map_err(storage)?,
        aggregate_revision: revision
            .map(u64::try_from)
            .transpose()
            .map_err(|_| OutboxError::Storage)?,
        event_type: row.try_get("event_type").map_err(storage)?,
        payload: row.try_get("payload").map_err(storage)?,
        headers: row.try_get("headers").map_err(storage)?,
        attempts: u32::try_from(attempts).map_err(|_| OutboxError::Storage)?,
        available_at: row.try_get("available_at").map_err(storage)?,
        claimed_by: row.try_get("claimed_by").map_err(storage)?,
        claimed_at: row.try_get("claimed_at").map_err(storage)?,
        created_at: row.try_get("created_at").map_err(storage)?,
    })
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

fn storage<T>(_error: T) -> OutboxError {
    OutboxError::Storage
}
