use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    config::{
        GOOGLE_CALENDAR_READONLY_SCOPE, GOOGLE_CALENDAR_SCOPE, GOOGLE_TASKS_READONLY_SCOPE,
        GOOGLE_TASKS_SCOPE,
    },
    google_sync::{
        DiscoveredCollection, GoogleCollectionKind, GoogleOutboundAccepted, GoogleSyncCollection,
        GoogleSyncRepository, GoogleSyncRepositoryError, GoogleSyncRole, GoogleSyncRunState,
        GoogleSyncRunStatus, ImportOutcome, OutboundOperation, OutboundResult, OutboundWork,
        OutboxCounts, PreparedOutbound, RemoteItemChange, StoredCursor, SyncClaim, SyncCounts,
        SyncFailureKind,
    },
    items::{Item, ItemStatus, ItemTombstone, ReplaceItem, SplitPolicy},
};

use super::DatabaseScope;

const COLLECTION_COLUMNS: &str = "id, provider_account_id, collection_kind, remote_collection_id, display_name, \
    provider_access_role, provider_primary, provider_selected, provider_hidden, provider_deleted, \
    selected, visible, sync_role, revision, discovered_at, configured_at, last_import_at, \
    created_at, updated_at";

#[derive(Clone, Debug)]
pub(crate) struct PostgresGoogleSyncRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresGoogleSyncRepository {
    #[must_use]
    pub(crate) fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }

    async fn ensure_account(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        account_id: Uuid,
        require_active: bool,
    ) -> Result<Vec<String>, GoogleSyncRepositoryError> {
        let row = sqlx::query(
            "SELECT status, sync_enabled, granted_scopes FROM provider_accounts \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND provider = 'google' \
             AND tombstoned_at IS NULL FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleSyncRepositoryError::AccountNotFound)?;
        let status: String = row.try_get("status").map_err(internal)?;
        let enabled: bool = row.try_get("sync_enabled").map_err(internal)?;
        if require_active && (status != "active" || !enabled) {
            return Err(GoogleSyncRepositoryError::AccountNotFound);
        }
        row.try_get("granted_scopes").map_err(internal)
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)] // SQL transaction bodies keep each durable fence atomic.
impl GoogleSyncRepository for PostgresGoogleSyncRepository {
    async fn replace_discovered(
        &self,
        account_id: Uuid,
        claim: Option<&SyncClaim>,
        kind: GoogleCollectionKind,
        collections: Vec<DiscoveredCollection>,
        now: DateTime<Utc>,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let granted_scopes = self
            .ensure_account(&mut transaction, account_id, true)
            .await?;
        if !has_collection_read_scope(&granted_scopes, kind) {
            return Err(GoogleSyncRepositoryError::ReadScopeMissing);
        }
        if let Some(claim) = claim {
            if claim.account_id != account_id {
                return Err(GoogleSyncRepositoryError::ClaimLost);
            }
            ensure_run_claim(&mut transaction, self.scope, claim, now).await?;
        }
        let mut seen = Vec::with_capacity(collections.len());
        for collection in collections {
            if collection.kind != kind {
                return Err(GoogleSyncRepositoryError::Internal);
            }
            if kind == GoogleCollectionKind::Calendar
                && !matches!(
                    collection.provider_access_role.as_deref(),
                    Some("owner" | "writer")
                )
            {
                let downgraded: Option<Uuid> = sqlx::query_scalar(
                    "UPDATE google_sync_collections SET sync_role = 'read_only', \
                     revision = revision + 1, updated_at = $5 WHERE workspace_id = $1 \
                     AND user_id = $2 AND provider_account_id = $3 \
                     AND collection_kind = 'calendar' AND remote_collection_id = $4 \
                     AND sync_role = 'writable' RETURNING id",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(account_id)
                .bind(&collection.remote_id)
                .bind(now)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(internal)?;
                if let Some(collection_id) = downgraded {
                    sqlx::query(
                        "DELETE FROM provider_sync_cursors WHERE workspace_id = $1 \
                         AND provider_account_id = $2 AND collection_key = $3",
                    )
                    .bind(self.scope.workspace_id)
                    .bind(account_id)
                    .bind(format!("calendar:{collection_id}"))
                    .execute(&mut *transaction)
                    .await
                    .map_err(internal)?;
                    sqlx::query(
                        "UPDATE google_sync_outbox SET state = 'conflict', \
                         claim_id = NULL, claimed_at = NULL, \
                         last_error_code = 'collection_access_revoked', updated_at = $4 \
                         WHERE workspace_id = $1 AND user_id = $2 AND collection_id = $3 \
                           AND state IN ('pending', 'delivering', 'backoff')",
                    )
                    .bind(self.scope.workspace_id)
                    .bind(self.scope.user_id)
                    .bind(collection_id)
                    .bind(now)
                    .execute(&mut *transaction)
                    .await
                    .map_err(internal)?;
                }
            }
            seen.push(collection.remote_id.clone());
            sqlx::query(
                "INSERT INTO google_sync_collections (id, workspace_id, user_id, \
                 provider_account_id, collection_kind, remote_collection_id, display_name, \
                 provider_access_role, provider_primary, provider_selected, provider_hidden, \
                 provider_deleted, selected, discovered_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, false, $13, $13, $13) \
                 ON CONFLICT (workspace_id, provider_account_id, collection_kind, remote_collection_id) \
                 DO UPDATE SET display_name = EXCLUDED.display_name, \
                   provider_access_role = EXCLUDED.provider_access_role, \
                   provider_primary = EXCLUDED.provider_primary, \
                   provider_selected = EXCLUDED.provider_selected, \
                   provider_hidden = EXCLUDED.provider_hidden, \
                   provider_deleted = EXCLUDED.provider_deleted, \
                   selected = CASE WHEN EXCLUDED.provider_deleted THEN false \
                                   ELSE google_sync_collections.selected END, \
                   sync_role = CASE WHEN google_sync_collections.collection_kind = 'calendar' \
                                      AND google_sync_collections.sync_role = 'writable' \
                                      AND COALESCE(EXCLUDED.provider_access_role, '') \
                                          NOT IN ('owner', 'writer') \
                                    THEN 'read_only' ELSE google_sync_collections.sync_role END, \
                   discovered_at = EXCLUDED.discovered_at, updated_at = EXCLUDED.updated_at, \
                   revision = CASE WHEN (google_sync_collections.display_name, \
                       google_sync_collections.provider_access_role, \
                       google_sync_collections.provider_primary, \
                       google_sync_collections.provider_selected, \
                        google_sync_collections.provider_hidden, \
                        google_sync_collections.provider_deleted) IS DISTINCT FROM \
                       (EXCLUDED.display_name, EXCLUDED.provider_access_role, \
                        EXCLUDED.provider_primary, EXCLUDED.provider_selected, \
                        EXCLUDED.provider_hidden, EXCLUDED.provider_deleted) \
                       OR (google_sync_collections.collection_kind = 'calendar' \
                           AND google_sync_collections.sync_role = 'writable' \
                           AND COALESCE(EXCLUDED.provider_access_role, '') \
                               NOT IN ('owner', 'writer')) \
                       THEN google_sync_collections.revision + 1 \
                       ELSE google_sync_collections.revision END",
            )
            .bind(Uuid::new_v4())
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(account_id)
            .bind(kind.as_db())
            .bind(collection.remote_id)
            .bind(collection.display_name)
            .bind(collection.provider_access_role)
            .bind(collection.provider_primary)
            .bind(collection.provider_selected)
            .bind(collection.provider_hidden)
            .bind(collection.provider_deleted)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        sqlx::query(
            "UPDATE google_sync_collections SET provider_deleted = true, selected = false, \
             revision = revision + 1, updated_at = $6 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             AND collection_kind = $4 AND NOT provider_deleted \
             AND NOT (remote_collection_id = ANY($5))",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(kind.as_db())
        .bind(&seen)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET state = 'conflict', claim_id = NULL, \
             claimed_at = NULL, last_error_code = 'collection_deleted', updated_at = $5 \
             FROM google_sync_collections collection WHERE outbox.workspace_id = $1 \
             AND outbox.user_id = $2 AND outbox.provider_account_id = $3 \
             AND outbox.collection_id = collection.id \
             AND collection.workspace_id = outbox.workspace_id \
             AND collection.collection_kind = $4 AND collection.provider_deleted \
             AND outbox.state IN ('pending', 'delivering', 'backoff')",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(kind.as_db())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {COLLECTION_COLUMNS} FROM google_sync_collections \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             ORDER BY collection_kind, lower(display_name), id"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(internal)?;
        let result = rows.iter().map(collection_from_row).collect();
        transaction.commit().await.map_err(internal)?;
        result
    }

    async fn collections(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncRepositoryError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {COLLECTION_COLUMNS} FROM google_sync_collections \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             ORDER BY collection_kind, lower(display_name), id"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.iter().map(collection_from_row).collect()
    }

    async fn collection(
        &self,
        account_id: Uuid,
        collection_id: Uuid,
    ) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError> {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {COLLECTION_COLUMNS} FROM google_sync_collections \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 AND id = $4"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(GoogleSyncRepositoryError::CollectionNotFound)?;
        collection_from_row(&row)
    }

    async fn configure_collection(
        &self,
        account_id: Uuid,
        collection_id: Uuid,
        expected_revision: u64,
        selected: bool,
        visible: bool,
        role: GoogleSyncRole,
        now: DateTime<Utc>,
    ) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let granted_scopes = self
            .ensure_account(&mut transaction, account_id, true)
            .await?;
        let current = sqlx::query(
            "SELECT collection_kind, provider_access_role, provider_deleted, selected, visible, \
             sync_role, revision \
             FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
             AND provider_account_id = $3 AND id = $4 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleSyncRepositoryError::CollectionNotFound)?;
        let revision = i64_to_u64(current.try_get("revision").map_err(internal)?)?;
        if revision != expected_revision {
            return Err(GoogleSyncRepositoryError::RevisionConflict {
                expected: expected_revision,
                actual: revision,
            });
        }
        if current
            .try_get::<bool, _>("provider_deleted")
            .map_err(internal)?
        {
            return Err(GoogleSyncRepositoryError::CollectionDeleted);
        }
        let collection_kind: String = current.try_get("collection_kind").map_err(internal)?;
        if selected && !has_collection_read_scope_db(&granted_scopes, &collection_kind) {
            return Err(GoogleSyncRepositoryError::ReadScopeMissing);
        }
        let projection_changed = current.try_get::<bool, _>("visible").map_err(internal)?
            != visible
            || current
                .try_get::<String, _>("sync_role")
                .map_err(internal)?
                != role.as_db();
        if collection_kind == "task_list" && role == GoogleSyncRole::Blocking {
            return Err(GoogleSyncRepositoryError::InvalidCollectionRole);
        }
        if collection_kind == "calendar" && role == GoogleSyncRole::Writable {
            if !granted_scopes
                .iter()
                .any(|scope| scope == GOOGLE_CALENDAR_SCOPE)
            {
                return Err(GoogleSyncRepositoryError::WriteScopeMissing);
            }
            let access: Option<String> =
                current.try_get("provider_access_role").map_err(internal)?;
            if !matches!(access.as_deref(), Some("owner" | "writer")) {
                return Err(GoogleSyncRepositoryError::InvalidCollectionRole);
            }
        }
        if collection_kind == "task_list"
            && role == GoogleSyncRole::Writable
            && !granted_scopes
                .iter()
                .any(|scope| scope == GOOGLE_TASKS_SCOPE)
        {
            return Err(GoogleSyncRepositoryError::WriteScopeMissing);
        }
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE google_sync_collections SET selected = $5, visible = $6, sync_role = $7, \
             configured_at = $8, updated_at = $8, revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 AND id = $4 \
             RETURNING {COLLECTION_COLUMNS}"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(selected)
        .bind(visible)
        .bind(role.as_db())
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if projection_changed {
            let collection_key = if collection_kind == "calendar" {
                format!("calendar:{collection_id}")
            } else {
                format!("tasks:{collection_id}")
            };
            sqlx::query(
                "DELETE FROM provider_sync_cursors WHERE workspace_id = $1 \
                 AND provider_account_id = $2 AND collection_key = $3",
            )
            .bind(self.scope.workspace_id)
            .bind(account_id)
            .bind(collection_key)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        if !selected || role != GoogleSyncRole::Writable {
            sqlx::query(
                "UPDATE google_sync_outbox SET state = 'conflict', \
                 claim_id = NULL, claimed_at = NULL, \
                 last_error_code = 'collection_not_writable', updated_at = $4 \
                 WHERE workspace_id = $1 AND user_id = $2 AND collection_id = $3 \
                   AND state IN ('pending', 'delivering', 'backoff')",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(collection_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        if selected {
            ensure_run_row(&mut transaction, self.scope, account_id, now).await?;
            sqlx::query(
                "UPDATE google_sync_runs SET requested_at = $4, next_attempt_at = \
                 LEAST(next_attempt_at, $4), updated_at = $4, revision = revision + 1 \
                 WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(account_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        let result = collection_from_row(&row);
        transaction.commit().await.map_err(internal)?;
        result
    }

    async fn recover_startup(&self, now: DateTime<Utc>) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_runs SET state = 'backoff', claim_id = NULL, lease_until = NULL, \
             next_attempt_at = $3, last_error_code = 'worker_restarted', last_error_at = $3, \
             consecutive_failures = consecutive_failures + 1, revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND user_id = $2 AND state = 'running'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_outbox SET state = 'backoff', claim_id = NULL, claimed_at = NULL, \
             available_at = $3, last_error_code = 'worker_restarted', attempts = attempts + 1, \
             updated_at = $3 WHERE workspace_id = $1 AND user_id = $2 AND state = 'delivering'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "INSERT INTO google_sync_runs (workspace_id, user_id, provider_account_id, \
             next_attempt_at, created_at, updated_at) \
             SELECT $1, $2, account.id, $3, $3, $3 FROM provider_accounts account \
             WHERE account.workspace_id = $1 AND account.user_id = $2 AND account.provider = 'google' \
             AND account.status = 'active' AND account.sync_enabled AND account.tombstoned_at IS NULL \
             AND EXISTS (SELECT 1 FROM google_sync_collections collection \
               WHERE collection.workspace_id = $1 AND collection.user_id = $2 \
               AND collection.provider_account_id = account.id AND collection.selected \
               AND NOT collection.provider_deleted) \
             ON CONFLICT (workspace_id, provider_account_id) DO NOTHING",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn request_refresh(
        &self,
        account_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        self.ensure_account(&mut transaction, account_id, true)
            .await?;
        ensure_run_row(&mut transaction, self.scope, account_id, now).await?;
        sqlx::query(
            "UPDATE google_sync_runs SET requested_at = $4, \
             next_attempt_at = CASE WHEN state = 'running' THEN next_attempt_at \
                                    ELSE LEAST(next_attempt_at, $4) END, \
             state = CASE WHEN state IN ('failed', 'reauthorization_required') THEN 'idle' ELSE state END, \
             last_error_code = CASE WHEN state IN ('failed', 'reauthorization_required') THEN NULL ELSE last_error_code END, \
             revision = revision + 1, updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_outbox SET available_at = LEAST(available_at, $4), updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND state = 'backoff'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> Result<Option<SyncClaim>, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_runs SET state = 'backoff', claim_id = NULL, lease_until = NULL, \
             next_attempt_at = $3, consecutive_failures = consecutive_failures + 1, \
             last_error_code = 'lease_expired', last_error_at = $3, revision = revision + 1, \
             updated_at = $3 WHERE workspace_id = $1 AND user_id = $2 AND state = 'running' \
             AND lease_until <= $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let claim_id = Uuid::new_v4();
        let row = sqlx::query(
            "WITH candidate AS (SELECT run.provider_account_id FROM google_sync_runs run \
               JOIN provider_accounts account ON account.workspace_id = run.workspace_id \
                 AND account.id = run.provider_account_id \
               WHERE run.workspace_id = $1 AND run.user_id = $2 \
                 AND run.state IN ('idle', 'backoff') AND run.next_attempt_at <= $3 \
                 AND account.user_id = $2 AND account.provider = 'google' \
                 AND account.status = 'active' AND account.sync_enabled \
               ORDER BY run.next_attempt_at, run.provider_account_id \
               FOR UPDATE OF account, run SKIP LOCKED LIMIT 1) \
             UPDATE google_sync_runs run SET state = 'running', claim_id = $4, lease_until = $5, \
               started_at = $3, updated_at = $3, revision = revision + 1 \
             FROM candidate WHERE run.workspace_id = $1 \
               AND run.provider_account_id = candidate.provider_account_id \
             RETURNING run.provider_account_id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .bind(claim_id)
        .bind(lease_until)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let claim = row
            .map(|row| {
                Ok(SyncClaim {
                    account_id: row.try_get("provider_account_id").map_err(internal)?,
                    claim_id,
                })
            })
            .transpose()?;
        transaction.commit().await.map_err(internal)?;
        Ok(claim)
    }

    async fn renew_claim(
        &self,
        claim: &SyncClaim,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let updated = sqlx::query(
            "UPDATE google_sync_runs SET lease_until = $5, updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             AND state = 'running' AND claim_id = $6 AND lease_until > $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .bind(lease_until)
        .bind(claim.claim_id)
        .execute(&self.pool)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            Ok(())
        } else {
            Err(GoogleSyncRepositoryError::ClaimLost)
        }
    }

    async fn complete_claim(
        &self,
        claim: &SyncClaim,
        counts: &SyncCounts,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let updated = sqlx::query(
            "UPDATE google_sync_runs SET state = 'idle', claim_id = NULL, lease_until = NULL, \
             completed_at = $5, next_attempt_at = CASE WHEN requested_at > started_at THEN $5 ELSE $6 END, \
             requested_at = NULL, consecutive_failures = 0, last_error_code = NULL, last_error_at = NULL, \
             imported_count = $7, updated_count = $8, deleted_count = $9, conflict_count = $10, \
             rejected_count = $11, revision = revision + 1, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND state = 'running' AND claim_id = $4 AND lease_until > $5",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(claim.claim_id)
        .bind(now)
        .bind(next_attempt_at)
        .bind(u64_to_i64(counts.imported)?)
        .bind(u64_to_i64(counts.updated)?)
        .bind(u64_to_i64(counts.deleted)?)
        .bind(u64_to_i64(counts.conflicts)?)
        .bind(u64_to_i64(counts.rejected)?)
        .execute(&self.pool)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            Ok(())
        } else {
            Err(GoogleSyncRepositoryError::ClaimLost)
        }
    }

    async fn fail_claim(
        &self,
        claim: &SyncClaim,
        kind: SyncFailureKind,
        code: &'static str,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let state = match kind {
            SyncFailureKind::Backoff => "backoff",
            SyncFailureKind::ReauthorizationRequired => "reauthorization_required",
            SyncFailureKind::Failed => "failed",
        };
        let updated = sqlx::query(
            "UPDATE google_sync_runs SET state = $5, claim_id = NULL, lease_until = NULL, \
             next_attempt_at = $6, consecutive_failures = consecutive_failures + 1, \
             last_error_code = $7, last_error_at = $8, revision = revision + 1, updated_at = $8 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND state = 'running' AND claim_id = $4 AND lease_until > $8",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(claim.claim_id)
        .bind(state)
        .bind(next_attempt_at)
        .bind(code)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            Ok(())
        } else {
            Err(GoogleSyncRepositoryError::ClaimLost)
        }
    }

    async fn run_status(
        &self,
        account_id: Uuid,
    ) -> Result<Option<GoogleSyncRunStatus>, GoogleSyncRepositoryError> {
        let row = sqlx::query(
            "SELECT provider_account_id, state, requested_at, started_at, completed_at, \
             next_attempt_at, consecutive_failures, last_error_code, last_error_at, imported_count, \
             updated_count, deleted_count, conflict_count, rejected_count, revision \
             FROM google_sync_runs WHERE workspace_id = $1 AND user_id = $2 \
             AND provider_account_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.as_ref().map(run_from_row).transpose()
    }

    async fn cursor(
        &self,
        account_id: Uuid,
        collection_key: &str,
    ) -> Result<Option<StoredCursor>, GoogleSyncRepositoryError> {
        let row = sqlx::query(
            "SELECT encrypted_cursor, cursor_key_version, revision FROM provider_sync_cursors \
             WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_key = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(account_id)
        .bind(collection_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.map(|row| {
            Ok(StoredCursor {
                encrypted: row.try_get("encrypted_cursor").map_err(internal)?,
                key_version: i32_to_u32(row.try_get("cursor_key_version").map_err(internal)?)?,
                revision: i64_to_u64(row.try_get("revision").map_err(internal)?)?,
            })
        })
        .transpose()
    }

    async fn store_cursor(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        collection_key: &str,
        expected_revision: Option<u64>,
        encrypted: Vec<u8>,
        key_version: u32,
        watermark_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            collection_id,
            collection_revision,
            now,
        )
        .await?;
        let affected = if let Some(revision) = expected_revision {
            sqlx::query(
                "UPDATE provider_sync_cursors SET encrypted_cursor = $5, cursor_key_version = $6, \
                 watermark_at = $7, last_success_at = $8, revision = revision + 1, updated_at = $8 \
                 WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_key = $3 \
                   AND revision = $4",
            )
            .bind(self.scope.workspace_id)
            .bind(claim.account_id)
            .bind(collection_key)
            .bind(u64_to_i64(revision)?)
            .bind(&encrypted)
            .bind(u32_to_i32(key_version)?)
            .bind(watermark_at)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?
            .rows_affected()
        } else {
            sqlx::query(
                "INSERT INTO provider_sync_cursors (workspace_id, provider_account_id, collection_key, \
                 encrypted_cursor, cursor_key_version, watermark_at, last_success_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7) ON CONFLICT DO NOTHING",
            )
            .bind(self.scope.workspace_id)
            .bind(claim.account_id)
            .bind(collection_key)
            .bind(&encrypted)
            .bind(u32_to_i32(key_version)?)
            .bind(watermark_at)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?
            .rows_affected()
        };
        if affected != 1 {
            return Err(GoogleSyncRepositoryError::CursorConflict);
        }
        sqlx::query(
            "UPDATE google_sync_collections SET last_import_at = $5, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 AND id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(collection_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn clear_cursor(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        collection_key: &str,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            collection_id,
            collection_revision,
            now,
        )
        .await?;
        sqlx::query(
            "DELETE FROM provider_sync_cursors WHERE workspace_id = $1 \
             AND provider_account_id = $2 AND collection_key = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(claim.account_id)
        .bind(collection_key)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn apply_remote_item(
        &self,
        claim: &SyncClaim,
        change: RemoteItemChange,
        now: DateTime<Utc>,
    ) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
        if claim.account_id != change.account_id {
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            change.collection_id,
            change.collection_revision,
            now,
        )
        .await?;
        let mapping = sqlx::query(
            "SELECT id, local_entity_id, remote_payload_hash, remote_projection_hash, local_revision, \
             sync_state, ownership \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND provider_account_id = $2 \
             AND collection_id = $3 AND entity_kind = 'item' AND remote_resource_id = $4 \
             AND tombstoned_at IS NULL FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(change.account_id)
        .bind(change.collection_id)
        .bind(&change.remote_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let outcome = if change.is_deleted() {
            apply_remote_delete(&mut transaction, self.scope, &change, mapping.as_ref(), now)
                .await?
        } else {
            apply_remote_upsert(&mut transaction, self.scope, change, mapping.as_ref(), now).await?
        };
        transaction.commit().await.map_err(internal)?;
        Ok(outcome)
    }

    async fn mark_rejected(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        remote_id: &str,
        reason: &'static str,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            collection_id,
            collection_revision,
            now,
        )
        .await?;
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, collection_id, \
             entity_kind, remote_resource_id, sync_state, conflict_metadata, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'item', $5, 'conflict', $6, $7, $7) \
             ON CONFLICT (workspace_id, provider_account_id, collection_id, entity_kind, remote_resource_id) \
             WHERE collection_id IS NOT NULL AND tombstoned_at IS NULL \
             DO UPDATE SET sync_state = 'conflict', conflict_metadata = EXCLUDED.conflict_metadata, \
               updated_at = EXCLUDED.updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(self.scope.workspace_id)
        .bind(claim.account_id)
        .bind(collection_id)
        .bind(remote_id)
        .bind(json!({"reason": reason}))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn sweep_full_snapshot(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        seen_remote_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<SyncCounts, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            collection_id,
            collection_revision,
            now,
        )
        .await?;
        let mappings = sqlx::query(
            "SELECT id, local_entity_id, remote_resource_id, remote_etag, remote_updated_at, \
             remote_parent_id, remote_payload_hash, remote_projection_hash, local_revision, \
             sync_state, ownership FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_id = $3 \
               AND entity_kind = 'item' AND tombstoned_at IS NULL \
               AND sync_state <> 'deleted_remote' AND NOT (remote_resource_id = ANY($4)) \
             ORDER BY remote_resource_id FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(claim.account_id)
        .bind(collection_id)
        .bind(seen_remote_ids)
        .fetch_all(&mut *transaction)
        .await
        .map_err(internal)?;
        let mut counts = SyncCounts::default();
        for mapping in &mappings {
            let remote_id: String = mapping.try_get("remote_resource_id").map_err(internal)?;
            let change = RemoteItemChange {
                account_id: claim.account_id,
                collection_id,
                collection_revision,
                dayweave_item_id: None,
                remote_id,
                remote_parent_id: mapping.try_get("remote_parent_id").map_err(internal)?,
                remote_etag: mapping.try_get("remote_etag").map_err(internal)?,
                remote_updated_at: mapping.try_get("remote_updated_at").map_err(internal)?,
                remote_payload_hash: mapping_hash(mapping, "remote_payload_hash")?,
                remote_projection_hash: mapping_hash(mapping, "remote_projection_hash")?,
                item: None,
            };
            counts.add(
                apply_remote_delete(&mut transaction, self.scope, &change, Some(mapping), now)
                    .await?,
            );
        }
        transaction.commit().await.map_err(internal)?;
        Ok(counts)
    }

    async fn enqueue_outbound(
        &self,
        account_id: Uuid,
        prepared: PreparedOutbound,
        collection_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<GoogleOutboundAccepted, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let granted_scopes = self
            .ensure_account(&mut transaction, account_id, true)
            .await?;
        let collection = sqlx::query(
            "SELECT collection_kind, selected, provider_deleted, sync_role \
             FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
             AND provider_account_id = $3 AND id = $4 FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleSyncRepositoryError::CollectionNotFound)?;
        let collection_kind: String = collection.try_get("collection_kind").map_err(internal)?;
        if !collection
            .try_get::<bool, _>("selected")
            .map_err(internal)?
            || collection
                .try_get::<bool, _>("provider_deleted")
                .map_err(internal)?
            || collection
                .try_get::<String, _>("sync_role")
                .map_err(internal)?
                != "writable"
        {
            return Err(GoogleSyncRepositoryError::CollectionNotWritable);
        }
        let required_scope = match (prepared.entity_kind, collection_kind.as_str()) {
            ("calendar_event", "calendar") => GOOGLE_CALENDAR_SCOPE,
            ("task", "task_list") => GOOGLE_TASKS_SCOPE,
            _ => return Err(GoogleSyncRepositoryError::CollectionNotWritable),
        };
        if !granted_scopes.iter().any(|scope| scope == required_scope) {
            return Err(GoogleSyncRepositoryError::WriteScopeMissing);
        }
        let stored_revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM items WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(prepared.item.id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let Some(stored_revision) = stored_revision else {
            return Err(GoogleSyncRepositoryError::ItemNotFound);
        };
        let stored_revision = i64_to_u64(stored_revision)?;
        if stored_revision != prepared.item.revision {
            return Err(GoogleSyncRepositoryError::RevisionConflict {
                expected: prepared.item.revision,
                actual: stored_revision,
            });
        }
        let imported_external: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND entity_kind = 'item' AND local_entity_id = $2 AND ownership = 'external' \
             AND tombstoned_at IS NULL)",
        )
        .bind(self.scope.workspace_id)
        .bind(prepared.item.id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if imported_external {
            return Err(GoogleSyncRepositoryError::ExternalMutationForbidden);
        }
        let mapping = sqlx::query(
            "SELECT remote_resource_id, remote_etag, ownership FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_id = $3 \
               AND entity_kind = 'item' AND local_entity_id = $4 AND tombstoned_at IS NULL FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(prepared.item.id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        if mapping.as_ref().is_some_and(|row| {
            row.try_get::<String, _>("ownership").ok().as_deref() != Some("dayweave")
        }) {
            return Err(GoogleSyncRepositoryError::ExternalMutationForbidden);
        }
        let remote_resource_id: Option<String> = mapping
            .as_ref()
            .map(|row| row.try_get("remote_resource_id").map_err(internal))
            .transpose()?;
        let expected_etag: Option<String> = mapping
            .as_ref()
            .map(|row| row.try_get("remote_etag").map_err(internal))
            .transpose()?;
        if remote_resource_id.is_some() && expected_etag.is_none() {
            return Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable);
        }
        if prepared.operation == OutboundOperation::Delete && remote_resource_id.is_none() {
            return Err(GoogleSyncRepositoryError::ExternalMutationForbidden);
        }
        let superseded = sqlx::query(
            "UPDATE google_sync_outbox SET state = 'superseded', claim_id = NULL, claimed_at = NULL, \
             last_error_code = 'superseded_by_newer_revision', updated_at = $7 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND collection_id = $4 AND item_id = $5 AND item_revision < $6 \
               AND state IN ('pending', 'delivering', 'backoff', 'conflict', 'failed')",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(prepared.item.id)
        .bind(u64_to_i64(prepared.item.revision)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if superseded > 0 {
            sqlx::query(
                "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
                 entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
                 VALUES ($1, $2, $3, 'google.sync.outbound_superseded', 'item', $4, $5, $5, \
                 'succeeded', $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(prepared.item.id)
            .bind(u64_to_i64(prepared.item.revision)?)
            .bind(json!({"collection_id": collection_id, "superseded_count": superseded}))
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        let outbox_id = Uuid::new_v4();
        let inserted = sqlx::query(
            "INSERT INTO google_sync_outbox (id, workspace_id, user_id, provider_account_id, \
             collection_id, item_id, item_revision, entity_kind, operation, remote_resource_id, \
             expected_etag, app_owned, payload, available_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, true, $12, $13, $13, $13) \
             ON CONFLICT (workspace_id, collection_id, item_id, item_revision, operation) DO NOTHING",
        )
        .bind(outbox_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(prepared.item.id)
        .bind(u64_to_i64(prepared.item.revision)?)
        .bind(prepared.entity_kind)
        .bind(prepared.operation.as_db())
        .bind(remote_resource_id)
        .bind(expected_etag)
        .bind(prepared.payload)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        let result = if inserted == 1 {
            GoogleOutboundAccepted {
                outbox_id,
                replayed: false,
            }
        } else {
            let existing: Uuid = sqlx::query_scalar(
                "SELECT id FROM google_sync_outbox WHERE workspace_id = $1 AND collection_id = $2 \
                 AND item_id = $3 AND item_revision = $4 AND operation = $5",
            )
            .bind(self.scope.workspace_id)
            .bind(collection_id)
            .bind(prepared.item.id)
            .bind(u64_to_i64(prepared.item.revision)?)
            .bind(prepared.operation.as_db())
            .fetch_one(&mut *transaction)
            .await
            .map_err(internal)?;
            GoogleOutboundAccepted {
                outbox_id: existing,
                replayed: true,
            }
        };
        ensure_run_row(&mut transaction, self.scope, account_id, now).await?;
        sqlx::query(
            "UPDATE google_sync_runs SET requested_at = $4, next_attempt_at = LEAST(next_attempt_at, $4), \
             revision = revision + 1, updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 \
             AND provider_account_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(result)
    }

    async fn claim_outbound(
        &self,
        claim: &SyncClaim,
        now: DateTime<Utc>,
    ) -> Result<Option<OutboundWork>, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let granted_scopes = self
            .ensure_account(&mut transaction, claim.account_id, true)
            .await?;
        ensure_run_claim(&mut transaction, self.scope, claim, now).await?;
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET state = 'superseded', claim_id = NULL, \
             claimed_at = NULL, last_error_code = 'superseded_by_canonical_revision', \
             updated_at = $4 FROM items item WHERE outbox.workspace_id = $1 \
             AND outbox.user_id = $2 AND outbox.provider_account_id = $3 \
             AND item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
             AND item.revision <> outbox.item_revision \
             AND outbox.state IN ('pending', 'delivering', 'backoff')",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_outbox SET state = 'backoff', claim_id = NULL, claimed_at = NULL, \
             available_at = $4, attempts = attempts + 1, last_error_code = 'delivery_lease_expired', \
             updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             AND state = 'delivering' AND claimed_at <= $4 - interval '10 minutes'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let claim_id = Uuid::new_v4();
        let row = sqlx::query(
            "WITH candidate AS (SELECT outbox.id FROM google_sync_outbox outbox \
               JOIN google_sync_collections collection ON collection.workspace_id = outbox.workspace_id \
                 AND collection.id = outbox.collection_id \
               JOIN items item ON item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
               WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
                 AND outbox.provider_account_id = $3 AND outbox.state IN ('pending', 'backoff') \
                 AND collection.user_id = $2 AND collection.provider_account_id = $3 \
                 AND collection.selected AND NOT collection.provider_deleted \
                 AND collection.sync_role = 'writable' \
                 AND item.revision = outbox.item_revision \
                 AND outbox.available_at <= $4 ORDER BY outbox.available_at, outbox.created_at, outbox.id \
               FOR UPDATE OF outbox, collection SKIP LOCKED LIMIT 1) \
             UPDATE google_sync_outbox outbox SET state = 'delivering', claim_id = $5, claimed_at = $4, \
               updated_at = $4 FROM candidate WHERE outbox.id = candidate.id \
             RETURNING outbox.id, outbox.provider_account_id, outbox.collection_id, outbox.item_id, \
               outbox.item_revision, outbox.entity_kind, outbox.operation, outbox.remote_resource_id, \
               outbox.expected_etag, outbox.payload, outbox.attempts, \
               (SELECT remote_collection_id FROM google_sync_collections collection \
                 WHERE collection.workspace_id = outbox.workspace_id AND collection.id = outbox.collection_id) \
                 AS collection_remote_id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .bind(claim_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let work = row
            .map(|row| outbound_from_row(&row, claim_id))
            .transpose()?;
        if let Some(work) = &work {
            let required_scope = match work.entity_kind.as_str() {
                "calendar_event" => GOOGLE_CALENDAR_SCOPE,
                "task" => GOOGLE_TASKS_SCOPE,
                _ => return Err(GoogleSyncRepositoryError::Internal),
            };
            if !granted_scopes.iter().any(|scope| scope == required_scope) {
                return Err(GoogleSyncRepositoryError::WriteScopeMissing);
            }
        }
        transaction.commit().await.map_err(internal)?;
        Ok(work)
    }

    async fn renew_outbound(
        &self,
        work: &OutboundWork,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let updated = sqlx::query(
            "UPDATE google_sync_outbox outbox SET claimed_at = $5, updated_at = $5 \
             FROM google_sync_collections collection, provider_accounts account, items item \
             WHERE outbox.workspace_id = $1 AND outbox.id = $2 \
               AND outbox.provider_account_id = $3 AND outbox.state = 'delivering' \
               AND outbox.claim_id = $4 AND collection.workspace_id = outbox.workspace_id \
               AND collection.id = outbox.collection_id AND collection.selected \
               AND NOT collection.provider_deleted AND collection.sync_role = 'writable' \
               AND account.workspace_id = outbox.workspace_id AND account.id = outbox.provider_account_id \
               AND account.user_id = outbox.user_id AND account.provider = 'google' \
               AND account.status = 'active' AND account.sync_enabled \
               AND account.tombstoned_at IS NULL AND item.workspace_id = outbox.workspace_id \
               AND item.id = outbox.item_id AND item.revision = outbox.item_revision",
        )
        .bind(self.scope.workspace_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            Ok(())
        } else {
            Err(GoogleSyncRepositoryError::ClaimLost)
        }
    }

    async fn complete_outbound(
        &self,
        work: &OutboundWork,
        result: OutboundResult,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let account = sqlx::query(
            "SELECT status, sync_enabled, granted_scopes, tombstoned_at FROM provider_accounts \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND provider = 'google' \
             FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(work.account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let account_valid = account.as_ref().is_some_and(|row| {
            row.try_get::<String, _>("status").ok().as_deref() == Some("active")
                && row.try_get::<bool, _>("sync_enabled").ok() == Some(true)
                && row
                    .try_get::<Option<DateTime<Utc>>, _>("tombstoned_at")
                    .ok()
                    .flatten()
                    .is_none()
        });
        let granted_scopes = account
            .as_ref()
            .map(|row| {
                row.try_get::<Vec<String>, _>("granted_scopes")
                    .map_err(internal)
            })
            .transpose()?
            .unwrap_or_default();
        let required_scope = match work.entity_kind.as_str() {
            "calendar_event" => GOOGLE_CALENDAR_SCOPE,
            "task" => GOOGLE_TASKS_SCOPE,
            _ => return Err(GoogleSyncRepositoryError::Internal),
        };
        if !account_valid || !granted_scopes.iter().any(|scope| scope == required_scope) {
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                "conflict",
                if account_valid {
                    "write_scope_revoked"
                } else {
                    "account_not_active"
                },
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        let collection_valid: bool = sqlx::query_scalar(
            "SELECT selected AND NOT provider_deleted AND sync_role = 'writable' \
             FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
             AND provider_account_id = $3 AND id = $4 FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(work.account_id)
        .bind(work.collection_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .unwrap_or(false);
        if !collection_valid {
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                "conflict",
                "collection_not_writable",
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        let item_revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM items WHERE workspace_id = $1 AND id = $2 FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .bind(work.item_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        if item_revision != Some(u64_to_i64(work.item_revision)?) {
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                "superseded",
                "superseded_during_delivery",
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        if work
            .remote_resource_id
            .as_deref()
            .is_some_and(|remote_id| remote_id != result.remote_resource_id)
        {
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                "conflict",
                "provider_identity_mismatch",
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        let updated = sqlx::query(
            "UPDATE google_sync_outbox SET state = 'published', claim_id = NULL, claimed_at = NULL, \
             remote_resource_id = $5, expected_etag = $6, attempts = attempts + 1, \
             last_error_code = NULL, updated_at = $7 WHERE workspace_id = $1 AND id = $2 \
             AND provider_account_id = $3 AND state = 'delivering' AND claim_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(&result.remote_resource_id)
        .bind(&result.remote_etag)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated != 1 {
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, collection_id, \
             entity_kind, local_entity_id, remote_resource_id, remote_etag, remote_updated_at, \
             remote_payload_hash, local_revision, sync_state, ownership, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, $8, $9, $10, 'synced', 'dayweave', $11, $11) \
             ON CONFLICT (workspace_id, provider_account_id, collection_id, entity_kind, local_entity_id) \
             WHERE collection_id IS NOT NULL AND local_entity_id IS NOT NULL AND tombstoned_at IS NULL \
             DO UPDATE SET remote_resource_id = EXCLUDED.remote_resource_id, remote_etag = EXCLUDED.remote_etag, \
               remote_updated_at = EXCLUDED.remote_updated_at, remote_payload_hash = EXCLUDED.remote_payload_hash, \
               local_revision = EXCLUDED.local_revision, sync_state = 'synced', ownership = 'dayweave', \
               conflict_metadata = NULL, updated_at = EXCLUDED.updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(self.scope.workspace_id)
        .bind(work.account_id)
        .bind(work.collection_id)
        .bind(work.item_id)
        .bind(&result.remote_resource_id)
        .bind(result.remote_etag)
        .bind(result.remote_updated_at)
        .bind(result.payload_hash.as_slice())
        .bind(u64_to_i64(work.item_revision)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        if work.operation == OutboundOperation::Delete {
            sqlx::query(
                "UPDATE provider_sync_mappings SET sync_state = 'deleted_remote', \
                 conflict_metadata = NULL, updated_at = $5 WHERE workspace_id = $1 \
                 AND provider_account_id = $2 AND collection_id = $3 AND local_entity_id = $4 \
                 AND ownership = 'dayweave' AND tombstoned_at IS NULL",
            )
            .bind(self.scope.workspace_id)
            .bind(work.account_id)
            .bind(work.collection_id)
            .bind(work.item_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        sqlx::query(
            "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
             entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
             VALUES ($1, $2, $3, 'google.sync.outbound_published', 'item', $4, $5, $5, \
             'succeeded', $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(work.item_id)
        .bind(u64_to_i64(work.item_revision)?)
        .bind(json!({"collection_id": work.collection_id, "operation": work.operation.as_db()}))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn fail_outbound(
        &self,
        work: &OutboundWork,
        terminal_state: &'static str,
        code: &'static str,
        available_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        if !matches!(terminal_state, "backoff" | "conflict" | "failed") {
            return Err(GoogleSyncRepositoryError::Internal);
        }
        let updated = sqlx::query(
            "UPDATE google_sync_outbox SET state = $5, claim_id = NULL, claimed_at = NULL, \
             attempts = attempts + 1, available_at = $6, last_error_code = $7, updated_at = $8 \
             WHERE workspace_id = $1 AND id = $2 AND provider_account_id = $3 \
               AND state = 'delivering' AND claim_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(terminal_state)
        .bind(available_at)
        .bind(code)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            Ok(())
        } else {
            Err(GoogleSyncRepositoryError::ClaimLost)
        }
    }

    async fn outbox_counts(
        &self,
        account_id: Uuid,
    ) -> Result<OutboxCounts, GoogleSyncRepositoryError> {
        let row = sqlx::query(
            "SELECT count(*) FILTER (WHERE state IN ('pending', 'delivering', 'backoff')) AS pending, \
             count(*) FILTER (WHERE state = 'conflict') AS conflicted, \
             count(*) FILTER (WHERE state = 'failed') AS failed, \
             (array_agg(last_error_code ORDER BY updated_at DESC, id DESC) \
                FILTER (WHERE last_error_code IS NOT NULL \
                    AND state IN ('delivering', 'backoff', 'conflict', 'failed')))[1] \
                AS last_error_code, \
             max(updated_at) FILTER (WHERE last_error_code IS NOT NULL \
                AND state IN ('delivering', 'backoff', 'conflict', 'failed')) AS last_error_at, \
             min(available_at) FILTER (WHERE state IN ('pending', 'backoff')) AS next_attempt_at, \
             (SELECT count(*) FROM provider_sync_mappings mapping WHERE mapping.workspace_id = $1 \
               AND mapping.provider_account_id = $3 AND mapping.sync_state = 'conflict' \
               AND mapping.tombstoned_at IS NULL) AS import_conflicts \
             FROM google_sync_outbox WHERE workspace_id = $1 AND user_id = $2 \
               AND provider_account_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        Ok(OutboxCounts {
            import_conflicts: i64_to_u64(row.try_get("import_conflicts").map_err(internal)?)?,
            pending: i64_to_u64(row.try_get("pending").map_err(internal)?)?,
            conflicted: i64_to_u64(row.try_get("conflicted").map_err(internal)?)?,
            failed: i64_to_u64(row.try_get("failed").map_err(internal)?)?,
            last_error_code: row.try_get("last_error_code").map_err(internal)?,
            last_error_at: row.try_get("last_error_at").map_err(internal)?,
            next_attempt_at: row.try_get("next_attempt_at").map_err(internal)?,
        })
    }
}

async fn ensure_run_row(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "INSERT INTO google_sync_runs (workspace_id, user_id, provider_account_id, next_attempt_at, \
         created_at, updated_at) VALUES ($1, $2, $3, $4, $4, $4) \
         ON CONFLICT (workspace_id, provider_account_id) DO NOTHING",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

fn has_collection_read_scope(scopes: &[String], kind: GoogleCollectionKind) -> bool {
    let (read_only, read_write) = match kind {
        GoogleCollectionKind::Calendar => (GOOGLE_CALENDAR_READONLY_SCOPE, GOOGLE_CALENDAR_SCOPE),
        GoogleCollectionKind::TaskList => (GOOGLE_TASKS_READONLY_SCOPE, GOOGLE_TASKS_SCOPE),
    };
    scopes
        .iter()
        .any(|scope| scope == read_only || scope == read_write)
}

fn has_collection_read_scope_db(scopes: &[String], kind: &str) -> bool {
    match kind {
        "calendar" => has_collection_read_scope(scopes, GoogleCollectionKind::Calendar),
        "task_list" => has_collection_read_scope(scopes, GoogleCollectionKind::TaskList),
        _ => false,
    }
}

async fn ensure_run_claim(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    claim: &SyncClaim,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    let retained = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM google_sync_runs WHERE workspace_id = $1 AND user_id = $2 \
         AND provider_account_id = $3 AND state = 'running' AND claim_id = $4 \
         AND lease_until > $5 FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(claim.account_id)
    .bind(claim.claim_id)
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    if retained.is_some() {
        Ok(())
    } else {
        Err(GoogleSyncRepositoryError::ClaimLost)
    }
}

async fn ensure_inbound_claim(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    claim: &SyncClaim,
    collection_id: Uuid,
    collection_revision: u64,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    let row = sqlx::query(
        "SELECT account.granted_scopes, collection.collection_kind, collection.revision, \
           collection.selected, collection.provider_deleted \
         FROM provider_accounts account \
         JOIN google_sync_runs run ON run.workspace_id = account.workspace_id \
           AND run.user_id = account.user_id AND run.provider_account_id = account.id \
         JOIN google_sync_collections collection ON collection.workspace_id = account.workspace_id \
           AND collection.user_id = account.user_id \
           AND collection.provider_account_id = account.id \
         WHERE account.workspace_id = $1 AND account.user_id = $2 AND account.id = $3 \
           AND account.provider = 'google' AND account.status = 'active' \
           AND account.sync_enabled AND account.tombstoned_at IS NULL \
           AND run.state = 'running' AND run.claim_id = $4 AND run.lease_until > $6 \
           AND collection.id = $5 FOR SHARE OF account, run, collection",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(claim.account_id)
    .bind(claim.claim_id)
    .bind(collection_id)
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleSyncRepositoryError::ClaimLost)?;
    let granted_scopes: Vec<String> = row.try_get("granted_scopes").map_err(internal)?;
    let collection_kind: String = row.try_get("collection_kind").map_err(internal)?;
    if !has_collection_read_scope_db(&granted_scopes, &collection_kind) {
        return Err(GoogleSyncRepositoryError::ReadScopeMissing);
    }
    let current_revision = i64_to_u64(row.try_get("revision").map_err(internal)?)?;
    if current_revision != collection_revision
        || !row.try_get::<bool, _>("selected").map_err(internal)?
        || row
            .try_get::<bool, _>("provider_deleted")
            .map_err(internal)?
    {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Canonical item, mapping, delta, outbox, and audit are one transaction.
async fn apply_remote_delete(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    change: &RemoteItemChange,
    mapping: Option<&PgRow>,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let Some(mapping) = mapping else {
        if let Some((local_id, local_revision)) =
            recover_dayweave_mapping(transaction, scope, change, false, now).await?
        {
            let mapping_id: Uuid = sqlx::query_scalar(
                "SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
                 AND provider_account_id = $2 AND collection_id = $3 AND entity_kind = 'item' \
                 AND remote_resource_id = $4 AND tombstoned_at IS NULL FOR UPDATE",
            )
            .bind(scope.workspace_id)
            .bind(change.account_id)
            .bind(change.collection_id)
            .bind(&change.remote_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(internal)?;
            update_mapping_remote(
                transaction,
                mapping_id,
                change,
                Some(local_revision),
                "conflict",
                Some(json!({"reason": "provider_deleted_dayweave_owned_item"})),
                now,
            )
            .await?;
            conflict_active_outbox(
                transaction,
                scope,
                change.account_id,
                change.collection_id,
                local_id,
                "provider_deleted_dayweave_owned_item",
                now,
            )
            .await?;
            return Ok(ImportOutcome::Conflict);
        }
        // Incremental feeds and complete snapshots may contain tombstones for
        // records that were never observed locally. A tombstone has no
        // canonical state to project and must not reserve the remote ID,
        // because Google may later restore that same record.
        return Ok(ImportOutcome::Unchanged);
    };
    let mapping_id: Uuid = mapping.try_get("id").map_err(internal)?;
    let ownership: String = mapping.try_get("ownership").map_err(internal)?;
    let mapping_state: String = mapping.try_get("sync_state").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    if local_id.is_none() && change.dayweave_item_id.is_some() {
        if recover_dayweave_mapping(transaction, scope, change, false, now)
            .await?
            .is_some()
        {
            return Ok(ImportOutcome::Unchanged);
        }
        update_mapping_remote(
            transaction,
            mapping_id,
            change,
            None,
            "conflict",
            Some(json!({"reason": "unrecognized_dayweave_marker"})),
            now,
        )
        .await?;
        return Ok(ImportOutcome::Conflict);
    }
    if ownership == "dayweave" {
        if mapping_state != "deleted_remote" {
            update_mapping_remote(
                transaction,
                mapping_id,
                change,
                local_revision,
                "conflict",
                Some(json!({"reason": "provider_deleted_dayweave_owned_item"})),
                now,
            )
            .await?;
            if let Some(local_id) = local_id {
                conflict_active_outbox(
                    transaction,
                    scope,
                    change.account_id,
                    change.collection_id,
                    local_id,
                    "provider_deleted_dayweave_owned_item",
                    now,
                )
                .await?;
            }
            return Ok(ImportOutcome::Conflict);
        }
        update_mapping_remote(
            transaction,
            mapping_id,
            change,
            local_revision,
            "deleted_remote",
            None,
            now,
        )
        .await?;
        return Ok(ImportOutcome::Unchanged);
    }
    let Some(local_id) = local_id else {
        update_mapping_remote(
            transaction,
            mapping_id,
            change,
            None,
            "deleted_remote",
            None,
            now,
        )
        .await?;
        return Ok(ImportOutcome::Unchanged);
    };
    let row = sqlx::query(
        "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(local_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        update_mapping_remote(
            transaction,
            mapping_id,
            change,
            local_revision,
            "conflict",
            Some(json!({"reason": "local_item_missing"})),
            now,
        )
        .await?;
        return Ok(ImportOutcome::Conflict);
    };
    let actual: i64 = row.try_get("revision").map_err(internal)?;
    if local_revision != Some(actual) {
        update_mapping_remote(
            transaction,
            mapping_id,
            change,
            local_revision,
            "conflict",
            Some(json!({"reason": "remote_deleted_local_changed", "local_revision": actual})),
            now,
        )
        .await?;
        return Ok(ImportOutcome::Conflict);
    }
    let already_deleted: Option<DateTime<Utc>> = row.try_get("trashed_at").map_err(internal)?;
    let next_revision = if already_deleted.is_some() {
        actual
    } else {
        let next = actual
            .checked_add(1)
            .ok_or(GoogleSyncRepositoryError::Internal)?;
        sqlx::query(
            "UPDATE items SET revision = $3, updated_at = $4, trashed_at = $4 \
             WHERE workspace_id = $1 AND id = $2 AND revision = $5",
        )
        .bind(scope.workspace_id)
        .bind(local_id)
        .bind(next)
        .bind(now)
        .bind(actual)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        let tombstone = ItemTombstone {
            id: local_id,
            revision: i64_to_u64(next)?,
            deleted_at: now,
            parent_id: None,
        };
        record_import_mutation(
            transaction,
            scope,
            local_id,
            next,
            "tombstone",
            serde_json::to_value(tombstone).map_err(|_| GoogleSyncRepositoryError::Internal)?,
            "item.google_import_deleted",
            Some(actual),
            now,
        )
        .await?;
        next
    };
    update_mapping_remote(
        transaction,
        mapping_id,
        change,
        Some(next_revision),
        "deleted_remote",
        None,
        now,
    )
    .await?;
    Ok(if already_deleted.is_some() {
        ImportOutcome::Unchanged
    } else {
        ImportOutcome::Deleted
    })
}

#[allow(clippy::too_many_lines)] // Canonical item, mapping, delta, outbox, and audit are one transaction.
async fn apply_remote_upsert(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    mut change: RemoteItemChange,
    mapping: Option<&PgRow>,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let input = change
        .item
        .take()
        .ok_or(GoogleSyncRepositoryError::Internal)?;
    let candidate = Item::new(input, now).map_err(|_| GoogleSyncRepositoryError::Internal)?;
    let Some(mapping) = mapping else {
        if change.dayweave_item_id.is_some() {
            if recover_dayweave_mapping(transaction, scope, &change, true, now)
                .await?
                .is_some()
            {
                return Ok(ImportOutcome::Unchanged);
            }
            insert_mapping(
                transaction,
                scope,
                &change,
                None,
                None,
                "conflict",
                "external",
                Some(json!({"reason": "unrecognized_dayweave_marker"})),
                now,
            )
            .await?;
            return Ok(ImportOutcome::Conflict);
        }
        insert_imported_item(transaction, scope, &candidate).await?;
        record_import_mutation(
            transaction,
            scope,
            candidate.id,
            u64_to_i64(candidate.revision)?,
            "upsert",
            serde_json::to_value(&candidate).map_err(|_| GoogleSyncRepositoryError::Internal)?,
            "item.google_import_created",
            None,
            now,
        )
        .await?;
        insert_mapping(
            transaction,
            scope,
            &change,
            Some(candidate.id),
            Some(u64_to_i64(candidate.revision)?),
            "synced",
            "external",
            None,
            now,
        )
        .await?;
        return Ok(ImportOutcome::Created);
    };
    let mapping_id: Uuid = mapping.try_get("id").map_err(internal)?;
    let ownership: String = mapping.try_get("ownership").map_err(internal)?;
    let old_hash: Option<Vec<u8>> = mapping.try_get("remote_payload_hash").map_err(internal)?;
    let old_projection_hash: Option<Vec<u8>> = mapping
        .try_get("remote_projection_hash")
        .map_err(internal)?;
    let mapping_state: String = mapping.try_get("sync_state").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    if local_id.is_none() && change.dayweave_item_id.is_some() {
        if recover_dayweave_mapping(transaction, scope, &change, true, now)
            .await?
            .is_some()
        {
            return Ok(ImportOutcome::Unchanged);
        }
        update_mapping_remote(
            transaction,
            mapping_id,
            &change,
            None,
            "conflict",
            Some(json!({"reason": "unrecognized_dayweave_marker"})),
            now,
        )
        .await?;
        return Ok(ImportOutcome::Conflict);
    }
    if ownership == "dayweave" {
        if change.remote_etag.is_none() {
            sqlx::query(
                "UPDATE provider_sync_mappings SET remote_updated_at = $2, remote_parent_id = $3, \
                 remote_payload_hash = $4, remote_projection_hash = $5, sync_state = 'conflict', \
                 conflict_metadata = $6, updated_at = $7 WHERE id = $1",
            )
            .bind(mapping_id)
            .bind(change.remote_updated_at)
            .bind(&change.remote_parent_id)
            .bind(change.remote_payload_hash.as_slice())
            .bind(change.remote_projection_hash.as_slice())
            .bind(json!({"reason": "provider_etag_missing"}))
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(internal)?;
            if let Some(local_id) = local_id {
                conflict_active_outbox(
                    transaction,
                    scope,
                    change.account_id,
                    change.collection_id,
                    local_id,
                    "provider_etag_missing",
                    now,
                )
                .await?;
            }
            return Ok(ImportOutcome::Conflict);
        }
        if old_hash.as_deref() == Some(change.remote_payload_hash.as_slice()) {
            update_mapping_remote(
                transaction,
                mapping_id,
                &change,
                local_revision,
                "synced",
                None,
                now,
            )
            .await?;
            return Ok(ImportOutcome::Unchanged);
        }
        update_mapping_remote(
            transaction,
            mapping_id,
            &change,
            local_revision,
            "conflict",
            Some(json!({"reason": "provider_changed_dayweave_owned_item"})),
            now,
        )
        .await?;
        if let Some(local_id) = local_id {
            conflict_active_outbox(
                transaction,
                scope,
                change.account_id,
                change.collection_id,
                local_id,
                "provider_changed_dayweave_owned_item",
                now,
            )
            .await?;
        }
        return Ok(ImportOutcome::Conflict);
    }
    let Some(local_id) = local_id else {
        insert_imported_item(transaction, scope, &candidate).await?;
        record_import_mutation(
            transaction,
            scope,
            candidate.id,
            u64_to_i64(candidate.revision)?,
            "upsert",
            serde_json::to_value(&candidate).map_err(|_| GoogleSyncRepositoryError::Internal)?,
            "item.google_import_created",
            None,
            now,
        )
        .await?;
        sqlx::query(
            "UPDATE provider_sync_mappings SET local_entity_id = $2, local_revision = $3, \
             remote_etag = $4, remote_updated_at = $5, remote_payload_hash = $6, \
             remote_projection_hash = $7, remote_parent_id = $8, sync_state = 'synced', \
             conflict_metadata = NULL, updated_at = $9 \
             WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(candidate.id)
        .bind(u64_to_i64(candidate.revision)?)
        .bind(change.remote_etag)
        .bind(change.remote_updated_at)
        .bind(change.remote_payload_hash.as_slice())
        .bind(change.remote_projection_hash.as_slice())
        .bind(change.remote_parent_id)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        return Ok(ImportOutcome::Created);
    };
    let current = fetch_import_item(transaction, scope.workspace_id, local_id).await?;
    let expected = local_revision.ok_or(GoogleSyncRepositoryError::Internal)?;
    if u64_to_i64(current.revision)? != expected {
        update_mapping_remote(
            transaction,
            mapping_id,
            &change,
            local_revision,
            "conflict",
            Some(json!({"reason": "local_item_changed", "local_revision": current.revision})),
            now,
        )
        .await?;
        return Ok(ImportOutcome::Conflict);
    }
    if old_projection_hash.as_deref() == Some(change.remote_projection_hash.as_slice()) {
        update_mapping_remote(
            transaction,
            mapping_id,
            &change,
            local_revision,
            "synced",
            None,
            now,
        )
        .await?;
        return Ok(ImportOutcome::Unchanged);
    }
    if current.deleted_at.is_some() && mapping_state != "deleted_remote" {
        update_mapping_remote(
            transaction,
            mapping_id,
            &change,
            local_revision,
            "conflict",
            Some(json!({"reason": "provider_updated_locally_deleted_item"})),
            now,
        )
        .await?;
        return Ok(ImportOutcome::Conflict);
    }
    let replacement = ReplaceItem {
        kind: candidate.kind,
        status: candidate.status,
        title: candidate.title,
        notes: candidate.notes,
        timezone_name: candidate.timezone_name,
        duration_seconds: candidate.duration_seconds,
        deadline_at: candidate.deadline_at,
        earliest_start_at: candidate.earliest_start_at,
        recurrence: candidate.recurrence,
        flexible_constraints: candidate.flexible_constraints,
        split_policy: candidate.split_policy,
        importance: candidate.importance,
        urgency: candidate.urgency,
        parent_id: current.parent_id,
        sibling_order: current.sibling_order,
    };
    let restored = current.deleted_at.is_some();
    let mut updated = current
        .replaced(replacement, now)
        .map_err(|_| GoogleSyncRepositoryError::Internal)?;
    if restored {
        updated.deleted_at = None;
    }
    update_imported_item(transaction, scope.workspace_id, &updated).await?;
    record_import_mutation(
        transaction,
        scope,
        updated.id,
        u64_to_i64(updated.revision)?,
        "upsert",
        serde_json::to_value(&updated).map_err(|_| GoogleSyncRepositoryError::Internal)?,
        if restored {
            "item.google_import_restored"
        } else {
            "item.google_import_updated"
        },
        Some(expected),
        now,
    )
    .await?;
    update_mapping_remote(
        transaction,
        mapping_id,
        &change,
        Some(u64_to_i64(updated.revision)?),
        "synced",
        None,
        now,
    )
    .await?;
    Ok(ImportOutcome::Updated)
}

#[allow(clippy::too_many_lines)] // Validates and adopts one exact durable ownership identity.
async fn recover_dayweave_mapping(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    change: &RemoteItemChange,
    live_resource: bool,
    now: DateTime<Utc>,
) -> Result<Option<(Uuid, i64)>, GoogleSyncRepositoryError> {
    let Some(item_id) = change.dayweave_item_id else {
        return Ok(None);
    };
    if live_resource && change.remote_etag.is_none() {
        // A live resource without an ETag cannot safely become DayWeave-owned:
        // the next write would be unconditional. Leave it as a provider
        // conflict and keep the durable outbound intent unresolved.
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT outbox.id, outbox.item_revision, outbox.entity_kind, outbox.payload, \
           outbox.remote_resource_id \
         FROM google_sync_outbox outbox JOIN items item \
           ON item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
         WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
           AND outbox.provider_account_id = $3 AND outbox.collection_id = $4 \
           AND outbox.item_id = $5 AND outbox.operation = 'upsert' AND outbox.app_owned \
           AND outbox.item_revision = item.revision AND item.trashed_at IS NULL \
           AND (outbox.state IN ('pending', 'delivering', 'backoff') \
             OR (outbox.state = 'conflict' \
               AND outbox.last_error_code = 'provider_identity_unresolved')) \
         ORDER BY outbox.item_revision DESC, outbox.created_at DESC, outbox.id DESC \
         FOR UPDATE OF outbox, item LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(item_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let outbox_id: Uuid = row.try_get("id").map_err(internal)?;
    let local_revision: i64 = row.try_get("item_revision").map_err(internal)?;
    let entity_kind: String = row.try_get("entity_kind").map_err(internal)?;
    let payload: Value = row.try_get("payload").map_err(internal)?;
    let retained_remote_id: Option<String> = row.try_get("remote_resource_id").map_err(internal)?;
    let identity_matches = match entity_kind.as_str() {
        "calendar_event" => {
            payload.get("id").and_then(Value::as_str) == Some(change.remote_id.as_str())
        }
        "task" => true,
        _ => false,
    } && retained_remote_id
        .as_deref()
        .is_none_or(|remote_id| remote_id == change.remote_id);
    if !identity_matches {
        return Ok(None);
    }
    let local_mapping = sqlx::query(
        "SELECT id, remote_resource_id FROM provider_sync_mappings WHERE workspace_id = $1 \
         AND provider_account_id = $2 AND collection_id = $3 AND entity_kind = 'item' \
         AND local_entity_id = $4 AND tombstoned_at IS NULL FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(item_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let mapping_id: Uuid = if let Some(mapping) = local_mapping {
        if mapping
            .try_get::<String, _>("remote_resource_id")
            .map_err(internal)?
            != change.remote_id
        {
            return Ok(None);
        }
        mapping.try_get("id").map_err(internal)?
    } else {
        let remote_mapping = sqlx::query(
            "SELECT id, local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND provider_account_id = $2 AND collection_id = $3 AND entity_kind = 'item' \
             AND remote_resource_id = $4 AND tombstoned_at IS NULL FOR UPDATE",
        )
        .bind(scope.workspace_id)
        .bind(change.account_id)
        .bind(change.collection_id)
        .bind(&change.remote_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(internal)?;
        if let Some(mapping) = remote_mapping {
            if mapping
                .try_get::<Option<Uuid>, _>("local_entity_id")
                .map_err(internal)?
                .is_some_and(|mapped| mapped != item_id)
            {
                return Ok(None);
            }
            mapping.try_get("id").map_err(internal)?
        } else {
            insert_mapping(
                transaction,
                scope,
                change,
                Some(item_id),
                Some(local_revision),
                "synced",
                "dayweave",
                None,
                now,
            )
            .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
                 AND provider_account_id = $2 AND collection_id = $3 AND entity_kind = 'item' \
                 AND remote_resource_id = $4 AND tombstoned_at IS NULL",
            )
            .bind(scope.workspace_id)
            .bind(change.account_id)
            .bind(change.collection_id)
            .bind(&change.remote_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(internal)?
        }
    };
    sqlx::query(
        "UPDATE provider_sync_mappings SET local_entity_id = $2, local_revision = $3, \
         remote_etag = $4, remote_updated_at = $5, remote_parent_id = $6, \
         remote_payload_hash = $7, remote_projection_hash = $8, sync_state = 'synced', \
         ownership = 'dayweave', conflict_metadata = NULL, updated_at = $9 WHERE id = $1",
    )
    .bind(mapping_id)
    .bind(item_id)
    .bind(local_revision)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(&change.remote_parent_id)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "UPDATE google_sync_outbox SET remote_resource_id = $2, expected_etag = $3, \
         state = CASE WHEN state IN ('backoff', 'conflict') THEN 'pending' ELSE state END, \
         claim_id = CASE WHEN state IN ('backoff', 'conflict') THEN NULL ELSE claim_id END, \
         claimed_at = CASE WHEN state IN ('backoff', 'conflict') THEN NULL ELSE claimed_at END, \
         available_at = CASE WHEN state IN ('backoff', 'conflict') THEN $4 ELSE available_at END, \
         last_error_code = CASE WHEN state IN ('backoff', 'conflict') THEN NULL \
                                ELSE last_error_code END, updated_at = $4 WHERE id = $1",
    )
    .bind(outbox_id)
    .bind(&change.remote_id)
    .bind(&change.remote_etag)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, 'google.sync.outbound_identity_recovered', 'item', $4, $5, $5, \
         'succeeded', $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(item_id)
    .bind(local_revision)
    .bind(json!({"collection_id": change.collection_id, "remote_id": &change.remote_id}))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(Some((item_id, local_revision)))
}

#[allow(clippy::too_many_arguments)]
async fn conflict_active_outbox(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    collection_id: Uuid,
    item_id: Uuid,
    code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "UPDATE google_sync_outbox SET state = 'conflict', claim_id = NULL, claimed_at = NULL, \
         available_at = $7, last_error_code = $6, updated_at = $7 WHERE workspace_id = $1 \
         AND user_id = $2 AND provider_account_id = $3 AND collection_id = $4 AND item_id = $5 \
         AND state IN ('pending', 'delivering', 'backoff', 'conflict')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(collection_id)
    .bind(item_id)
    .bind(code)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn revoke_outbound_after_provider(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    work: &OutboundWork,
    state: &'static str,
    code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    if !matches!(state, "conflict" | "superseded") {
        return Err(GoogleSyncRepositoryError::Internal);
    }
    sqlx::query(
        "UPDATE google_sync_outbox SET state = $5, claim_id = NULL, claimed_at = NULL, \
         attempts = attempts + 1, available_at = $6, last_error_code = $7, updated_at = $6 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND provider_account_id = $4 \
           AND state = 'delivering' AND claim_id = $8",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(work.id)
    .bind(work.account_id)
    .bind(state)
    .bind(now)
    .bind(code)
    .bind(work.claim_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // One atomic mapping row mirrors the durable provider fence.
async fn insert_mapping(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    change: &RemoteItemChange,
    local_id: Option<Uuid>,
    local_revision: Option<i64>,
    state: &'static str,
    ownership: &'static str,
    conflict: Option<Value>,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, collection_id, \
         entity_kind, local_entity_id, remote_resource_id, remote_etag, remote_updated_at, \
         remote_parent_id, remote_payload_hash, remote_projection_hash, local_revision, sync_state, \
         ownership, conflict_metadata, created_at, updated_at) VALUES ($1, $2, $3, $4, 'item', \
         $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(local_id)
    .bind(&change.remote_id)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(&change.remote_parent_id)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(local_revision)
    .bind(state)
    .bind(ownership)
    .bind(conflict)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn update_mapping_remote(
    transaction: &mut Transaction<'_, Postgres>,
    mapping_id: Uuid,
    change: &RemoteItemChange,
    local_revision: Option<i64>,
    state: &'static str,
    conflict: Option<Value>,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "UPDATE provider_sync_mappings SET remote_etag = $2, remote_updated_at = $3, \
         remote_parent_id = $4, remote_payload_hash = $5, remote_projection_hash = $6, \
         local_revision = $7, sync_state = $8, conflict_metadata = $9, updated_at = $10 WHERE id = $1",
    )
    .bind(mapping_id)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(&change.remote_parent_id)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(local_revision)
    .bind(state)
    .bind(conflict)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn insert_imported_item(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item: &Item,
) -> Result<(), GoogleSyncRepositoryError> {
    let (split, minimum, maximum) = split_columns(&item.split_policy);
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, notes, \
         timezone_name, duration_seconds, deadline_at, earliest_start_at, recurrence, \
         scheduling_constraints, split_allowed, minimum_chunk_seconds, maximum_chunk_seconds, \
         importance, urgency, sibling_order, revision, created_at, updated_at, completed_at, trashed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
         $16, $17, $18, $19, $20, $21, $22, $23, $24)",
    )
    .bind(item.id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(kind_name(item))
    .bind(status_name(item.status))
    .bind(&item.title)
    .bind(&item.notes)
    .bind(&item.timezone_name)
    .bind(item.duration_seconds.map(u32_to_i32).transpose()?)
    .bind(item.deadline_at)
    .bind(item.earliest_start_at)
    .bind(&item.recurrence)
    .bind(&item.flexible_constraints)
    .bind(split)
    .bind(minimum)
    .bind(maximum)
    .bind(i16::from(item.importance))
    .bind(i16::from(item.urgency))
    .bind(u32_to_i32(item.sibling_order)?)
    .bind(u64_to_i64(item.revision)?)
    .bind(item.created_at)
    .bind(item.updated_at)
    .bind(item.completed_at)
    .bind(item.deleted_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn update_imported_item(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item: &Item,
) -> Result<(), GoogleSyncRepositoryError> {
    let (split, minimum, maximum) = split_columns(&item.split_policy);
    sqlx::query(
        "UPDATE items SET kind = $3, status = $4, title = $5, notes = $6, timezone_name = $7, \
         duration_seconds = $8, deadline_at = $9, earliest_start_at = $10, recurrence = $11, \
         scheduling_constraints = $12, split_allowed = $13, minimum_chunk_seconds = $14, \
         maximum_chunk_seconds = $15, importance = $16, urgency = $17, revision = $18, \
         updated_at = $19, completed_at = $20, trashed_at = $21 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(item.id)
    .bind(kind_name(item))
    .bind(status_name(item.status))
    .bind(&item.title)
    .bind(&item.notes)
    .bind(&item.timezone_name)
    .bind(item.duration_seconds.map(u32_to_i32).transpose()?)
    .bind(item.deadline_at)
    .bind(item.earliest_start_at)
    .bind(&item.recurrence)
    .bind(&item.flexible_constraints)
    .bind(split)
    .bind(minimum)
    .bind(maximum)
    .bind(i16::from(item.importance))
    .bind(i16::from(item.urgency))
    .bind(u64_to_i64(item.revision)?)
    .bind(item.updated_at)
    .bind(item.completed_at)
    .bind(item.deleted_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Records one complete canonical mutation envelope.
async fn record_import_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_id: Uuid,
    revision: i64,
    change_kind: &'static str,
    payload: Value,
    event: &'static str,
    base_revision: Option<i64>,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "INSERT INTO item_changes (workspace_id, item_id, item_revision, change_kind, payload, changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(revision)
    .bind(change_kind)
    .bind(payload)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, \
         aggregate_revision, event_type, deduplication_key, payload) \
         VALUES ($1, $2, 'item', $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(revision)
    .bind(event)
    .bind(format!("{event}:{item_id}:{revision}"))
    .bind(json!({"item_id": item_id, "revision": revision, "change": change_kind}))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, entity_type, \
         entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, 'succeeded', \
         '{\"source\":\"google_sync\"}'::jsonb, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(event)
    .bind(item_id)
    .bind(base_revision)
    .bind(revision)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn fetch_import_item(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<Item, GoogleSyncRepositoryError> {
    let row = sqlx::query(
        "SELECT item.id, item.kind, item.status, item.title, item.notes, item.timezone_name, \
         item.duration_seconds, item.deadline_at, item.earliest_start_at, item.recurrence, \
         item.scheduling_constraints, item.split_allowed, item.minimum_chunk_seconds, \
         item.maximum_chunk_seconds, item.importance, item.urgency, item.revision, item.created_at, \
         item.updated_at, item.completed_at, item.trashed_at, hierarchy.parent_item_id, \
         COALESCE(hierarchy.position, item.sibling_order) AS sibling_order \
         FROM items item LEFT JOIN item_hierarchy hierarchy ON hierarchy.workspace_id = item.workspace_id \
           AND hierarchy.child_item_id = item.id \
         WHERE item.workspace_id = $1 AND item.id = $2 FOR UPDATE OF item",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleSyncRepositoryError::ItemNotFound)?;
    item_from_row(&row)
}

fn item_from_row(row: &PgRow) -> Result<Item, GoogleSyncRepositoryError> {
    let split = row.try_get::<bool, _>("split_allowed").map_err(internal)?;
    let split_policy = if split {
        SplitPolicy::Splittable {
            minimum_chunk_seconds: i32_to_u32(
                row.try_get("minimum_chunk_seconds").map_err(internal)?,
            )?,
            maximum_chunk_seconds: i32_to_u32(
                row.try_get("maximum_chunk_seconds").map_err(internal)?,
            )?,
        }
    } else {
        SplitPolicy::Indivisible
    };
    let deleted_at: Option<DateTime<Utc>> = row.try_get("trashed_at").map_err(internal)?;
    Ok(Item {
        id: row.try_get("id").map_err(internal)?,
        kind: match row.try_get::<String, _>("kind").map_err(internal)?.as_str() {
            "event" => crate::items::ItemKind::Event,
            "task" => crate::items::ItemKind::Task,
            _ => return Err(GoogleSyncRepositoryError::Internal),
        },
        status: parse_status(&row.try_get::<String, _>("status").map_err(internal)?)?,
        title: row.try_get("title").map_err(internal)?,
        notes: row.try_get("notes").map_err(internal)?,
        timezone_name: row.try_get("timezone_name").map_err(internal)?,
        duration_seconds: row
            .try_get::<Option<i32>, _>("duration_seconds")
            .map_err(internal)?
            .map(i32_to_u32)
            .transpose()?,
        deadline_at: row.try_get("deadline_at").map_err(internal)?,
        earliest_start_at: row.try_get("earliest_start_at").map_err(internal)?,
        recurrence: row.try_get("recurrence").map_err(internal)?,
        flexible_constraints: row.try_get("scheduling_constraints").map_err(internal)?,
        split_policy,
        importance: i16_to_u8(row.try_get("importance").map_err(internal)?)?,
        urgency: i16_to_u8(row.try_get("urgency").map_err(internal)?)?,
        parent_id: row.try_get("parent_item_id").map_err(internal)?,
        sibling_order: i32_to_u32(row.try_get("sibling_order").map_err(internal)?)?,
        is_executable: deleted_at.is_none(),
        revision: i64_to_u64(row.try_get("revision").map_err(internal)?)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
        completed_at: row.try_get("completed_at").map_err(internal)?,
        deleted_at,
    })
}

fn collection_from_row(row: &PgRow) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError> {
    Ok(GoogleSyncCollection {
        id: row.try_get("id").map_err(internal)?,
        account_id: row.try_get("provider_account_id").map_err(internal)?,
        kind: parse_collection_kind(
            &row.try_get::<String, _>("collection_kind")
                .map_err(internal)?,
        )?,
        remote_collection_id: row.try_get("remote_collection_id").map_err(internal)?,
        display_name: row.try_get("display_name").map_err(internal)?,
        provider_access_role: row.try_get("provider_access_role").map_err(internal)?,
        provider_primary: row.try_get("provider_primary").map_err(internal)?,
        provider_selected: row.try_get("provider_selected").map_err(internal)?,
        provider_hidden: row.try_get("provider_hidden").map_err(internal)?,
        provider_deleted: row.try_get("provider_deleted").map_err(internal)?,
        selected: row.try_get("selected").map_err(internal)?,
        visible: row.try_get("visible").map_err(internal)?,
        sync_role: parse_role(&row.try_get::<String, _>("sync_role").map_err(internal)?)?,
        revision: i64_to_u64(row.try_get("revision").map_err(internal)?)?,
        discovered_at: row.try_get("discovered_at").map_err(internal)?,
        configured_at: row.try_get("configured_at").map_err(internal)?,
        last_import_at: row.try_get("last_import_at").map_err(internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
    })
}

fn run_from_row(row: &PgRow) -> Result<GoogleSyncRunStatus, GoogleSyncRepositoryError> {
    Ok(GoogleSyncRunStatus {
        account_id: row.try_get("provider_account_id").map_err(internal)?,
        state: match row
            .try_get::<String, _>("state")
            .map_err(internal)?
            .as_str()
        {
            "idle" => GoogleSyncRunState::Idle,
            "running" => GoogleSyncRunState::Running,
            "backoff" => GoogleSyncRunState::Backoff,
            "reauthorization_required" => GoogleSyncRunState::ReauthorizationRequired,
            "failed" => GoogleSyncRunState::Failed,
            _ => return Err(GoogleSyncRepositoryError::Internal),
        },
        requested_at: row.try_get("requested_at").map_err(internal)?,
        started_at: row.try_get("started_at").map_err(internal)?,
        completed_at: row.try_get("completed_at").map_err(internal)?,
        next_attempt_at: row.try_get("next_attempt_at").map_err(internal)?,
        consecutive_failures: i32_to_u32(row.try_get("consecutive_failures").map_err(internal)?)?,
        last_error_code: row.try_get("last_error_code").map_err(internal)?,
        last_error_at: row.try_get("last_error_at").map_err(internal)?,
        imported_count: i64_to_u64(row.try_get("imported_count").map_err(internal)?)?,
        updated_count: i64_to_u64(row.try_get("updated_count").map_err(internal)?)?,
        deleted_count: i64_to_u64(row.try_get("deleted_count").map_err(internal)?)?,
        conflict_count: i64_to_u64(row.try_get("conflict_count").map_err(internal)?)?,
        rejected_count: i64_to_u64(row.try_get("rejected_count").map_err(internal)?)?,
        revision: i64_to_u64(row.try_get("revision").map_err(internal)?)?,
    })
}

fn outbound_from_row(
    row: &PgRow,
    claim_id: Uuid,
) -> Result<OutboundWork, GoogleSyncRepositoryError> {
    Ok(OutboundWork {
        id: row.try_get("id").map_err(internal)?,
        account_id: row.try_get("provider_account_id").map_err(internal)?,
        collection_id: row.try_get("collection_id").map_err(internal)?,
        collection_remote_id: row.try_get("collection_remote_id").map_err(internal)?,
        item_id: row.try_get("item_id").map_err(internal)?,
        item_revision: i64_to_u64(row.try_get("item_revision").map_err(internal)?)?,
        entity_kind: row.try_get("entity_kind").map_err(internal)?,
        operation: match row
            .try_get::<String, _>("operation")
            .map_err(internal)?
            .as_str()
        {
            "upsert" => OutboundOperation::Upsert,
            "delete" => OutboundOperation::Delete,
            _ => return Err(GoogleSyncRepositoryError::Internal),
        },
        remote_resource_id: row.try_get("remote_resource_id").map_err(internal)?,
        expected_etag: row.try_get("expected_etag").map_err(internal)?,
        payload: row.try_get("payload").map_err(internal)?,
        claim_id,
        attempts: i32_to_u32(row.try_get("attempts").map_err(internal)?)?,
    })
}

fn parse_collection_kind(value: &str) -> Result<GoogleCollectionKind, GoogleSyncRepositoryError> {
    match value {
        "calendar" => Ok(GoogleCollectionKind::Calendar),
        "task_list" => Ok(GoogleCollectionKind::TaskList),
        _ => Err(GoogleSyncRepositoryError::Internal),
    }
}

fn parse_role(value: &str) -> Result<GoogleSyncRole, GoogleSyncRepositoryError> {
    match value {
        "read_only" => Ok(GoogleSyncRole::ReadOnly),
        "blocking" => Ok(GoogleSyncRole::Blocking),
        "writable" => Ok(GoogleSyncRole::Writable),
        _ => Err(GoogleSyncRepositoryError::Internal),
    }
}

fn parse_status(value: &str) -> Result<ItemStatus, GoogleSyncRepositoryError> {
    match value {
        "inbox" => Ok(ItemStatus::Inbox),
        "planned" => Ok(ItemStatus::Planned),
        "scheduled" => Ok(ItemStatus::Scheduled),
        "in_progress" => Ok(ItemStatus::InProgress),
        "paused" => Ok(ItemStatus::Paused),
        "completed" => Ok(ItemStatus::Completed),
        "skipped" => Ok(ItemStatus::Skipped),
        "cancelled" => Ok(ItemStatus::Cancelled),
        _ => Err(GoogleSyncRepositoryError::Internal),
    }
}

fn kind_name(item: &Item) -> &'static str {
    match item.kind {
        crate::items::ItemKind::Event => "event",
        crate::items::ItemKind::Task => "task",
        _ => unreachable!("Google import validates event/task kinds"),
    }
}

const fn status_name(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Inbox => "inbox",
        ItemStatus::Planned => "planned",
        ItemStatus::Scheduled => "scheduled",
        ItemStatus::InProgress => "in_progress",
        ItemStatus::Paused => "paused",
        ItemStatus::Completed => "completed",
        ItemStatus::Skipped => "skipped",
        ItemStatus::Cancelled => "cancelled",
    }
}

fn split_columns(policy: &SplitPolicy) -> (bool, Option<i32>, Option<i32>) {
    match policy {
        SplitPolicy::Indivisible => (false, None, None),
        SplitPolicy::Splittable {
            minimum_chunk_seconds,
            maximum_chunk_seconds,
        } => (
            true,
            i32::try_from(*minimum_chunk_seconds).ok(),
            i32::try_from(*maximum_chunk_seconds).ok(),
        ),
    }
}

fn internal<T>(_error: T) -> GoogleSyncRepositoryError {
    GoogleSyncRepositoryError::Internal
}

fn u64_to_i64(value: u64) -> Result<i64, GoogleSyncRepositoryError> {
    i64::try_from(value).map_err(internal)
}

fn i64_to_u64(value: i64) -> Result<u64, GoogleSyncRepositoryError> {
    u64::try_from(value).map_err(internal)
}

fn mapping_hash(row: &PgRow, column: &str) -> Result<[u8; 32], GoogleSyncRepositoryError> {
    let value: Option<Vec<u8>> = row.try_get(column).map_err(internal)?;
    value.map_or(Ok([0; 32]), |value| value.try_into().map_err(internal))
}

fn u32_to_i32(value: u32) -> Result<i32, GoogleSyncRepositoryError> {
    i32::try_from(value).map_err(internal)
}

fn i32_to_u32(value: i32) -> Result<u32, GoogleSyncRepositoryError> {
    u32::try_from(value).map_err(internal)
}

fn i16_to_u8(value: i16) -> Result<u8, GoogleSyncRepositoryError> {
    u8::try_from(value).map_err(internal)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::Duration;
    use serde_json::json;
    use sqlx::{
        ConnectOptions, Executor,
        postgres::{PgConnectOptions, PgPoolOptions},
    };

    use crate::{
        google_oauth::{GoogleOAuthRepository, OAuthIdempotency},
        google_sync::OutboundOperation,
        items::{ItemKind, NewItem},
        persistence::{MIGRATOR, PostgresGoogleOAuthRepository},
    };

    use super::*;

    struct SyncFixture {
        database: TestDatabase,
        scope: DatabaseScope,
        account_id: Uuid,
        repository: PostgresGoogleSyncRepository,
        collection: GoogleSyncCollection,
        claim: SyncClaim,
        now: DateTime<Utc>,
    }

    async fn sync_fixture(database_url: &str) -> SyncFixture {
        let database = TestDatabase::create(database_url).await;
        MIGRATOR
            .run(&database.pool)
            .await
            .expect("migrations apply");
        let scope = seed_scope(&database.pool).await;
        sqlx::query("INSERT INTO google_oauth_scope_state (workspace_id, user_id) VALUES ($1, $2)")
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .execute(&database.pool)
            .await
            .expect("OAuth scope state fixture");
        let account_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
             display_label, encrypted_credentials, credential_key_version, granted_scopes, status, \
             sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, 'Google owner', \
             $5, 1, ARRAY[$6, $7], 'active', true, true)",
        )
        .bind(account_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(format!("sync-fixture-{account_id}"))
        .bind(vec![7_u8; 64])
        .bind(GOOGLE_CALENDAR_SCOPE)
        .bind(GOOGLE_TASKS_SCOPE)
        .execute(&database.pool)
        .await
        .expect("Google account fixture");
        let repository = PostgresGoogleSyncRepository::new(database.pool.clone(), scope);
        let now: DateTime<Utc> = "2026-08-29T10:00:00Z".parse().expect("time");
        let collections = repository
            .replace_discovered(
                account_id,
                None,
                GoogleCollectionKind::Calendar,
                vec![DiscoveredCollection {
                    kind: GoogleCollectionKind::Calendar,
                    remote_id: "primary@example.test".to_owned(),
                    display_name: "Primary".to_owned(),
                    provider_access_role: Some("owner".to_owned()),
                    provider_primary: true,
                    provider_selected: true,
                    provider_hidden: false,
                    provider_deleted: false,
                }],
                now,
            )
            .await
            .expect("discovery persists");
        let collection = repository
            .configure_collection(
                account_id,
                collections[0].id,
                collections[0].revision,
                true,
                true,
                GoogleSyncRole::Writable,
                now,
            )
            .await
            .expect("writable owner calendar");
        repository
            .request_refresh(account_id, now)
            .await
            .expect("refresh requested");
        let claim = repository
            .claim_due(now, now + Duration::minutes(10))
            .await
            .expect("claim query")
            .expect("due run");
        SyncFixture {
            database,
            scope,
            account_id,
            repository,
            collection,
            claim,
            now,
        }
    }

    fn oauth_idempotency(
        namespace: &'static str,
        marker: u8,
        now: DateTime<Utc>,
    ) -> OAuthIdempotency {
        OAuthIdempotency {
            namespace,
            key_hash: [marker; 32],
            request_fingerprint: [marker.wrapping_add(1); 32],
            expires_at: now + Duration::days(1),
        }
    }

    fn local_firm_block(id: Uuid, title: &str, now: DateTime<Utc>) -> Item {
        Item::new(
            NewItem {
                id,
                kind: ItemKind::Event,
                status: ItemStatus::Scheduled,
                title: title.to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(3600),
                deadline_at: Some(now + Duration::hours(1)),
                earliest_start_at: Some(now),
                recurrence: None,
                flexible_constraints: json!({"dayweave_firm_block": {"owned": true}}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            },
            now,
        )
        .expect("local firm block")
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_full_snapshot_sweep_removes_absent_external_and_conflicts_guarded_items() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL Google sync test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let repository = &fixture.repository;
        for (remote_id, title, hash) in [
            ("snapshot-retained", "Retained", [51; 32]),
            ("snapshot-absent", "Absent", [52; 32]),
            ("snapshot-locally-edited", "Locally edited", [53; 32]),
        ] {
            repository
                .apply_remote_item(
                    &fixture.claim,
                    remote_event(
                        fixture.account_id,
                        fixture.collection.id,
                        fixture.collection.revision,
                        remote_id,
                        title,
                        hash,
                    ),
                    fixture.now,
                )
                .await
                .expect("snapshot fixture imported");
        }
        let locally_edited_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = 'snapshot-locally-edited'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("locally edited mapping");
        sqlx::query(
            "UPDATE items SET title = 'Retain local edit', revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(locally_edited_id)
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("local edit fixture");

        let owned = local_firm_block(Uuid::new_v4(), "Owned block", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("owned item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &owned)
            .await
            .expect("owned item fixture");
        transaction.commit().await.expect("owned item commit");
        repository
            .enqueue_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: owned.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "snapshot-owned"}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("owned publication queued");
        let mut observed_owned = remote_event(
            fixture.account_id,
            fixture.collection.id,
            fixture.collection.revision,
            "snapshot-owned",
            "Owned block",
            [54; 32],
        );
        observed_owned.dayweave_item_id = Some(owned.id);
        let mut missing_etag = observed_owned.clone();
        missing_etag.remote_etag = None;
        assert_eq!(
            repository
                .apply_remote_item(&fixture.claim, missing_etag, fixture.now)
                .await
                .expect("missing ETag marker is retained as a conflict"),
            ImportOutcome::Conflict
        );
        let unresolved_local_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = 'snapshot-owned'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("missing ETag conflict mapping");
        assert!(unresolved_local_id.is_none());
        repository
            .apply_remote_item(&fixture.claim, observed_owned, fixture.now)
            .await
            .expect("owned identity recovered");
        let mut owned_without_etag = remote_event(
            fixture.account_id,
            fixture.collection.id,
            fixture.collection.revision,
            "snapshot-owned",
            "Owned block edited without a conditional token",
            [55; 32],
        );
        owned_without_etag.dayweave_item_id = Some(owned.id);
        owned_without_etag.remote_etag = None;
        assert_eq!(
            repository
                .apply_remote_item(&fixture.claim, owned_without_etag, fixture.now)
                .await
                .expect("owned resource without ETag conflicts"),
            ImportOutcome::Conflict
        );
        let retained_etag: Option<String> = sqlx::query_scalar(
            "SELECT remote_etag FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = 'snapshot-owned'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("owned ETag retained");
        assert_eq!(retained_etag.as_deref(), Some("etag-snapshot-owned"));

        let counts = repository
            .sweep_full_snapshot(
                &fixture.claim,
                fixture.collection.id,
                fixture.collection.revision,
                &["snapshot-retained".to_owned()],
                fixture.now + Duration::seconds(2),
            )
            .await
            .expect("complete snapshot swept");
        assert_eq!(counts.deleted, 1);
        assert_eq!(counts.conflicts, 2);
        let absent_trashed: bool = sqlx::query_scalar(
            "SELECT item.trashed_at IS NOT NULL FROM items item JOIN provider_sync_mappings mapping \
             ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id = 'snapshot-absent'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("absent item state");
        assert!(absent_trashed);
        let retained_states: Vec<bool> = sqlx::query_scalar(
            "SELECT item.trashed_at IS NULL FROM items item JOIN provider_sync_mappings mapping \
             ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id IN ('snapshot-retained', 'snapshot-locally-edited', \
                   'snapshot-owned') ORDER BY mapping.remote_resource_id",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_all(&fixture.database.pool)
        .await
        .expect("guarded snapshot states");
        assert_eq!(retained_states, vec![true, true, true]);
        let owned_conflict: String = sqlx::query_scalar(
            "SELECT last_error_code FROM google_sync_outbox WHERE workspace_id = $1 \
             AND collection_id = $2 AND item_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(owned.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("owned provider deletion conflict");
        assert_eq!(owned_conflict, "provider_deleted_dayweave_owned_item");
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_pause_disconnect_and_post_provider_guardians_revoke_stale_work() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL Google sync test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let repository = &fixture.repository;
        let stale_item = local_firm_block(Uuid::new_v4(), "Stale response", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("stale item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &stale_item)
            .await
            .expect("stale item fixture");
        transaction.commit().await.expect("stale item commit");
        let stale_outbox = repository
            .enqueue_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: stale_item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "stale-response"}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("stale response queued");
        let stale_work = repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("stale response claim")
            .expect("stale response work");
        sqlx::query(
            "UPDATE items SET revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(stale_item.id)
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("canonical revision changed during provider call");
        assert_eq!(
            repository
                .complete_outbound(
                    &stale_work,
                    OutboundResult {
                        remote_resource_id: "stale-response".to_owned(),
                        remote_etag: Some("etag-stale".to_owned()),
                        remote_updated_at: Some(fixture.now),
                        payload_hash: [60; 32],
                    },
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        let stale_state: (String, String) = sqlx::query_as(
            "SELECT state, last_error_code FROM google_sync_outbox WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(stale_outbox.outbox_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("stale response state");
        assert_eq!(
            stale_state,
            (
                "superseded".to_owned(),
                "superseded_during_delivery".to_owned()
            )
        );

        let pause_item = local_firm_block(Uuid::new_v4(), "Pause race", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("pause item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &pause_item)
            .await
            .expect("pause item fixture");
        transaction.commit().await.expect("pause item commit");
        let pause_outbox = repository
            .enqueue_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: pause_item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "pause-race"}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("pause work queued");
        let pause_work = repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("pause claim")
            .expect("pause work");
        let oauth =
            PostgresGoogleOAuthRepository::new(fixture.database.pool.clone(), fixture.scope);
        let paused = oauth
            .set_paused(
                fixture.account_id,
                1,
                true,
                fixture.now + Duration::minutes(1),
                fixture.now - Duration::minutes(2),
                oauth_idempotency("google_oauth_pause", 70, fixture.now),
            )
            .await
            .expect("account paused");
        let stale_remote = remote_event(
            fixture.account_id,
            fixture.collection.id,
            fixture.collection.revision,
            "fetched-before-pause",
            "Must not commit",
            [61; 32],
        );
        assert_eq!(
            repository
                .apply_remote_item(
                    &fixture.claim,
                    stale_remote,
                    fixture.now + Duration::minutes(1),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        assert_eq!(
            repository
                .mark_rejected(
                    &fixture.claim,
                    fixture.collection.id,
                    fixture.collection.revision,
                    "rejected-before-pause",
                    "provider_metadata_invalid",
                    fixture.now + Duration::minutes(1),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        assert_eq!(
            repository
                .store_cursor(
                    &fixture.claim,
                    fixture.collection.id,
                    fixture.collection.revision,
                    &format!("calendar:{}", fixture.collection.id),
                    None,
                    vec![1; 64],
                    1,
                    Some(fixture.now),
                    fixture.now + Duration::minutes(1),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        assert_eq!(
            repository
                .complete_outbound(
                    &pause_work,
                    OutboundResult {
                        remote_resource_id: "pause-race".to_owned(),
                        remote_etag: Some("etag-pause".to_owned()),
                        remote_updated_at: Some(fixture.now),
                        payload_hash: [62; 32],
                    },
                    fixture.now + Duration::minutes(1),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        let pause_state: (String, Option<Uuid>, String) = sqlx::query_as(
            "SELECT state, claim_id, last_error_code FROM google_sync_outbox \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(pause_outbox.outbox_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("pause outbox state");
        assert_eq!(
            pause_state,
            ("backoff".to_owned(), None, "account_paused".to_owned())
        );
        assert!(
            repository
                .claim_due(
                    fixture.now + Duration::minutes(1),
                    fixture.now + Duration::minutes(11),
                )
                .await
                .expect("paused claim scan")
                .is_none()
        );
        let paused_run = repository
            .run_status(fixture.account_id)
            .await
            .expect("paused run status")
            .expect("run row");
        assert_eq!(paused_run.state, GoogleSyncRunState::Idle);
        assert_eq!(
            paused_run.last_error_code.as_deref(),
            Some("account_paused")
        );

        let resumed = oauth
            .set_paused(
                fixture.account_id,
                paused.account.revision,
                false,
                fixture.now + Duration::minutes(2),
                fixture.now - Duration::minutes(2),
                oauth_idempotency("google_oauth_resume", 72, fixture.now),
            )
            .await
            .expect("account resumed");
        let resumed_claim = repository
            .claim_due(
                fixture.now + Duration::minutes(2),
                fixture.now + Duration::minutes(12),
            )
            .await
            .expect("resumed claim scan")
            .expect("resumed run claim");
        assert!(matches!(
            repository
                .claim_outbound(&fixture.claim, fixture.now + Duration::minutes(2))
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        ));
        assert_eq!(
            repository
                .replace_discovered(
                    fixture.account_id,
                    Some(&fixture.claim),
                    GoogleCollectionKind::Calendar,
                    vec![DiscoveredCollection {
                        kind: GoogleCollectionKind::Calendar,
                        remote_id: "primary@example.test".to_owned(),
                        display_name: "Stale discovery page".to_owned(),
                        provider_access_role: Some("reader".to_owned()),
                        provider_primary: true,
                        provider_selected: true,
                        provider_hidden: false,
                        provider_deleted: false,
                    }],
                    fixture.now + Duration::minutes(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        let retained_discovery: (String, String) = sqlx::query_as(
            "SELECT display_name, sync_role FROM google_sync_collections \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("stale discovery did not mutate collection");
        assert_eq!(
            retained_discovery,
            ("Primary".to_owned(), "writable".to_owned())
        );
        sqlx::query(
            "UPDATE provider_accounts SET granted_scopes = ARRAY[$3] \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .bind(GOOGLE_CALENDAR_READONLY_SCOPE)
        .execute(&fixture.database.pool)
        .await
        .expect("write scope removed");
        assert_eq!(
            repository
                .configure_collection(
                    fixture.account_id,
                    fixture.collection.id,
                    fixture.collection.revision,
                    true,
                    true,
                    GoogleSyncRole::Writable,
                    fixture.now + Duration::minutes(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::WriteScopeMissing)
        );
        assert!(matches!(
            repository
                .claim_outbound(&resumed_claim, fixture.now + Duration::minutes(2))
                .await,
            Err(GoogleSyncRepositoryError::WriteScopeMissing)
        ));
        sqlx::query(
            "UPDATE provider_accounts SET granted_scopes = ARRAY[]::text[] \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .execute(&fixture.database.pool)
        .await
        .expect("read scope removed");
        assert_eq!(
            repository
                .replace_discovered(
                    fixture.account_id,
                    Some(&resumed_claim),
                    GoogleCollectionKind::Calendar,
                    Vec::new(),
                    fixture.now + Duration::minutes(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ReadScopeMissing)
        );
        assert_eq!(
            repository
                .apply_remote_item(
                    &resumed_claim,
                    remote_event(
                        fixture.account_id,
                        fixture.collection.id,
                        fixture.collection.revision,
                        "fetched-before-scope-loss",
                        "Must not commit",
                        [64; 32],
                    ),
                    fixture.now + Duration::minutes(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ReadScopeMissing)
        );
        let scope_race_mapping: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND provider_account_id = $2 AND remote_resource_id = 'fetched-before-scope-loss'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("scope-race mapping count");
        assert_eq!(scope_race_mapping, 0);
        oauth
            .claim_disconnect(
                fixture.account_id,
                resumed.account.revision,
                Uuid::new_v4(),
                fixture.now + Duration::minutes(3),
                fixture.now,
                fixture.now,
                oauth_idempotency("google_oauth_disconnect", 74, fixture.now),
            )
            .await
            .expect("disconnect guardian claimed");
        assert_eq!(
            repository
                .apply_remote_item(
                    &resumed_claim,
                    remote_event(
                        fixture.account_id,
                        fixture.collection.id,
                        fixture.collection.revision,
                        "fetched-before-disconnect",
                        "Must not commit",
                        [63; 32],
                    ),
                    fixture.now + Duration::minutes(3),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        assert!(
            repository
                .claim_due(
                    fixture.now + Duration::minutes(3),
                    fixture.now + Duration::minutes(13),
                )
                .await
                .expect("disconnect claim scan")
                .is_none()
        );
        let disconnected_run: (String, Option<Uuid>) = sqlx::query_as(
            "SELECT state, claim_id FROM google_sync_runs WHERE workspace_id = $1 \
             AND provider_account_id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("disconnect run state");
        assert_eq!(disconnected_run, ("idle".to_owned(), None));
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_sync_fences_local_edits_tombstones_cursors_and_outbox() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL Google sync test skipped");
            return;
        };
        let database = TestDatabase::create(&database_url).await;
        MIGRATOR
            .run(&database.pool)
            .await
            .expect("migrations apply");
        let scope = seed_scope(&database.pool).await;
        let account_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
             display_label, encrypted_credentials, credential_key_version, granted_scopes, status, \
             sync_enabled, is_default) VALUES ($1, $2, $3, 'google', 'subject-1', 'Google owner', \
             $4, 1, ARRAY['https://www.googleapis.com/auth/calendar'], 'active', true, true)",
        )
        .bind(account_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(vec![7_u8; 64])
        .execute(&database.pool)
        .await
        .expect("Google account fixture");

        let repository = PostgresGoogleSyncRepository::new(database.pool.clone(), scope);
        let now: DateTime<Utc> = "2026-08-29T10:00:00Z".parse().expect("time");
        let collections = repository
            .replace_discovered(
                account_id,
                None,
                GoogleCollectionKind::Calendar,
                vec![DiscoveredCollection {
                    kind: GoogleCollectionKind::Calendar,
                    remote_id: "primary@example.test".to_owned(),
                    display_name: "Primary".to_owned(),
                    provider_access_role: Some("owner".to_owned()),
                    provider_primary: true,
                    provider_selected: true,
                    provider_hidden: false,
                    provider_deleted: false,
                }],
                now,
            )
            .await
            .expect("discovery persists");
        let collection = repository
            .configure_collection(
                account_id,
                collections[0].id,
                collections[0].revision,
                true,
                true,
                GoogleSyncRole::Writable,
                now,
            )
            .await
            .expect("writable owner calendar");

        repository
            .request_refresh(account_id, now)
            .await
            .expect("manual refresh durable");
        let claim = repository
            .claim_due(now, now + Duration::minutes(10))
            .await
            .expect("claim query")
            .expect("due run");
        assert_eq!(claim.account_id, account_id);

        let first = remote_event(
            account_id,
            collection.id,
            collection.revision,
            "remote-1",
            "Provider title",
            [1; 32],
        );
        assert_eq!(
            repository
                .apply_remote_item(&claim, first, now)
                .await
                .expect("first import"),
            ImportOutcome::Created
        );
        let mapping = sqlx::query(
            "SELECT local_entity_id, local_revision FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 AND remote_resource_id = 'remote-1'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("mapping");
        let imported_id: Uuid = mapping.try_get("local_entity_id").expect("local id");
        let imported_revision: i64 = mapping.try_get("local_revision").expect("revision");
        sqlx::query(
            "UPDATE items SET title = 'Local edit', revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(imported_id)
        .bind(now + Duration::minutes(1))
        .execute(&database.pool)
        .await
        .expect("simulate canonical local edit");
        assert_eq!(imported_revision, 1);
        let provider_update = remote_event(
            account_id,
            collection.id,
            collection.revision,
            "remote-1",
            "Provider changed",
            [2; 32],
        );
        assert_eq!(
            repository
                .apply_remote_item(&claim, provider_update, now + Duration::minutes(2))
                .await
                .expect("conflict recorded"),
            ImportOutcome::Conflict
        );
        let retained_title: String =
            sqlx::query_scalar("SELECT title FROM items WHERE workspace_id = $1 AND id = $2")
                .bind(scope.workspace_id)
                .bind(imported_id)
                .fetch_one(&database.pool)
                .await
                .expect("retained local title");
        assert_eq!(retained_title, "Local edit");
        let tombstone = remote_tombstone(
            account_id,
            collection.id,
            collection.revision,
            "remote-1",
            [3; 32],
        );
        assert_eq!(
            repository
                .apply_remote_item(&claim, tombstone, now + Duration::minutes(3))
                .await
                .expect("delete conflict"),
            ImportOutcome::Conflict
        );
        let still_active: bool = sqlx::query_scalar(
            "SELECT trashed_at IS NULL FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(imported_id)
        .fetch_one(&database.pool)
        .await
        .expect("conflicted local item retained");
        assert!(still_active);

        repository
            .apply_remote_item(
                &claim,
                remote_event(
                    account_id,
                    collection.id,
                    collection.revision,
                    "remote-2",
                    "Delete me",
                    [4; 32],
                ),
                now,
            )
            .await
            .expect("second import");
        assert_eq!(
            repository
                .apply_remote_item(
                    &claim,
                    remote_tombstone(
                        account_id,
                        collection.id,
                        collection.revision,
                        "remote-2",
                        [5; 32],
                    ),
                    now + Duration::minutes(4),
                )
                .await
                .expect("unchanged item trash"),
            ImportOutcome::Deleted
        );
        let deleted: bool = sqlx::query_scalar(
            "SELECT item.trashed_at IS NOT NULL FROM items item JOIN provider_sync_mappings mapping \
             ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id = 'remote-2'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("remote delete moved item to trash");
        assert!(deleted);
        assert_eq!(
            repository
                .apply_remote_item(
                    &claim,
                    remote_event(
                        account_id,
                        collection.id,
                        collection.revision,
                        "remote-2",
                        "Restored at Google",
                        [42; 32],
                    ),
                    now + Duration::minutes(5),
                )
                .await
                .expect("provider restoration"),
            ImportOutcome::Updated
        );
        let restored: bool = sqlx::query_scalar(
            "SELECT item.trashed_at IS NULL FROM items item JOIN provider_sync_mappings mapping \
             ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id = 'remote-2'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("provider restoration clears recoverable trash");
        assert!(restored);

        repository
            .apply_remote_item(
                &claim,
                remote_event(
                    account_id,
                    collection.id,
                    collection.revision,
                    "declined-after-import",
                    "Invitation initially accepted",
                    [40; 32],
                ),
                now,
            )
            .await
            .expect("accepted invitation imported");
        assert_eq!(
            repository
                .apply_remote_item(
                    &claim,
                    remote_tombstone(
                        account_id,
                        collection.id,
                        collection.revision,
                        "declined-after-import",
                        [41; 32],
                    ),
                    now + Duration::minutes(5),
                )
                .await
                .expect("self-decline projects as removal"),
            ImportOutcome::Deleted
        );
        let declined_trashed: bool = sqlx::query_scalar(
            "SELECT item.trashed_at IS NOT NULL FROM items item \
             JOIN provider_sync_mappings mapping ON mapping.workspace_id = item.workspace_id \
               AND mapping.local_entity_id = item.id WHERE mapping.workspace_id = $1 \
               AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id = 'declined-after-import'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("declined invitation is in recoverable trash");
        assert!(declined_trashed);

        assert_eq!(
            repository
                .apply_remote_item(
                    &claim,
                    remote_tombstone(
                        account_id,
                        collection.id,
                        collection.revision,
                        "never-observed",
                        [43; 32],
                    ),
                    now + Duration::minutes(5),
                )
                .await
                .expect("unknown tombstone is harmless"),
            ImportOutcome::Unchanged
        );
        let unknown_tombstone_mapping: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = 'never-observed'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("unknown tombstone mapping count");
        assert_eq!(unknown_tombstone_mapping, 0);

        let mut forged_marker = remote_event(
            account_id,
            collection.id,
            collection.revision,
            "unrecognized-marker",
            "Must not import",
            [44; 32],
        );
        forged_marker.dayweave_item_id = Some(Uuid::new_v4());
        let canonical_count_before_forgery: i64 =
            sqlx::query_scalar("SELECT count(*) FROM items WHERE workspace_id = $1")
                .bind(scope.workspace_id)
                .fetch_one(&database.pool)
                .await
                .expect("canonical count before forged marker");
        assert_eq!(
            repository
                .apply_remote_item(&claim, forged_marker.clone(), now + Duration::minutes(5),)
                .await
                .expect("unrecognized marker is recorded fail-closed"),
            ImportOutcome::Conflict
        );
        assert_eq!(
            repository
                .apply_remote_item(&claim, forged_marker, now + Duration::minutes(5))
                .await
                .expect("unrecognized marker replay remains fail-closed"),
            ImportOutcome::Conflict
        );
        let canonical_count_after_forgery: i64 =
            sqlx::query_scalar("SELECT count(*) FROM items WHERE workspace_id = $1")
                .bind(scope.workspace_id)
                .fetch_one(&database.pool)
                .await
                .expect("canonical count after forged marker replay");
        assert_eq!(
            canonical_count_after_forgery, canonical_count_before_forgery,
            "replaying an unrecognized ownership marker must never create a canonical duplicate"
        );
        let forged_mapping = sqlx::query(
            "SELECT local_entity_id, ownership, sync_state, conflict_metadata->>'reason' AS reason \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
               AND remote_resource_id = 'unrecognized-marker'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("unrecognized marker mapping");
        assert!(
            forged_mapping
                .try_get::<Option<Uuid>, _>("local_entity_id")
                .expect("nullable local id")
                .is_none()
        );
        assert_eq!(
            forged_mapping
                .try_get::<String, _>("ownership")
                .expect("external ownership"),
            "external"
        );
        assert_eq!(
            forged_mapping
                .try_get::<String, _>("sync_state")
                .expect("conflict state"),
            "conflict"
        );
        assert_eq!(
            forged_mapping
                .try_get::<String, _>("reason")
                .expect("conflict reason"),
            "unrecognized_dayweave_marker"
        );

        let mut reprojection = remote_event(
            account_id,
            collection.id,
            collection.revision,
            "remote-3",
            "Visible title",
            [6; 32],
        );
        assert_eq!(
            repository
                .apply_remote_item(&claim, reprojection.clone(), now)
                .await
                .expect("projection fixture"),
            ImportOutcome::Created
        );
        reprojection.item.as_mut().expect("projected item").title = "Redacted title".to_owned();
        reprojection.remote_projection_hash = [7; 32];
        assert_eq!(
            repository
                .apply_remote_item(&claim, reprojection, now + Duration::minutes(5))
                .await
                .expect("unchanged provider payload re-projected"),
            ImportOutcome::Updated
        );

        let collection_key = format!("calendar:{}", collection.id);
        repository
            .store_cursor(
                &claim,
                collection.id,
                collection.revision,
                &collection_key,
                None,
                vec![8; 64],
                1,
                Some(now + Duration::minutes(5)),
                now + Duration::minutes(5),
            )
            .await
            .expect("initial encrypted cursor");
        let cursor = repository
            .cursor(account_id, &collection_key)
            .await
            .expect("cursor lookup")
            .expect("cursor");
        assert_eq!(cursor.revision, 1);
        assert_eq!(
            repository
                .store_cursor(
                    &claim,
                    collection.id,
                    collection.revision,
                    &collection_key,
                    Some(9),
                    vec![9; 64],
                    1,
                    Some(now),
                    now,
                )
                .await,
            Err(GoogleSyncRepositoryError::CursorConflict)
        );

        let reconfigured = repository
            .configure_collection(
                account_id,
                collection.id,
                collection.revision,
                true,
                false,
                GoogleSyncRole::Writable,
                now + Duration::minutes(6),
            )
            .await
            .expect("visibility update");
        assert_eq!(reconfigured.revision, collection.revision + 1);
        assert!(
            repository
                .cursor(account_id, &collection_key)
                .await
                .expect("cursor lookup after reconfiguration")
                .is_none(),
            "projection-affecting configuration must force a full import"
        );
        assert_eq!(
            repository
                .apply_remote_item(
                    &claim,
                    remote_event(
                        account_id,
                        collection.id,
                        collection.revision,
                        "stale-worker",
                        "Must not import",
                        [8; 32],
                    ),
                    now + Duration::minutes(7),
                )
                .await,
            Err(GoogleSyncRepositoryError::CursorConflict),
            "an in-flight worker must not project with stale visibility or role"
        );

        let local = crate::items::Item::new(
            NewItem {
                id: Uuid::new_v4(),
                kind: ItemKind::Event,
                status: ItemStatus::Scheduled,
                title: "DayWeave firm block".to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(3600),
                deadline_at: Some(now + Duration::hours(1)),
                earliest_start_at: Some(now),
                recurrence: None,
                flexible_constraints: json!({"dayweave_firm_block": {"owned": true}}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            },
            now,
        )
        .expect("local item");
        let mut transaction = database.pool.begin().await.expect("item transaction");
        insert_imported_item(&mut transaction, scope, &local)
            .await
            .expect("local item fixture");
        transaction.commit().await.expect("item fixture commit");
        let prepared = PreparedOutbound {
            entity_kind: "calendar_event",
            item: local.clone(),
            operation: OutboundOperation::Upsert,
            payload: json!({"id": "stable-provider-id"}),
        };
        let queued = repository
            .enqueue_outbound(account_id, prepared.clone(), collection.id, now)
            .await
            .expect("outbound queued");
        let replay = repository
            .enqueue_outbound(account_id, prepared, collection.id, now)
            .await
            .expect("outbound replay");
        assert_eq!(queued.outbox_id, replay.outbox_id);
        assert!(replay.replayed);
        sqlx::query(
            "UPDATE items SET title = 'DayWeave firm block revised', revision = 2, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(local.id)
        .bind(now + Duration::seconds(30))
        .execute(&database.pool)
        .await
        .expect("new canonical revision");
        let mut revised_local = local.clone();
        revised_local.title = "DayWeave firm block revised".to_owned();
        revised_local.revision = 2;
        revised_local.updated_at = now + Duration::seconds(30);
        let revised = repository
            .enqueue_outbound(
                account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: revised_local,
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "stable-provider-id"}),
                },
                collection.id,
                now + Duration::seconds(30),
            )
            .await
            .expect("newer outbound revision");
        assert_ne!(revised.outbox_id, queued.outbox_id);
        let superseded_state: String = sqlx::query_scalar(
            "SELECT state FROM google_sync_outbox WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(queued.outbox_id)
        .fetch_one(&database.pool)
        .await
        .expect("older outbox state");
        assert_eq!(superseded_state, "superseded");
        let mut observed_before_ack = remote_event(
            account_id,
            collection.id,
            reconfigured.revision,
            "stable-provider-id",
            "DayWeave firm block",
            [9; 32],
        );
        observed_before_ack.dayweave_item_id = Some(local.id);
        assert_eq!(
            repository
                .apply_remote_item(&claim, observed_before_ack, now + Duration::minutes(1))
                .await
                .expect("provider acceptance recovered through durable marker"),
            ImportOutcome::Unchanged
        );
        let recovered_mapping = sqlx::query(
            "SELECT local_entity_id, ownership FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 \
               AND remote_resource_id = 'stable-provider-id'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("recovered ownership mapping");
        assert_eq!(
            recovered_mapping
                .try_get::<Uuid, _>("local_entity_id")
                .expect("recovered local item"),
            local.id
        );
        assert_eq!(
            recovered_mapping
                .try_get::<String, _>("ownership")
                .expect("recovered ownership"),
            "dayweave"
        );
        let mut duplicate_marker = remote_event(
            account_id,
            collection.id,
            reconfigured.revision,
            "duplicate-marker-provider-id",
            "Forged duplicate ownership marker",
            [45; 32],
        );
        duplicate_marker.dayweave_item_id = Some(local.id);
        assert_eq!(
            repository
                .apply_remote_item(&claim, duplicate_marker, now + Duration::minutes(1))
                .await
                .expect("a second remote identity is retained as a conflict"),
            ImportOutcome::Conflict
        );
        let owned_remote_ids: Vec<String> = sqlx::query_scalar(
            "SELECT remote_resource_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND provider_account_id = $2 AND collection_id = $3 AND local_entity_id = $4 \
             AND tombstoned_at IS NULL ORDER BY remote_resource_id",
        )
        .bind(scope.workspace_id)
        .bind(account_id)
        .bind(collection.id)
        .bind(local.id)
        .fetch_all(&database.pool)
        .await
        .expect("single retained provider identity");
        assert_eq!(owned_remote_ids, vec!["stable-provider-id".to_owned()]);
        let duplicate_mapping = sqlx::query(
            "SELECT local_entity_id, sync_state, conflict_metadata->>'reason' AS reason \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
               AND remote_resource_id = 'duplicate-marker-provider-id'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("duplicate marker conflict mapping");
        assert!(
            duplicate_mapping
                .try_get::<Option<Uuid>, _>("local_entity_id")
                .expect("nullable duplicate local id")
                .is_none()
        );
        assert_eq!(
            duplicate_mapping
                .try_get::<String, _>("sync_state")
                .expect("duplicate marker state"),
            "conflict"
        );
        assert_eq!(
            duplicate_mapping
                .try_get::<String, _>("reason")
                .expect("duplicate marker reason"),
            "unrecognized_dayweave_marker"
        );
        let work = repository
            .claim_outbound(&claim, now + Duration::minutes(1))
            .await
            .expect("claim outbound")
            .expect("outbound work");
        assert_eq!(
            work.remote_resource_id.as_deref(),
            Some("stable-provider-id")
        );
        assert_eq!(work.item_revision, 2);
        repository
            .renew_outbound(&work, now + Duration::minutes(1))
            .await
            .expect("long delivery lease renewed");
        repository
            .fail_outbound(
                &work,
                "backoff",
                "provider_temporary",
                now + Duration::hours(1),
                now + Duration::minutes(1),
            )
            .await
            .expect("transient delivery retained");
        let backoff = repository
            .outbox_counts(account_id)
            .await
            .expect("outbox status");
        assert_eq!(
            backoff.last_error_code.as_deref(),
            Some("provider_temporary")
        );
        repository
            .request_refresh(account_id, now + Duration::minutes(2))
            .await
            .expect("manual refresh advances retained delivery");
        let work = repository
            .claim_outbound(&claim, now + Duration::minutes(2))
            .await
            .expect("reclaim outbound")
            .expect("backoff advanced by manual refresh");
        repository
            .complete_outbound(
                &work,
                OutboundResult {
                    remote_resource_id: "stable-provider-id".to_owned(),
                    remote_etag: Some("etag-1".to_owned()),
                    remote_updated_at: Some(now),
                    payload_hash: [9; 32],
                },
                now + Duration::minutes(2),
            )
            .await
            .expect("publish acknowledgement");
        let ownership: String = sqlx::query_scalar(
            "SELECT ownership FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND local_entity_id = $3",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .bind(local.id)
        .fetch_one(&database.pool)
        .await
        .expect("DayWeave ownership mapping");
        assert_eq!(ownership, "dayweave");

        sqlx::query(
            "UPDATE items SET title = 'Third revision', revision = 3, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(local.id)
        .bind(now + Duration::minutes(3))
        .execute(&database.pool)
        .await
        .expect("third canonical revision");
        let mut third_revision = local.clone();
        third_revision.title = "Third revision".to_owned();
        third_revision.revision = 3;
        third_revision.updated_at = now + Duration::minutes(3);
        let third_outbound = repository
            .enqueue_outbound(
                account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: third_revision,
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "stable-provider-id"}),
                },
                collection.id,
                now + Duration::minutes(3),
            )
            .await
            .expect("third outbound revision");
        let mut provider_edit = remote_event(
            account_id,
            collection.id,
            reconfigured.revision,
            "stable-provider-id",
            "Edited outside DayWeave",
            [12; 32],
        );
        provider_edit.dayweave_item_id = Some(local.id);
        assert_eq!(
            repository
                .apply_remote_item(&claim, provider_edit, now + Duration::minutes(3))
                .await
                .expect("provider edit conflicts app-owned publication"),
            ImportOutcome::Conflict
        );
        let outbound_conflict = sqlx::query(
            "SELECT state, last_error_code FROM google_sync_outbox \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(third_outbound.outbox_id)
        .fetch_one(&database.pool)
        .await
        .expect("conflicted active outbound");
        assert_eq!(
            outbound_conflict
                .try_get::<String, _>("state")
                .expect("state"),
            "conflict"
        );
        assert_eq!(
            outbound_conflict
                .try_get::<String, _>("last_error_code")
                .expect("error code"),
            "provider_changed_dayweave_owned_item"
        );
        assert_eq!(
            repository
                .apply_remote_item(
                    &claim,
                    remote_tombstone(
                        account_id,
                        collection.id,
                        reconfigured.revision,
                        "stable-provider-id",
                        [11; 32],
                    ),
                    now + Duration::minutes(3),
                )
                .await
                .expect("app-owned external deletion conflict"),
            ImportOutcome::Conflict
        );
        let local_retained: bool = sqlx::query_scalar(
            "SELECT trashed_at IS NULL FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(local.id)
        .fetch_one(&database.pool)
        .await
        .expect("DayWeave-owned item retained");
        assert!(local_retained);

        let stale_local = crate::items::Item::new(
            NewItem {
                id: Uuid::new_v4(),
                kind: ItemKind::Event,
                status: ItemStatus::Scheduled,
                title: "Stale queued block".to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(1800),
                deadline_at: Some(now + Duration::hours(2)),
                earliest_start_at: Some(now + Duration::hours(1)),
                recurrence: None,
                flexible_constraints: json!({"dayweave_firm_block": {"owned": true}}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            },
            now,
        )
        .expect("stale local item");
        let mut transaction = database.pool.begin().await.expect("stale item transaction");
        insert_imported_item(&mut transaction, scope, &stale_local)
            .await
            .expect("stale item fixture");
        transaction.commit().await.expect("stale item commit");
        let stale_outbound = repository
            .enqueue_outbound(
                account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: stale_local.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "stale-provider-id"}),
                },
                collection.id,
                now + Duration::minutes(4),
            )
            .await
            .expect("stale outbound queued");
        sqlx::query(
            "UPDATE items SET revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(stale_local.id)
        .bind(now + Duration::minutes(5))
        .execute(&database.pool)
        .await
        .expect("canonical item changed without requeue");
        assert!(
            repository
                .claim_outbound(&claim, now + Duration::minutes(5))
                .await
                .expect("stale outbound scan")
                .is_none(),
            "a stale durable payload must not reach Google"
        );
        let stale_state: String = sqlx::query_scalar(
            "SELECT state FROM google_sync_outbox WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(stale_outbound.outbox_id)
        .fetch_one(&database.pool)
        .await
        .expect("stale outbox state");
        assert_eq!(stale_state, "superseded");
        let mut guarded_local = stale_local;
        guarded_local.revision = 2;
        guarded_local.updated_at = now + Duration::minutes(5);
        let guarded_outbound = repository
            .enqueue_outbound(
                account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: guarded_local,
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "stale-provider-id"}),
                },
                collection.id,
                now + Duration::minutes(6),
            )
            .await
            .expect("guardian-fenced outbound queued");
        let guarded_work = repository
            .claim_outbound(&claim, now + Duration::minutes(6))
            .await
            .expect("guardian delivery claim")
            .expect("guardian delivery work");
        assert_eq!(guarded_work.id, guarded_outbound.outbox_id);

        repository
            .store_cursor(
                &claim,
                collection.id,
                reconfigured.revision,
                &collection_key,
                None,
                vec![10; 64],
                1,
                Some(now + Duration::minutes(3)),
                now + Duration::minutes(3),
            )
            .await
            .expect("cursor before access downgrade");
        let downgraded = repository
            .replace_discovered(
                account_id,
                None,
                GoogleCollectionKind::Calendar,
                vec![DiscoveredCollection {
                    kind: GoogleCollectionKind::Calendar,
                    remote_id: "primary@example.test".to_owned(),
                    display_name: "Primary".to_owned(),
                    provider_access_role: Some("reader".to_owned()),
                    provider_primary: true,
                    provider_selected: true,
                    provider_hidden: false,
                    provider_deleted: false,
                }],
                now + Duration::minutes(8),
            )
            .await
            .expect("provider access downgrade");
        let downgraded = downgraded
            .into_iter()
            .find(|candidate| candidate.id == collection.id)
            .expect("calendar retained");
        assert_eq!(downgraded.sync_role, GoogleSyncRole::ReadOnly);
        assert_eq!(
            repository
                .renew_outbound(&guarded_work, now + Duration::minutes(8))
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost),
            "provider ACL downgrade must revoke an already claimed delivery"
        );
        let guarded_state = sqlx::query(
            "SELECT state, last_error_code FROM google_sync_outbox \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(guarded_outbound.outbox_id)
        .fetch_one(&database.pool)
        .await
        .expect("guardian outbox state");
        assert_eq!(
            guarded_state
                .try_get::<String, _>("state")
                .expect("guardian state"),
            "conflict"
        );
        assert_eq!(
            guarded_state
                .try_get::<String, _>("last_error_code")
                .expect("guardian code"),
            "collection_access_revoked"
        );
        assert!(
            repository
                .cursor(account_id, &collection_key)
                .await
                .expect("cursor after access downgrade")
                .is_none()
        );

        let counts = SyncCounts::default();
        repository
            .complete_claim(
                &claim,
                &counts,
                now + Duration::minutes(5),
                now + Duration::minutes(15),
            )
            .await
            .expect("run completion");
        assert_eq!(
            repository
                .run_status(account_id)
                .await
                .expect("status")
                .expect("run")
                .state,
            GoogleSyncRunState::Idle
        );
        database.destroy().await;
    }

    fn remote_event(
        account_id: Uuid,
        collection_id: Uuid,
        collection_revision: u64,
        remote_id: &str,
        title: &str,
        hash: [u8; 32],
    ) -> RemoteItemChange {
        RemoteItemChange {
            account_id,
            collection_id,
            collection_revision,
            dayweave_item_id: None,
            remote_id: remote_id.to_owned(),
            remote_parent_id: None,
            remote_etag: Some(format!("etag-{remote_id}")),
            remote_updated_at: None,
            remote_payload_hash: hash,
            remote_projection_hash: hash,
            item: Some(NewItem {
                id: Uuid::new_v4(),
                kind: ItemKind::Event,
                status: ItemStatus::Scheduled,
                title: title.to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(3600),
                deadline_at: Some("2026-08-29T11:00:00Z".parse().expect("end")),
                earliest_start_at: Some("2026-08-29T10:00:00Z".parse().expect("start")),
                recurrence: None,
                flexible_constraints: json!({"google_sync": {"remote_id": remote_id}}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            }),
        }
    }

    fn remote_tombstone(
        account_id: Uuid,
        collection_id: Uuid,
        collection_revision: u64,
        remote_id: &str,
        hash: [u8; 32],
    ) -> RemoteItemChange {
        RemoteItemChange {
            account_id,
            collection_id,
            collection_revision,
            dayweave_item_id: None,
            remote_id: remote_id.to_owned(),
            remote_parent_id: None,
            remote_etag: None,
            remote_updated_at: None,
            remote_payload_hash: hash,
            remote_projection_hash: hash,
            item: None,
        }
    }

    async fn seed_scope(pool: &PgPool) -> DatabaseScope {
        let scope = DatabaseScope {
            user_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
        };
        sqlx::query(
            "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
             VALUES ($1, $2, 'Google sync owner', 'UTC')",
        )
        .bind(scope.user_id)
        .bind(format!("sync-owner-{}", scope.user_id))
        .execute(pool)
        .await
        .expect("user fixture");
        sqlx::query(
            "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
             VALUES ($1, $2, $3, 'Google sync workspace', 'UTC')",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(format!("sync-{}", scope.workspace_id.simple()))
        .execute(pool)
        .await
        .expect("workspace fixture");
        sqlx::query(
            "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(pool)
        .await
        .expect("membership fixture");
        scope
    }

    struct TestDatabase {
        admin: PgPool,
        pool: PgPool,
        schema: String,
    }

    impl TestDatabase {
        async fn create(database_url: &str) -> Self {
            let options = PgConnectOptions::from_str(database_url)
                .expect("valid DAYWEAVE_TEST_DATABASE_URL")
                .disable_statement_logging();
            let admin = PgPoolOptions::new()
                .max_connections(2)
                .connect_with(options.clone())
                .await
                .expect("connect test PostgreSQL");
            let schema = format!("dayweave_google_sync_test_{}", Uuid::new_v4().simple());
            admin
                .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .await
                .expect("create isolated test schema");
            let connection_schema = schema.clone();
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .after_connect(move |connection, _| {
                    let statement = format!("SET search_path TO {connection_schema}");
                    Box::pin(async move {
                        connection.execute(AssertSqlSafe(statement)).await?;
                        Ok(())
                    })
                })
                .connect_with(options)
                .await
                .expect("connect isolated test pool");
            Self {
                admin,
                pool,
                schema,
            }
        }

        async fn destroy(self) {
            self.pool.close().await;
            self.admin
                .execute(AssertSqlSafe(format!(
                    "DROP SCHEMA {} CASCADE",
                    self.schema
                )))
                .await
                .expect("drop isolated test schema");
            self.admin.close().await;
        }
    }
}
