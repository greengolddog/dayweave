use std::{str::FromStr, time::Duration};

use sqlx::{
    ConnectOptions, PgPool,
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
        if bootstrap_personal_scope(&pool, config).await.is_err() {
            pool.close().await;
            return Err(PersistenceError::ScopeBootstrapFailed);
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
    #[error("external integration initialization failed")]
    IntegrationInitializationFailed,
}

async fn bootstrap_personal_scope(
    pool: &PgPool,
    config: &DatabaseConfig,
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
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
        ));
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
        ));
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
    transaction.commit().await
}
