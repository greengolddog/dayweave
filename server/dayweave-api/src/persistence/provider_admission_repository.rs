//! Durable ownership of active provider operations across runtimes.
//!
//! No operation expires automatically. An uncertain/crashed operation remains
//! visible and prevents fencing until its exact runtime explicitly settles it.
//! These detached rows contain no provider payloads or credentials and are not
//! removed by tenant purge. Database transactions protect only registry changes;
//! callers wait for provider work outside these transactions.

use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    account_deletion::AccountDeletionRepositoryError, google_oauth::OAuthScope,
    provider_admission::ProviderAdmissionError,
};

use super::DatabaseScope;

#[derive(Clone)]
pub(crate) struct PostgresProviderAdmissionRepository {
    pool: PgPool,
    scope: OAuthScope,
}

impl std::fmt::Debug for PostgresProviderAdmissionRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresProviderAdmissionRepository")
            .finish_non_exhaustive()
    }
}

impl PostgresProviderAdmissionRepository {
    pub(crate) fn new(pool: PgPool, scope: OAuthScope) -> Self {
        Self { pool, scope }
    }

    pub(crate) async fn register(
        &self,
        runtime_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ProviderAdmissionError> {
        validate_ids(self.scope, &[runtime_id, operation_id])?;
        let mut transaction = self.begin_scoped().await?;
        self.ensure_scope(&mut transaction).await?;
        let closed = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT closed_for_deletion_id FROM provider_admission_scopes \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_error)?;
        if closed.is_some() {
            return Err(ProviderAdmissionError::Closed);
        }
        let authority = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM workspaces AS workspace \
                 JOIN users AS owner ON owner.id = workspace.owner_user_id \
                 JOIN workspace_members AS member \
                     ON member.workspace_id = workspace.id AND member.user_id = owner.id \
                 WHERE workspace.id = $1 AND owner.id = $2 \
                     AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL \
                     AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL \
                     AND member.role = 'owner' AND member.removed_at IS NULL) AS current_owner, \
             (EXISTS(SELECT 1 FROM account_deletion_fences \
                 WHERE workspace_id = $1 OR user_id = $2) \
              OR EXISTS(SELECT 1 FROM provider_admission_scopes \
                 WHERE (workspace_id = $1 OR user_id = $2) \
                     AND closed_for_deletion_id IS NOT NULL)) AS fenced",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_error)?;
        if authority.try_get::<bool, _>("fenced").map_err(map_error)? {
            return Err(ProviderAdmissionError::Closed);
        }
        if !authority
            .try_get::<bool, _>("current_owner")
            .map_err(map_error)?
        {
            return Err(ProviderAdmissionError::InvalidScope);
        }
        if let Some(row) = sqlx::query(
            "SELECT workspace_id, user_id, runtime_id FROM provider_admission_operations \
             WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_error)?
        {
            if !matches_owner(&row, self.scope, runtime_id)? {
                return Err(ProviderAdmissionError::WrongOperation);
            }
        } else {
            sqlx::query(
                "INSERT INTO provider_admission_operations \
                 (workspace_id, user_id, runtime_id, operation_id) VALUES ($1, $2, $3, $4)",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(runtime_id)
            .bind(operation_id)
            .execute(&mut *transaction)
            .await
            .map_err(map_error)?;
        }
        transaction.commit().await.map_err(map_error)
    }

    pub(crate) async fn settle(
        &self,
        runtime_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ProviderAdmissionError> {
        validate_ids(self.scope, &[runtime_id, operation_id])?;
        let mut transaction = self.begin_scoped().await?;
        let row = sqlx::query(
            "SELECT workspace_id, user_id, runtime_id FROM provider_admission_operations \
             WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_error)?;
        if let Some(row) = row {
            if !matches_owner(&row, self.scope, runtime_id)? {
                return Err(ProviderAdmissionError::WrongOperation);
            }
            sqlx::query(
                "DELETE FROM provider_admission_operations \
                 WHERE workspace_id = $1 AND user_id = $2 AND runtime_id = $3 AND operation_id = $4",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(runtime_id)
            .bind(operation_id)
            .execute(&mut *transaction)
            .await
            .map_err(map_error)?;
        }
        // A lost settlement response may replay after the row was removed.
        // Missing active-only state requires no new row or history receipt.
        transaction.commit().await.map_err(map_error)
    }

    pub(crate) async fn close(&self, deletion_id: Uuid) -> Result<(), ProviderAdmissionError> {
        validate_ids(self.scope, &[deletion_id])?;
        let mut transaction = self.begin_scoped().await?;
        self.ensure_scope(&mut transaction).await?;
        let valid_deletion = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM account_deletion_lifecycles \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND status <> 'cancelled')",
        )
        .bind(deletion_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_error)?;
        if !valid_deletion {
            return Err(ProviderAdmissionError::ConflictingDeletion);
        }
        let closed = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT closed_for_deletion_id FROM provider_admission_scopes \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_error)?;
        match closed {
            Some(existing) if existing != deletion_id => {
                return Err(ProviderAdmissionError::ConflictingDeletion);
            }
            Some(_) => {}
            None => {
                sqlx::query(
                    "UPDATE provider_admission_scopes \
                     SET closed_for_deletion_id = $3, closed_at = clock_timestamp() \
                     WHERE workspace_id = $1 AND user_id = $2",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(deletion_id)
                .execute(&mut *transaction)
                .await
                .map_err(map_error)?;
            }
        }
        transaction.commit().await.map_err(map_error)
    }

    pub(crate) async fn is_drained(
        &self,
        deletion_id: Uuid,
    ) -> Result<bool, ProviderAdmissionError> {
        validate_ids(self.scope, &[deletion_id])?;
        let row = sqlx::query(
            "SELECT admission.closed_for_deletion_id, \
             EXISTS(SELECT 1 FROM account_deletion_lifecycles AS lifecycle \
                 WHERE lifecycle.id = $3 AND lifecycle.workspace_id = $1 \
                     AND lifecycle.user_id = $2 AND lifecycle.status <> 'cancelled') \
                 AS valid_deletion, \
             NOT EXISTS(SELECT 1 FROM provider_admission_operations AS operation \
                 WHERE operation.workspace_id = $1 OR operation.user_id = $2) AS drained \
             FROM provider_admission_scopes AS admission \
             WHERE admission.workspace_id = $1 AND admission.user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(deletion_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_error)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let closed: Option<Uuid> = row.try_get("closed_for_deletion_id").map_err(map_error)?;
        if closed.is_some_and(|closed| closed != deletion_id)
            || !row
                .try_get::<bool, _>("valid_deletion")
                .map_err(map_error)?
        {
            return Err(ProviderAdmissionError::ConflictingDeletion);
        }
        Ok(closed == Some(deletion_id) && row.try_get("drained").map_err(map_error)?)
    }

    async fn begin_scoped(&self) -> Result<Transaction<'_, Postgres>, ProviderAdmissionError> {
        let mut transaction = self.pool.begin().await.map_err(map_error)?;
        // Separate statements make the global-before-registry-before-scope
        // order explicit. The brief registry mutex also serializes overlapping
        // user/workspace pairs after a canonical ownership change.
        sqlx::query(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended(\
             'dayweave.account-deletion.global-mutation-barrier.v1', 0))",
        )
        .execute(&mut *transaction)
        .await
        .map_err(map_error)?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(\
             'dayweave.provider-admission.global-registry.v1', 0))",
        )
        .execute(&mut *transaction)
        .await
        .map_err(map_error)?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(\
             'dayweave.provider-admission.scope.v1:' || $1::uuid::text || ':' || $2::uuid::text, 0))",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_error)?;
        Ok(transaction)
    }

    async fn ensure_scope(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<(), ProviderAdmissionError> {
        // Do not execute INSERT triggers on exact replay after tenant purge.
        sqlx::query(
            "INSERT INTO provider_admission_scopes (workspace_id, user_id) \
             SELECT $1, $2 WHERE NOT EXISTS(SELECT 1 FROM provider_admission_scopes \
                 WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .execute(&mut **transaction)
        .await
        .map_err(map_error)?;
        Ok(())
    }
}

/// Serializes an authorized deletion-preparation transaction with every
/// provider registration and tenant mutation, before examining closure state.
/// Callers lock the lifecycle with NO KEY UPDATE first, as the final fence does.
pub(super) async fn lock_provider_admission_for_deletion(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    deletion_id: Uuid,
) -> Result<bool, AccountDeletionRepositoryError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.global-mutation-barrier.v1', 0))",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.provider-admission.global-registry.v1', 0))",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.provider-admission.scope.v1:' || $1::uuid::text || ':' || $2::uuid::text, 0))",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    let row = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM provider_admission_scopes \
             WHERE workspace_id = $1 AND user_id = $2 \
                 AND closed_for_deletion_id = $3) AS closed, \
         EXISTS(SELECT 1 FROM provider_admission_scopes \
             WHERE (workspace_id = $1 OR user_id = $2) \
                 AND closed_for_deletion_id <> $3) AS conflicting",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(deletion_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    if row
        .try_get::<bool, _>("conflicting")
        .map_err(|_| AccountDeletionRepositoryError::Internal)?
    {
        return Err(AccountDeletionRepositoryError::Conflict);
    }
    row.try_get("closed")
        .map_err(|_| AccountDeletionRepositoryError::Internal)
}

/// Persists closure in the same short transaction as the caller's complete
/// provider-readiness and fresh-authority checks. This never closes a local
/// controller, waits for provider work, or settles an operation registration.
pub(super) async fn close_provider_admission_for_deletion(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    deletion_id: Uuid,
) -> Result<(), AccountDeletionRepositoryError> {
    let closed = lock_provider_admission_for_deletion(transaction, scope, deletion_id).await?;
    let unsettled = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM provider_admission_operations \
         WHERE workspace_id = $1 OR user_id = $2)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    if unsettled {
        return Err(AccountDeletionRepositoryError::ProviderCleanupBlocked);
    }
    if !closed {
        sqlx::query(
            "INSERT INTO provider_admission_scopes (workspace_id, user_id) \
             SELECT $1, $2 WHERE NOT EXISTS(SELECT 1 FROM provider_admission_scopes \
                 WHERE workspace_id = $1 AND user_id = $2)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(&mut **transaction)
        .await
        .map_err(deletion_error)?;
        let changed = sqlx::query(
            "UPDATE provider_admission_scopes \
             SET closed_for_deletion_id = $3, closed_at = clock_timestamp() \
             WHERE workspace_id = $1 AND user_id = $2 AND closed_for_deletion_id IS NULL",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(deletion_id)
        .execute(&mut **transaction)
        .await
        .map_err(deletion_error)?
        .rows_affected();
        if changed != 1 {
            return Err(AccountDeletionRepositoryError::Conflict);
        }
    }
    Ok(())
}

/// Rechecks the authoritative registry in the final, short fence transaction.
/// Acquiring the existing exclusive barrier also protects callers that have not
/// acquired it yet. This function never waits for an active provider to finish.
pub(crate) async fn ensure_provider_admission_drained(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    deletion_id: Uuid,
) -> Result<(), AccountDeletionRepositoryError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.global-mutation-barrier.v1', 0))",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    let drained = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM provider_admission_scopes AS admission \
             JOIN account_deletion_lifecycles AS lifecycle \
                 ON lifecycle.id = admission.closed_for_deletion_id \
             WHERE admission.workspace_id = $1 AND admission.user_id = $2 \
                 AND admission.closed_for_deletion_id = $3 \
                 AND lifecycle.workspace_id = $1 AND lifecycle.user_id = $2 \
                 AND lifecycle.status <> 'cancelled') \
         AND NOT EXISTS(SELECT 1 FROM provider_admission_operations \
             WHERE workspace_id = $1 OR user_id = $2)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(deletion_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AccountDeletionRepositoryError::Internal)?;
    if drained {
        Ok(())
    } else {
        Err(AccountDeletionRepositoryError::ProviderCleanupBlocked)
    }
}

fn validate_ids(scope: OAuthScope, ids: &[Uuid]) -> Result<(), ProviderAdmissionError> {
    if scope.workspace_id.is_nil() || scope.user_id.is_nil() || ids.iter().any(Uuid::is_nil) {
        Err(ProviderAdmissionError::InvalidScope)
    } else {
        Ok(())
    }
}

fn matches_owner(
    row: &sqlx::postgres::PgRow,
    scope: OAuthScope,
    runtime_id: Uuid,
) -> Result<bool, ProviderAdmissionError> {
    Ok(
        row.try_get::<Uuid, _>("workspace_id").map_err(map_error)? == scope.workspace_id
            && row.try_get::<Uuid, _>("user_id").map_err(map_error)? == scope.user_id
            && row.try_get::<Uuid, _>("runtime_id").map_err(map_error)? == runtime_id,
    )
}

fn map_error(error: sqlx::Error) -> ProviderAdmissionError {
    let mapped = match error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
    {
        Some("DWADM" | "DWDEL") => ProviderAdmissionError::Closed,
        Some("DWSCP") => ProviderAdmissionError::InvalidScope,
        Some("DWOPR" | "23505") => ProviderAdmissionError::WrongOperation,
        Some("DWCON") => ProviderAdmissionError::ConflictingDeletion,
        _ => ProviderAdmissionError::Unavailable,
    };
    drop(error);
    mapped
}

fn deletion_error(error: sqlx::Error) -> AccountDeletionRepositoryError {
    match map_error(error) {
        ProviderAdmissionError::Closed | ProviderAdmissionError::ConflictingDeletion => {
            AccountDeletionRepositoryError::Conflict
        }
        ProviderAdmissionError::InvalidScope => AccountDeletionRepositoryError::InvalidAuthority,
        _ => AccountDeletionRepositoryError::Internal,
    }
}
