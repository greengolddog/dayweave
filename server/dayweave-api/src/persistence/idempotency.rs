use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use super::DatabaseScope;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdempotencyDecision {
    Acquired,
    InProgress,
    Replay {
        resource_id: Option<Uuid>,
        response: Option<Value>,
    },
    Conflict,
}

#[derive(Clone, Debug)]
pub struct PostgresIdempotencyRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresIdempotencyRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }

    /// Atomically reserves a key, or reports the durable state of an existing
    /// reservation. Raw keys are hashed before persistence.
    ///
    /// # Errors
    ///
    /// Returns a validation or storage error without exposing database details.
    pub async fn reserve(
        &self,
        namespace: &str,
        raw_key: &str,
        request_fingerprint: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<IdempotencyDecision, IdempotencyError> {
        validate(namespace, raw_key, request_fingerprint, expires_at)?;
        let key_hash = hash_key(raw_key);
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        sqlx::query(
            "DELETE FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
             AND key_hash = $3 AND expires_at <= clock_timestamp()",
        )
        .bind(self.scope.workspace_id)
        .bind(namespace)
        .bind(key_hash.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let inserted = sqlx::query(
            "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, \
             request_fingerprint, expires_at) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (workspace_id, namespace, key_hash) DO NOTHING",
        )
        .bind(self.scope.workspace_id)
        .bind(namespace)
        .bind(key_hash.as_slice())
        .bind(request_fingerprint)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?
        .rows_affected();
        if inserted == 1 {
            transaction.commit().await.map_err(storage)?;
            return Ok(IdempotencyDecision::Acquired);
        }

        let row = sqlx::query(
            "SELECT request_fingerprint, state, resource_id, response_json \
             FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
             AND key_hash = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(namespace)
        .bind(key_hash.as_slice())
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage)?;
        let stored_fingerprint: Vec<u8> = row.try_get("request_fingerprint").map_err(storage)?;
        let state: String = row.try_get("state").map_err(storage)?;
        let decision = if stored_fingerprint != request_fingerprint {
            IdempotencyDecision::Conflict
        } else if state == "completed" {
            IdempotencyDecision::Replay {
                resource_id: row.try_get("resource_id").map_err(storage)?,
                response: row.try_get("response_json").map_err(storage)?,
            }
        } else {
            IdempotencyDecision::InProgress
        };
        transaction.commit().await.map_err(storage)?;
        Ok(decision)
    }

    /// Marks a matching reservation complete and records a replay-safe result.
    ///
    /// # Errors
    ///
    /// Returns `NotFound`, `Conflict`, or a redacted storage error.
    pub async fn complete(
        &self,
        namespace: &str,
        raw_key: &str,
        request_fingerprint: &[u8],
        resource_type: Option<&str>,
        resource_id: Option<Uuid>,
        response: Option<&Value>,
    ) -> Result<(), IdempotencyError> {
        if namespace.trim().is_empty()
            || raw_key.is_empty()
            || request_fingerprint.len() < 16
            || resource_type.is_some_and(|value| value.trim().is_empty())
        {
            return Err(IdempotencyError::InvalidInput);
        }
        let key_hash = hash_key(raw_key);
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let stored = sqlx::query(
            "SELECT request_fingerprint, state, resource_type, resource_id, response_json \
             FROM idempotency_keys \
             WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 \
             AND expires_at > clock_timestamp() FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(namespace)
        .bind(key_hash.as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?;
        let Some(stored) = stored else {
            return Err(IdempotencyError::NotFound);
        };
        let stored_fingerprint: Vec<u8> = stored.try_get("request_fingerprint").map_err(storage)?;
        if stored_fingerprint != request_fingerprint {
            return Err(IdempotencyError::Conflict);
        }
        let state: String = stored.try_get("state").map_err(storage)?;
        if state == "completed" {
            let stored_resource_type: Option<String> =
                stored.try_get("resource_type").map_err(storage)?;
            let stored_resource_id: Option<Uuid> =
                stored.try_get("resource_id").map_err(storage)?;
            let stored_response: Option<Value> =
                stored.try_get("response_json").map_err(storage)?;
            if stored_resource_type.as_deref() == resource_type
                && stored_resource_id == resource_id
                && stored_response.as_ref() == response
            {
                transaction.commit().await.map_err(storage)?;
                return Ok(());
            }
            return Err(IdempotencyError::Conflict);
        }
        sqlx::query(
            "UPDATE idempotency_keys SET state = 'completed', resource_type = $4, \
             resource_id = $5, response_json = $6, updated_at = clock_timestamp() \
             WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(namespace)
        .bind(key_hash.as_slice())
        .bind(resource_type)
        .bind(resource_id)
        .bind(response)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        transaction.commit().await.map_err(storage)
    }

    /// Removes expired keys in bounded batches.
    ///
    /// # Errors
    ///
    /// Returns a redacted storage error.
    pub async fn purge_expired(&self, limit: u32) -> Result<u64, IdempotencyError> {
        if limit == 0 || limit > 10_000 {
            return Err(IdempotencyError::InvalidInput);
        }
        sqlx::query(
            "DELETE FROM idempotency_keys WHERE ctid IN (SELECT ctid FROM idempotency_keys \
             WHERE workspace_id = $1 AND expires_at <= clock_timestamp() \
             ORDER BY expires_at LIMIT $2)",
        )
        .bind(self.scope.workspace_id)
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(storage)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum IdempotencyError {
    #[error("invalid idempotency input")]
    InvalidInput,
    #[error("idempotency reservation was not found or expired")]
    NotFound,
    #[error("idempotency key was used for different content")]
    Conflict,
    #[error("idempotency storage operation failed")]
    Storage,
}

fn validate(
    namespace: &str,
    raw_key: &str,
    request_fingerprint: &[u8],
    expires_at: DateTime<Utc>,
) -> Result<(), IdempotencyError> {
    if namespace.trim().is_empty()
        || namespace.len() > 100
        || raw_key.len() < 8
        || raw_key.len() > 500
        || request_fingerprint.len() < 16
        || expires_at <= Utc::now()
    {
        Err(IdempotencyError::InvalidInput)
    } else {
        Ok(())
    }
}

fn hash_key(raw_key: &str) -> [u8; 32] {
    Sha256::digest(raw_key.as_bytes()).into()
}

fn storage<T>(_error: T) -> IdempotencyError {
    IdempotencyError::Storage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable_and_do_not_retain_the_raw_key() {
        let first = hash_key("conversation-key-123");
        let second = hash_key("conversation-key-123");
        assert_eq!(first, second);
        assert_ne!(first.as_slice(), b"conversation-key-123");
    }
}
