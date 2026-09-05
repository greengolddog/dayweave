use std::{str::FromStr, time::Duration};

use sqlx::{
    ConnectOptions, PgPool, Postgres, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use thiserror::Error;
use uuid::Uuid;

use crate::config::DatabaseConfig;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseScope {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
}

/// Serializes every canonical-item mutation and schedule publication for one
/// workspace, including the empty-table case that row locks cannot protect.
/// The domain tag prevents collisions with unrelated advisory-lock users.
pub(crate) async fn lock_canonical_item_space(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text, 0))",
    )
    .bind(workspace_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Serializes an execution-sensitive canonical item transaction with execution
/// Start. Callers take the workspace execution mutex before the canonical item
/// advisory lock so every path uses the same deadlock-free order.
pub(crate) async fn lock_execution_and_canonical_item_space(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO execution_state (workspace_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(workspace_id)
        .execute(&mut **transaction)
        .await?;
    let _: Uuid = sqlx::query_scalar(
        "SELECT workspace_id FROM execution_state WHERE workspace_id = $1 FOR UPDATE",
    )
    .bind(workspace_id)
    .fetch_one(&mut **transaction)
    .await?;
    lock_canonical_item_space(transaction, workspace_id).await
}

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
    scope: DatabaseScope,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Database")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl Database {
    /// Connects, applies embedded migrations, and ensures the configured
    /// personal user/workspace scope exists.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when configuration, connectivity, migrations,
    /// or scope bootstrapping fails.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let options = PgConnectOptions::from_str(config.url.expose())
            .map_err(|_| PersistenceError::InvalidConnectionConfiguration)?
            .disable_statement_logging();
        let pool = PgPoolOptions::new()
            .min_connections(config.min_connections)
            .max_connections(config.max_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(Some(Duration::from_mins(10)))
            .max_lifetime(Some(Duration::from_mins(30)))
            .connect_with(options)
            .await
            .map_err(|_| PersistenceError::ConnectionFailed)?;

        if MIGRATOR.run(&pool).await.is_err() {
            pool.close().await;
            return Err(PersistenceError::MigrationFailed);
        }
        let scope = DatabaseScope {
            workspace_id: config.workspace_id,
            user_id: config.user_id,
        };
        if let Err(error) = bootstrap_personal_scope(&pool, config).await {
            let persistence_error = match error {
                ScopeBootstrapError::Fenced => PersistenceError::AccountDeletionFenced,
                ScopeBootstrapError::Database => PersistenceError::ScopeBootstrapFailed,
            };
            pool.close().await;
            return Err(persistence_error);
        }
        Ok(Self { pool, scope })
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub const fn scope(&self) -> DatabaseScope {
        self.scope
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PersistenceError {
    #[error("database connection configuration is invalid")]
    InvalidConnectionConfiguration,
    #[error("database connection failed")]
    ConnectionFailed,
    #[error("database migration failed")]
    MigrationFailed,
    #[error("database personal scope initialization failed")]
    ScopeBootstrapFailed,
    #[error("database personal scope is permanently fenced for account deletion")]
    AccountDeletionFenced,
    #[error("external integration initialization failed")]
    IntegrationInitializationFailed,
    #[error("Google provider identity root does not match its durable binding")]
    GoogleIdentityRootMismatch,
    #[error("authentication runtime initialization failed")]
    AuthenticationInitializationFailed,
}

async fn bootstrap_personal_scope(
    pool: &PgPool,
    config: &DatabaseConfig,
) -> Result<(), ScopeBootstrapError> {
    let mut transaction = pool.begin().await?;
    let subject_hash: Vec<u8> = sqlx::query_scalar("SELECT sha256(convert_to($1, 'UTF8'))")
        .bind(&config.owner_subject)
        .fetch_one(&mut *transaction)
        .await?;
    // Bootstrap participates in the same global barrier as every mutation and
    // fence. Taking it first prevents a scope-specific lock from deadlocking a
    // pending fence that is waiting for bootstrap to finish.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.global-mutation-barrier.v1', 0))",
    )
    .execute(&mut *transaction)
    .await?;
    // Use separate statements so the subject -> user -> workspace order is
    // guaranteed rather than depending on SELECT-expression evaluation order.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.subject.v1:' || encode($1::bytea, 'hex'), 0))",
    )
    .bind(subject_hash.as_slice())
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.user.v1:' || $1::text, 0))",
    )
    .bind(config.user_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.account-deletion.workspace.v1:' || $1::text, 0))",
    )
    .bind(config.workspace_id)
    .execute(&mut *transaction)
    .await?;
    let fenced = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM account_deletion_fences \
         WHERE workspace_id = $1 OR user_id = $2 OR owner_subject_hash = $3)",
    )
    .bind(config.workspace_id)
    .bind(config.user_id)
    .bind(subject_hash.as_slice())
    .fetch_one(&mut *transaction)
    .await?;
    if fenced {
        return Err(ScopeBootstrapError::Fenced);
    }
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'Personal', $3) ON CONFLICT (id) DO NOTHING",
    )
    .bind(config.user_id)
    .bind(&config.owner_subject)
    .bind(&config.timezone_name)
    .execute(&mut *transaction)
    .await?;
    let stored_subject: String = sqlx::query_scalar(
        "SELECT auth_subject FROM users WHERE id = $1 AND trashed_at IS NULL FOR UPDATE",
    )
    .bind(config.user_id)
    .fetch_one(&mut *transaction)
    .await?;
    if stored_subject != config.owner_subject {
        return Err(sqlx::Error::Protocol(
            "configured user id belongs to a different subject".to_owned(),
        )
        .into());
    }

    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, 'personal', 'Personal', $3) ON CONFLICT (id) DO NOTHING",
    )
    .bind(config.workspace_id)
    .bind(config.user_id)
    .bind(&config.timezone_name)
    .execute(&mut *transaction)
    .await?;
    let stored_owner: Uuid = sqlx::query_scalar(
        "SELECT owner_user_id FROM workspaces \
         WHERE id = $1 AND trashed_at IS NULL FOR UPDATE",
    )
    .bind(config.workspace_id)
    .fetch_one(&mut *transaction)
    .await?;
    if stored_owner != config.user_id {
        return Err(sqlx::Error::Protocol(
            "configured workspace id belongs to a different owner".to_owned(),
        )
        .into());
    }

    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, 'owner') ON CONFLICT (workspace_id, user_id) DO UPDATE \
         SET role = 'owner', removed_at = NULL, updated_at = clock_timestamp()",
    )
    .bind(config.workspace_id)
    .bind(config.user_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

#[derive(Debug)]
enum ScopeBootstrapError {
    Fenced,
    Database,
}

impl From<sqlx::Error> for ScopeBootstrapError {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .is_some_and(|code| code == "DWDEL")
        {
            Self::Fenced
        } else {
            drop(error);
            Self::Database
        }
    }
}
