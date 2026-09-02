use std::collections::{BTreeMap, BTreeSet, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

#[cfg(test)]
use crate::google_sync::PreparedOutbound;
#[cfg(test)]
use sha2::{Digest as _, Sha256};

use crate::{
    config::{
        GOOGLE_CALENDAR_READONLY_SCOPE, GOOGLE_CALENDAR_SCOPE, GOOGLE_TASKS_READONLY_SCOPE,
        GOOGLE_TASKS_SCOPE,
    },
    google_sync::{
        CalendarProjectionBatch, CalendarProjectionResult, CalendarProjectionState,
        DiscoveredCollection, GoogleCalendarPolicy, GoogleCollectionKind, GoogleEventDisposition,
        GoogleOutboundAccepted, GoogleOutboundPreview, GoogleSyncCollection,
        GoogleSyncRefreshAccepted, GoogleSyncRepository, GoogleSyncRepositoryError, GoogleSyncRole,
        GoogleSyncRunState, GoogleSyncRunStatus, ImportOutcome, OutboundApprovalSpec,
        OutboundDispatchPermit, OutboundEnqueueSpec, OutboundOperation, OutboundPreviewSpec,
        OutboundResult, OutboundWork, OutboxCounts, RemoteCalendarSeriesChange, RemoteItemChange,
        StoredCursor, SyncClaim, SyncCounts, SyncFailureKind, outbound_intent_hash,
        outbound_preview_hash,
    },
    items::{Item, ItemStatus, ItemTombstone, NewItem, ReplaceItem, SplitPolicy},
};

use super::DatabaseScope;

const COLLECTION_COLUMNS: &str = "id, provider_account_id, collection_kind, remote_collection_id, display_name, \
    provider_access_role, provider_primary, provider_selected, provider_hidden, provider_deleted, \
    selected, visible, sync_role, confirmed_busy_policy, tentative_policy, free_policy, \
    all_day_policy, publish_all_day, publish_tentative, publish_free, revision, discovered_at, \
    configured_at, last_import_at, planning_projection_state, planning_generation, \
    planning_collection_revision, planning_window_start, planning_window_end, \
    planning_window_refreshed_at, created_at, updated_at";

const MAX_CALENDAR_PROJECTION_ENTRIES: usize = 10_000;
const MAX_CALENDAR_PROJECTION_WINDOW_DAYS: i64 = 150;

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

    #[cfg(test)]
    async fn enqueue_test_outbound(
        &self,
        account_id: Uuid,
        prepared: crate::google_sync::PreparedOutbound,
        collection_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<GoogleOutboundAccepted, GoogleSyncRepositoryError> {
        let collection = self.collection(account_id, collection_id).await?;
        let required_scope = match prepared.entity_kind {
            "calendar_event" => GOOGLE_CALENDAR_SCOPE,
            "task" => GOOGLE_TASKS_SCOPE,
            _ => return Err(GoogleSyncRepositoryError::CollectionNotWritable),
        };
        let preview_id = Uuid::new_v4();
        let mut capability_digest = Sha256::new();
        capability_digest.update(b"synthetic-google-approval");
        capability_digest.update(preview_id.as_bytes());
        let capability_hash: [u8; 32] = capability_digest.finalize().into();
        let request = crate::google_sync::OutboundRequest {
            collection_id,
            item_id: prepared.item.id,
            expected_item_revision: prepared.item.revision,
            operation: prepared.operation,
        };
        let preview = self
            .create_outbound_preview(
                OutboundPreviewSpec {
                    id: preview_id,
                    account_id,
                    collection_id,
                    collection_revision: collection.revision,
                    collection_remote_id: collection.remote_collection_id,
                    collection_display_name: collection.display_name,
                    required_scope,
                    prepared,
                    expires_at: now + chrono::Duration::minutes(10),
                },
                now,
            )
            .await?;
        let preview_hash = decode_hex_bytes(&preview.preview_hash)?;
        self.approve_outbound(
            OutboundApprovalSpec {
                account_id,
                preview_id,
                expected_preview_hash: preview_hash,
                capability_hash,
            },
            now,
        )
        .await?;
        self.enqueue_outbound(
            OutboundEnqueueSpec {
                account_id,
                request,
                capability_hash,
            },
            now,
        )
        .await
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)] // SQL transaction bodies keep each durable fence atomic.
impl GoogleSyncRepository for PostgresGoogleSyncRepository {
    async fn verify_or_initialize_identity_root(
        &self,
        identity_key_version: u32,
        root_verifier: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        if identity_key_version == 0 {
            return Err(GoogleSyncRepositoryError::IdentityRootMismatch);
        }
        let identity_key_version = i64::from(identity_key_version);
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "INSERT INTO google_provider_identity_roots (workspace_id, user_id, provider, \
             identity_key_version, root_verifier, created_at, last_verified_at) \
             VALUES ($1, $2, 'google', $3, $4, $5, $5) \
             ON CONFLICT (workspace_id, user_id, provider) DO NOTHING",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(identity_key_version)
        .bind(root_verifier.as_slice())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let matches: bool = sqlx::query_scalar(
            "SELECT identity_key_version = $4 AND root_verifier = $5 \
             FROM google_provider_identity_roots WHERE workspace_id = $1 AND user_id = $2 \
               AND provider = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind("google")
        .bind(identity_key_version)
        .bind(root_verifier.as_slice())
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if !matches {
            return Err(GoogleSyncRepositoryError::IdentityRootMismatch);
        }
        sqlx::query(
            "UPDATE google_provider_identity_roots SET last_verified_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind("google")
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn replace_discovered(
        &self,
        account_id: Uuid,
        claim: Option<&SyncClaim>,
        kind: GoogleCollectionKind,
        collections: Vec<DiscoveredCollection>,
        now: DateTime<Utc>,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if kind == GoogleCollectionKind::Calendar {
            super::database::lock_execution_and_canonical_item_space(
                &mut transaction,
                self.scope.workspace_id,
            )
            .await
            .map_err(internal)?;
        }
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
        let previous_calendar_state: BTreeMap<String, (Uuid, bool)> =
            if kind == GoogleCollectionKind::Calendar {
                sqlx::query_as::<_, (String, Uuid, bool)>(
                    "SELECT remote_collection_id, id, provider_deleted \
                     FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
                       AND provider_account_id = $3 AND collection_kind = 'calendar' \
                     ORDER BY id FOR UPDATE",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(account_id)
                .fetch_all(&mut *transaction)
                .await
                .map_err(internal)?
                .into_iter()
                .map(|(remote_id, id, deleted)| (remote_id, (id, deleted)))
                .collect()
            } else {
                BTreeMap::new()
            };
        let mut teardown_collections = BTreeSet::new();
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
                    teardown_collections.insert(collection_id);
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
                         claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
                         run_claim_generation = NULL, dispatch_nonce = NULL, \
                         dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
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
            if collection.provider_deleted
                && let Some((collection_id, false)) =
                    previous_calendar_state.get(&collection.remote_id)
            {
                teardown_collections.insert(*collection_id);
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
        let newly_deleted: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE google_sync_collections SET provider_deleted = true, selected = false, \
             revision = revision + 1, updated_at = $6 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             AND collection_kind = $4 AND NOT provider_deleted \
             AND NOT (remote_collection_id = ANY($5)) RETURNING id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(kind.as_db())
        .bind(&seen)
        .bind(now)
        .fetch_all(&mut *transaction)
        .await
        .map_err(internal)?;
        if kind == GoogleCollectionKind::Calendar {
            teardown_collections.extend(newly_deleted);
            for collection_id in teardown_collections {
                retire_active_calendar_occurrences(
                    &mut transaction,
                    self.scope,
                    account_id,
                    collection_id,
                    now,
                )
                .await?;
            }
        }
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET state = 'conflict', claim_id = NULL, \
             claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
             dispatch_nonce = NULL, dispatch_authorized_at = NULL, \
             dispatch_expires_at = NULL, last_error_code = 'collection_deleted', updated_at = $5 \
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
        calendar_policy: GoogleCalendarPolicy,
        now: DateTime<Utc>,
    ) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        super::database::lock_execution_and_canonical_item_space(
            &mut transaction,
            self.scope.workspace_id,
        )
        .await
        .map_err(internal)?;
        let granted_scopes = self
            .ensure_account(&mut transaction, account_id, true)
            .await?;
        let current = sqlx::query(
            "SELECT collection_kind, provider_access_role, provider_deleted, selected, visible, \
             sync_role, confirmed_busy_policy, tentative_policy, free_policy, all_day_policy, \
             publish_all_day, publish_tentative, publish_free, revision \
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
        let projection_changed = current.try_get::<bool, _>("selected").map_err(internal)?
            != selected
            || current.try_get::<bool, _>("visible").map_err(internal)? != visible
            || current
                .try_get::<String, _>("sync_role")
                .map_err(internal)?
                != role.as_db()
            || current
                .try_get::<String, _>("confirmed_busy_policy")
                .map_err(internal)?
                != calendar_policy.confirmed_busy.as_db()
            || current
                .try_get::<String, _>("tentative_policy")
                .map_err(internal)?
                != calendar_policy.tentative.as_db()
            || current
                .try_get::<String, _>("free_policy")
                .map_err(internal)?
                != calendar_policy.free.as_db()
            || current
                .try_get::<String, _>("all_day_policy")
                .map_err(internal)?
                != calendar_policy.all_day.as_db()
            || current
                .try_get::<bool, _>("publish_all_day")
                .map_err(internal)?
                != calendar_policy.publish_all_day
            || current
                .try_get::<bool, _>("publish_tentative")
                .map_err(internal)?
                != calendar_policy.publish_tentative
            || current
                .try_get::<bool, _>("publish_free")
                .map_err(internal)?
                != calendar_policy.publish_free;
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
             confirmed_busy_policy = $8, tentative_policy = $9, free_policy = $10, \
             all_day_policy = $11, publish_all_day = $12, publish_tentative = $13, \
             publish_free = $14, configured_at = $15, updated_at = $15, revision = revision + 1 \
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
        .bind(calendar_policy.confirmed_busy.as_db())
        .bind(calendar_policy.tentative.as_db())
        .bind(calendar_policy.free.as_db())
        .bind(calendar_policy.all_day.as_db())
        .bind(calendar_policy.publish_all_day)
        .bind(calendar_policy.publish_tentative)
        .bind(calendar_policy.publish_free)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if collection_kind == "calendar" && projection_changed {
            retire_active_calendar_occurrences(
                &mut transaction,
                self.scope,
                account_id,
                collection_id,
                now,
            )
            .await?;
        }
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
        {
            sqlx::query(
                "UPDATE google_sync_outbox SET state = 'conflict', \
                 claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
                 run_claim_generation = NULL, dispatch_nonce = NULL, \
                 dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
                 last_error_code = $5, updated_at = $4 \
                 WHERE workspace_id = $1 AND user_id = $2 AND collection_id = $3 \
                   AND state IN ('pending', 'delivering', 'backoff')",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(collection_id)
            .bind(now)
            .bind(if !selected || role != GoogleSyncRole::Writable {
                "collection_not_writable"
            } else {
                "collection_configuration_changed"
            })
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        if selected {
            ensure_run_row(&mut transaction, self.scope, account_id, now).await?;
            sqlx::query(
                "UPDATE google_sync_runs SET requested_at = $4, next_attempt_at = \
                 LEAST(next_attempt_at, $4), refresh_generation = refresh_generation + 1, \
                 updated_at = $4, revision = revision + 1 \
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
            "UPDATE google_sync_outbox SET \
             state = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                                AND remote_resource_id IS NULL \
                                AND provider_post_may_have_started \
                           THEN 'conflict' ELSE 'backoff' END, \
             claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
             run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, available_at = $3, \
             last_error_code = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                                          AND remote_resource_id IS NULL \
                                          AND provider_post_may_have_started \
                                    THEN 'provider_identity_unresolved' \
                                    ELSE 'worker_restarted_before_send' END, updated_at = $3 \
             WHERE workspace_id = $1 AND user_id = $2 AND state = 'delivering'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
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

    async fn refresh_request(
        &self,
        account_id: Uuid,
        request_id: Uuid,
    ) -> Result<Option<GoogleSyncRefreshAccepted>, GoogleSyncRepositoryError> {
        let existing = sqlx::query(
            "SELECT refresh_generation, requested_at FROM google_sync_refresh_requests \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND request_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        existing
            .map(|row| {
                Ok(GoogleSyncRefreshAccepted {
                    account_id,
                    request_id,
                    refresh_generation: i64_to_u64(
                        row.try_get("refresh_generation").map_err(internal)?,
                    )?,
                    requested_at: row.try_get("requested_at").map_err(internal)?,
                })
            })
            .transpose()
    }

    async fn request_refresh(
        &self,
        account_id: Uuid,
        request_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<GoogleSyncRefreshAccepted, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        if let Some(existing) = sqlx::query(
            "SELECT refresh_generation, requested_at FROM google_sync_refresh_requests \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND request_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(request_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        {
            let accepted = GoogleSyncRefreshAccepted {
                account_id,
                request_id,
                refresh_generation: i64_to_u64(
                    existing.try_get("refresh_generation").map_err(internal)?,
                )?,
                requested_at: existing.try_get("requested_at").map_err(internal)?,
            };
            transaction.commit().await.map_err(internal)?;
            return Ok(accepted);
        }
        self.ensure_account(&mut transaction, account_id, true)
            .await?;
        ensure_run_row(&mut transaction, self.scope, account_id, now).await?;
        let row = sqlx::query(
            "UPDATE google_sync_runs SET requested_at = $4, \
             next_attempt_at = CASE WHEN state = 'running' THEN next_attempt_at \
                                    ELSE LEAST(next_attempt_at, $4) END, \
             started_at = CASE WHEN state = 'running' THEN started_at ELSE NULL END, \
             completed_at = CASE WHEN state = 'running' THEN completed_at ELSE NULL END, \
             refresh_generation = refresh_generation + 1, \
             state = CASE WHEN state IN ('failed', 'reauthorization_required') THEN 'idle' ELSE state END, \
             last_error_code = CASE WHEN state IN ('failed', 'reauthorization_required') THEN NULL ELSE last_error_code END, \
             revision = revision + 1, updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             RETURNING refresh_generation",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let refresh_generation = i64_to_u64(row.try_get("refresh_generation").map_err(internal)?)?;
        sqlx::query(
            "INSERT INTO google_sync_refresh_requests \
             (workspace_id, user_id, provider_account_id, request_id, refresh_generation, \
              requested_at, created_at) VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(request_id)
        .bind(u64_to_i64(refresh_generation)?)
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
        Ok(GoogleSyncRefreshAccepted {
            account_id,
            request_id,
            refresh_generation,
            requested_at: now,
        })
    }

    async fn begin_calendar_projection_refresh(
        &self,
        claim: &SyncClaim,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        super::database::lock_execution_and_canonical_item_space(
            &mut transaction,
            self.scope.workspace_id,
        )
        .await
        .map_err(internal)?;
        ensure_run_claim(&mut transaction, self.scope, claim, now).await?;
        sqlx::query(
            "UPDATE google_sync_collections SET planning_projection_state = 'uninitialized', \
             planning_collection_revision = NULL, planning_window_start = NULL, \
             planning_window_end = NULL, planning_window_refreshed_at = NULL, \
             planning_last_error_code = NULL, updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND collection_kind = 'calendar' AND selected AND NOT provider_deleted",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
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
        // Reconcile every child delivery while the expired parent identity is
        // still visible. This makes takeover itself the fence: an old worker
        // cannot authorize or complete even if the outbox lease was newer.
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET \
             state = CASE WHEN outbox.entity_kind = 'task' AND outbox.operation = 'upsert' \
                                AND outbox.remote_resource_id IS NULL \
                                AND outbox.provider_post_may_have_started \
                           THEN 'conflict' ELSE 'backoff' END, \
             claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
             run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, available_at = $3, \
             last_error_code = CASE WHEN outbox.entity_kind = 'task' \
                                          AND outbox.operation = 'upsert' \
                                          AND outbox.remote_resource_id IS NULL \
                                          AND outbox.provider_post_may_have_started \
                                    THEN 'provider_identity_unresolved' \
                                    ELSE 'parent_run_lease_expired_before_send' END, updated_at = $3 \
             FROM google_sync_runs run WHERE run.workspace_id = $1 AND run.user_id = $2 \
               AND run.state = 'running' AND run.lease_until <= $3 \
               AND outbox.workspace_id = run.workspace_id AND outbox.user_id = run.user_id \
               AND outbox.provider_account_id = run.provider_account_id \
               AND outbox.state = 'delivering' AND outbox.run_claim_id = run.claim_id \
               AND outbox.run_claim_generation = run.claim_generation",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
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
               claim_generation = claim_generation + 1, \
               claimed_refresh_generation = refresh_generation, started_at = $3, \
               completed_at = NULL, updated_at = $3, \
               revision = revision + 1 \
             FROM candidate WHERE run.workspace_id = $1 \
               AND run.provider_account_id = candidate.provider_account_id \
             RETURNING run.provider_account_id, run.claim_generation",
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
                    claim_generation: i64_to_u64(
                        row.try_get("claim_generation").map_err(internal)?,
                    )?,
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
             AND state = 'running' AND claim_id = $6 AND claim_generation = $7 \
             AND lease_until > $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .bind(lease_until)
        .bind(claim.claim_id)
        .bind(u64_to_i64(claim.claim_generation)?)
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
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_run_claim(&mut transaction, self.scope, claim, now).await?;
        reconcile_outbound_for_parent_end(
            &mut transaction,
            self.scope,
            claim,
            "parent_run_completed_with_active_delivery",
            now,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE google_sync_runs SET state = 'idle', claim_id = NULL, lease_until = NULL, \
             completed_at = $5, completed_refresh_generation = claimed_refresh_generation, \
             next_attempt_at = CASE WHEN refresh_generation > claimed_refresh_generation \
                                    THEN $5 ELSE $6 END, \
             requested_at = CASE WHEN refresh_generation > claimed_refresh_generation \
                                 THEN requested_at ELSE NULL END, \
             consecutive_failures = 0, last_error_code = NULL, last_error_at = NULL, \
             imported_count = $7, updated_count = $8, deleted_count = $9, conflict_count = $10, \
             rejected_count = $11, revision = revision + 1, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND state = 'running' AND claim_id = $4 AND claim_generation = $12 \
               AND lease_until > $5",
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
        .bind(u64_to_i64(claim.claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            transaction.commit().await.map_err(internal)?;
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
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_run_claim(&mut transaction, self.scope, claim, now).await?;
        reconcile_outbound_for_parent_end(
            &mut transaction,
            self.scope,
            claim,
            "parent_run_failed_with_active_delivery",
            now,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE google_sync_runs SET state = $5, claim_id = NULL, lease_until = NULL, \
             next_attempt_at = $6, consecutive_failures = consecutive_failures + 1, \
             last_error_code = $7, last_error_at = $8, revision = revision + 1, updated_at = $8 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND state = 'running' AND claim_id = $4 AND claim_generation = $9 \
               AND lease_until > $8",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(claim.claim_id)
        .bind(state)
        .bind(next_attempt_at)
        .bind(code)
        .bind(now)
        .bind(u64_to_i64(claim.claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            transaction.commit().await.map_err(internal)?;
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
             updated_count, deleted_count, conflict_count, rejected_count, refresh_generation, \
             claimed_refresh_generation, completed_refresh_generation, revision \
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
        super::database::lock_execution_and_canonical_item_space(
            &mut transaction,
            self.scope.workspace_id,
        )
        .await
        .map_err(internal)?;
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
            "SELECT id, local_entity_id, remote_etag, remote_payload_hash, remote_projection_hash, \
             local_revision, sync_state, ownership \
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
            apply_remote_delete(
                &mut transaction,
                self.scope,
                claim,
                &change,
                mapping.as_ref(),
                now,
            )
            .await?
        } else {
            apply_remote_upsert(
                &mut transaction,
                self.scope,
                claim,
                change,
                mapping.as_ref(),
                now,
            )
            .await?
        };
        transaction.commit().await.map_err(internal)?;
        Ok(outcome)
    }

    async fn replace_calendar_projection(
        &self,
        claim: &SyncClaim,
        batch: CalendarProjectionBatch,
        now: DateTime<Utc>,
    ) -> Result<CalendarProjectionResult, GoogleSyncRepositoryError> {
        validate_calendar_projection_batch(claim, &batch)?;
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        super::database::lock_execution_and_canonical_item_space(
            &mut transaction,
            self.scope.workspace_id,
        )
        .await
        .map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            batch.collection_id,
            batch.collection_revision,
            now,
        )
        .await?;
        let generation =
            lock_calendar_projection_collection(&mut transaction, self.scope, &batch).await?;
        if !batch.rejected.is_empty() {
            record_projection_rejections(&mut transaction, self.scope, &batch, generation, now)
                .await?;
            transaction.commit().await.map_err(internal)?;
            return Ok(CalendarProjectionResult {
                generation,
                complete: false,
                counts: SyncCounts {
                    rejected: batch.rejected.len() as u64,
                    ..SyncCounts::default()
                },
            });
        }
        let next_generation = generation
            .checked_add(1)
            .ok_or(GoogleSyncRepositoryError::Internal)?;
        sqlx::query("SAVEPOINT replace_calendar_projection")
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        let replacement = replace_calendar_projection_tx(
            &mut transaction,
            self.scope,
            &batch,
            next_generation,
            now,
        )
        .await;
        let counts = match replacement {
            Ok(counts) => match seal_calendar_projection(
                &mut transaction,
                self.scope,
                &batch,
                generation,
                next_generation,
                now,
            )
            .await
            {
                Ok(()) => counts,
                Err(error) => {
                    return finish_projection_semantic_failure(
                        transaction,
                        self.scope,
                        &batch,
                        generation,
                        error,
                        now,
                    )
                    .await;
                }
            },
            Err(error) => {
                return finish_projection_semantic_failure(
                    transaction,
                    self.scope,
                    &batch,
                    generation,
                    error,
                    now,
                )
                .await;
            }
        };
        sqlx::query("RELEASE SAVEPOINT replace_calendar_projection")
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(CalendarProjectionResult {
            generation: next_generation,
            complete: true,
            counts,
        })
    }

    async fn apply_calendar_series_metadata(
        &self,
        claim: &SyncClaim,
        change: RemoteCalendarSeriesChange,
        now: DateTime<Utc>,
    ) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
        validate_calendar_series_change(claim, &change)?;
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        super::database::lock_execution_and_canonical_item_space(
            &mut transaction,
            self.scope.workspace_id,
        )
        .await
        .map_err(internal)?;
        ensure_inbound_claim(
            &mut transaction,
            self.scope,
            claim,
            change.collection_id,
            change.collection_revision,
            now,
        )
        .await?;
        let outcome =
            apply_calendar_series_metadata_tx(&mut transaction, self.scope, claim, &change, now)
                .await?;
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
        super::database::lock_execution_and_canonical_item_space(
            &mut transaction,
            self.scope.workspace_id,
        )
        .await
        .map_err(internal)?;
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
                reviewed_provider_projection: None,
                item: None,
            };
            counts.add(
                apply_remote_delete(
                    &mut transaction,
                    self.scope,
                    claim,
                    &change,
                    Some(mapping),
                    now,
                )
                .await?,
            );
        }
        transaction.commit().await.map_err(internal)?;
        Ok(counts)
    }

    async fn create_outbound_preview(
        &self,
        spec: OutboundPreviewSpec,
        now: DateTime<Utc>,
    ) -> Result<GoogleOutboundPreview, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let granted_scopes = self
            .ensure_account(&mut transaction, spec.account_id, true)
            .await?;
        let collection = sqlx::query(
            "SELECT collection_kind, remote_collection_id, revision, selected, provider_deleted, sync_role \
             FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
             AND provider_account_id = $3 AND id = $4 FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.account_id)
        .bind(spec.collection_id)
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
        let required_scope = match (spec.prepared.entity_kind, collection_kind.as_str()) {
            ("calendar_event", "calendar") => GOOGLE_CALENDAR_SCOPE,
            ("task", "task_list") => GOOGLE_TASKS_SCOPE,
            _ => return Err(GoogleSyncRepositoryError::CollectionNotWritable),
        };
        if required_scope != spec.required_scope
            || !granted_scopes.iter().any(|scope| scope == required_scope)
            || collection
                .try_get::<String, _>("remote_collection_id")
                .map_err(internal)?
                != spec.collection_remote_id
            || i64_to_u64(collection.try_get("revision").map_err(internal)?)?
                != spec.collection_revision
        {
            return Err(GoogleSyncRepositoryError::WriteScopeMissing);
        }
        let stored_revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM items WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(spec.prepared.item.id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let Some(stored_revision) = stored_revision else {
            return Err(GoogleSyncRepositoryError::ItemNotFound);
        };
        let stored_revision = i64_to_u64(stored_revision)?;
        if stored_revision != spec.prepared.item.revision {
            return Err(GoogleSyncRepositoryError::RevisionConflict {
                expected: spec.prepared.item.revision,
                actual: stored_revision,
            });
        }
        let imported_external: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND entity_kind = 'item' AND local_entity_id = $2 AND ownership = 'external' \
             AND tombstoned_at IS NULL)",
        )
        .bind(self.scope.workspace_id)
        .bind(spec.prepared.item.id)
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
        .bind(spec.account_id)
        .bind(spec.collection_id)
        .bind(spec.prepared.item.id)
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
        if remote_resource_id
            .as_deref()
            .is_some_and(|remote_id| remote_id.trim().is_empty())
            || (remote_resource_id.is_some()
                && expected_etag
                    .as_deref()
                    .is_none_or(|etag| etag.trim().is_empty()))
        {
            return Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable);
        }
        if spec.prepared.operation == OutboundOperation::Delete && remote_resource_id.is_none() {
            return Err(GoogleSyncRepositoryError::ExternalMutationForbidden);
        }
        if spec.prepared.entity_kind == "task"
            && spec.prepared.operation == OutboundOperation::Upsert
            && remote_resource_id.is_none()
        {
            let unsafe_prior_create: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM google_sync_outbox WHERE workspace_id = $1 \
                 AND user_id = $2 AND provider_account_id = $3 AND collection_id = $4 \
                 AND item_id = $5 AND entity_kind = 'task' AND operation = 'upsert' \
                 AND (last_error_code = 'provider_identity_unresolved' \
                   OR (remote_resource_id IS NULL AND provider_post_may_have_started)))",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(spec.account_id)
            .bind(spec.collection_id)
            .bind(spec.prepared.item.id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(internal)?;
            if unsafe_prior_create {
                return Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable);
            }
        }
        let intent_hash = outbound_intent_hash(
            self.scope.workspace_id,
            self.scope.user_id,
            spec.account_id,
            spec.collection_id,
            spec.collection_revision,
            &spec.collection_remote_id,
            parse_collection_kind(&collection_kind)?,
            spec.required_scope,
            spec.prepared.item.id,
            spec.prepared.item.revision,
            spec.prepared.entity_kind,
            spec.prepared.operation,
            &spec.prepared.payload,
            remote_resource_id.as_deref(),
            expected_etag.as_deref(),
        )
        .map_err(internal)?;
        let preview_hash = outbound_preview_hash(spec.id, intent_hash, spec.expires_at);
        sqlx::query(
            "INSERT INTO google_outbound_previews (id, workspace_id, user_id, provider_account_id, \
             collection_id, collection_revision, collection_remote_id, item_id, item_revision, \
             entity_kind, operation, required_scope, provider_resource_id, expected_etag, intent_hash, \
             preview_hash, payload, expires_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, \
             $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $19)",
        )
        .bind(spec.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.account_id)
        .bind(spec.collection_id)
        .bind(u64_to_i64(spec.collection_revision)?)
        .bind(&spec.collection_remote_id)
        .bind(spec.prepared.item.id)
        .bind(u64_to_i64(spec.prepared.item.revision)?)
        .bind(spec.prepared.entity_kind)
        .bind(spec.prepared.operation.as_db())
        .bind(spec.required_scope)
        .bind(&remote_resource_id)
        .bind(&expected_etag)
        .bind(intent_hash.as_slice())
        .bind(preview_hash.as_slice())
        .bind(&spec.prepared.payload)
        .bind(spec.expires_at)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let result = GoogleOutboundPreview {
            id: spec.id,
            account_id: spec.account_id,
            collection_id: spec.collection_id,
            collection_revision: spec.collection_revision,
            collection_display_name: spec.collection_display_name,
            item_id: spec.prepared.item.id,
            item_revision: spec.prepared.item.revision,
            entity_kind: spec.prepared.entity_kind.to_owned(),
            operation: spec.prepared.operation,
            provider_resource_id: remote_resource_id,
            provider_etag: expected_etag,
            preview_hash: encode_hex_bytes(&preview_hash),
            provider_payload: review_payload(&spec.prepared.payload),
            expires_at: spec.expires_at,
        };
        transaction.commit().await.map_err(internal)?;
        Ok(result)
    }

    async fn approve_outbound(
        &self,
        spec: OutboundApprovalSpec,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let row = sqlx::query(
            "SELECT provider_account_id, collection_id, collection_revision, collection_remote_id, \
             item_id, item_revision, entity_kind, operation, required_scope, intent_hash, \
             preview_hash, expires_at, approved_at FROM google_outbound_previews \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.preview_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleSyncRepositoryError::ApprovalInvalid)?;
        if row
            .try_get::<Uuid, _>("provider_account_id")
            .map_err(internal)?
            != spec.account_id
            || row
                .try_get::<Vec<u8>, _>("preview_hash")
                .map_err(internal)?
                != spec.expected_preview_hash
        {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(internal)?;
        if expires_at <= now {
            return Err(GoogleSyncRepositoryError::ApprovalExpired);
        }
        if row
            .try_get::<Option<DateTime<Utc>>, _>("approved_at")
            .map_err(internal)?
            .is_some()
        {
            return Err(GoogleSyncRepositoryError::ApprovalAlreadyIssued);
        }
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_accounts account \
             JOIN google_sync_collections collection ON collection.workspace_id = account.workspace_id \
               AND collection.user_id = account.user_id AND collection.provider_account_id = account.id \
             JOIN items item ON item.workspace_id = account.workspace_id \
             WHERE account.workspace_id = $1 AND account.user_id = $2 AND account.id = $3 \
               AND account.provider = 'google' AND account.status = 'active' AND account.sync_enabled \
               AND account.tombstoned_at IS NULL AND $4 = ANY(account.granted_scopes) \
               AND collection.id = $5 AND collection.revision = $6 \
               AND collection.remote_collection_id = $7 AND collection.selected \
               AND NOT collection.provider_deleted AND collection.sync_role = 'writable' \
               AND item.id = $8 AND item.revision = $9)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.account_id)
        .bind(row.try_get::<String, _>("required_scope").map_err(internal)?)
        .bind(row.try_get::<Uuid, _>("collection_id").map_err(internal)?)
        .bind(row.try_get::<i64, _>("collection_revision").map_err(internal)?)
        .bind(row.try_get::<String, _>("collection_remote_id").map_err(internal)?)
        .bind(row.try_get::<Uuid, _>("item_id").map_err(internal)?)
        .bind(row.try_get::<i64, _>("item_revision").map_err(internal)?)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if !valid {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        let audit_id = Uuid::new_v4();
        let item_id: Uuid = row.try_get("item_id").map_err(internal)?;
        let item_revision: i64 = row.try_get("item_revision").map_err(internal)?;
        let collection_id: Uuid = row.try_get("collection_id").map_err(internal)?;
        let operation: String = row.try_get("operation").map_err(internal)?;
        sqlx::query(
            "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
             entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
             VALUES ($1, $2, $3, 'google.sync.outbound_approved', 'item', $4, $5, $5, \
             'succeeded', $6, $7)",
        )
        .bind(audit_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(item_id)
        .bind(item_revision)
        .bind(json!({
            "preview_id": spec.preview_id,
            "account_id": spec.account_id,
            "collection_id": collection_id,
            "operation": operation,
            "expires_at": expires_at,
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_outbound_previews SET approved_at = $4, capability_hash = $5, \
             approval_audit_id = $6, updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 \
             AND id = $3 AND approved_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.preview_id)
        .bind(now)
        .bind(spec.capability_hash.as_slice())
        .bind(audit_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(expires_at)
    }

    async fn enqueue_outbound(
        &self,
        spec: OutboundEnqueueSpec,
        now: DateTime<Utc>,
    ) -> Result<GoogleOutboundAccepted, GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let approval = sqlx::query(
            "SELECT id, provider_account_id, collection_id, collection_revision, \
             collection_remote_id, item_id, item_revision, entity_kind, operation, required_scope, \
             provider_resource_id, expected_etag, intent_hash, payload, expires_at, approved_at, \
             consumed_at, outbox_id, approval_audit_id \
             FROM google_outbound_previews WHERE workspace_id = $1 AND user_id = $2 \
             AND capability_hash = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.capability_hash.as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleSyncRepositoryError::ApprovalInvalid)?;
        let account_id: Uuid = approval.try_get("provider_account_id").map_err(internal)?;
        let collection_id: Uuid = approval.try_get("collection_id").map_err(internal)?;
        let item_id: Uuid = approval.try_get("item_id").map_err(internal)?;
        let item_revision = i64_to_u64(approval.try_get("item_revision").map_err(internal)?)?;
        let operation = parse_outbound_operation(
            &approval
                .try_get::<String, _>("operation")
                .map_err(internal)?,
        )?;
        let request_matches = account_id == spec.account_id
            && collection_id == spec.request.collection_id
            && item_id == spec.request.item_id
            && item_revision == spec.request.expected_item_revision
            && operation == spec.request.operation;
        if !request_matches {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        // Consumption permanently removes enqueue authority. An exact retry is
        // only a receipt lookup, so it remains recoverable after capability expiry.
        if approval
            .try_get::<Option<DateTime<Utc>>, _>("consumed_at")
            .map_err(internal)?
            .is_some()
        {
            let outbox_id = approval
                .try_get::<Option<Uuid>, _>("outbox_id")
                .map_err(internal)?
                .ok_or(GoogleSyncRepositoryError::ApprovalInvalid)?;
            transaction.commit().await.map_err(internal)?;
            return Ok(GoogleOutboundAccepted {
                outbox_id,
                replayed: true,
            });
        }
        let expires_at: DateTime<Utc> = approval.try_get("expires_at").map_err(internal)?;
        if expires_at <= now {
            return Err(GoogleSyncRepositoryError::ApprovalExpired);
        }
        if approval
            .try_get::<Option<DateTime<Utc>>, _>("approved_at")
            .map_err(internal)?
            .is_none()
        {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        let required_scope: String = approval.try_get("required_scope").map_err(internal)?;
        let collection_revision: i64 = approval.try_get("collection_revision").map_err(internal)?;
        let collection_remote_id: String =
            approval.try_get("collection_remote_id").map_err(internal)?;
        let entity_kind: String = approval.try_get("entity_kind").map_err(internal)?;
        let (collection_kind, expected_required_scope) = match entity_kind.as_str() {
            "calendar_event" => (GoogleCollectionKind::Calendar, GOOGLE_CALENDAR_SCOPE),
            "task" => (GoogleCollectionKind::TaskList, GOOGLE_TASKS_SCOPE),
            _ => return Err(GoogleSyncRepositoryError::ApprovalInvalid),
        };
        if required_scope != expected_required_scope {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        let approved_remote_resource_id: Option<String> =
            approval.try_get("provider_resource_id").map_err(internal)?;
        let approved_etag: Option<String> = approval.try_get("expected_etag").map_err(internal)?;
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_accounts account \
             JOIN google_sync_collections collection ON collection.workspace_id = account.workspace_id \
               AND collection.user_id = account.user_id AND collection.provider_account_id = account.id \
             JOIN items item ON item.workspace_id = account.workspace_id \
             WHERE account.workspace_id = $1 AND account.user_id = $2 AND account.id = $3 \
               AND account.provider = 'google' AND account.status = 'active' AND account.sync_enabled \
               AND account.tombstoned_at IS NULL AND $4 = ANY(account.granted_scopes) \
               AND collection.id = $5 AND collection.revision = $6 \
               AND collection.remote_collection_id = $7 AND collection.selected \
               AND NOT collection.provider_deleted AND collection.sync_role = 'writable' \
               AND ((collection.collection_kind = 'calendar' AND $8 = 'calendar_event') \
                 OR (collection.collection_kind = 'task_list' AND $8 = 'task')) \
               AND item.id = $9 AND item.revision = $10)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(&required_scope)
        .bind(collection_id)
        .bind(collection_revision)
        .bind(&collection_remote_id)
        .bind(&entity_kind)
        .bind(item_id)
        .bind(u64_to_i64(item_revision)?)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if !valid {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        let mapping = sqlx::query(
            "SELECT remote_resource_id, remote_etag, ownership FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_id = $3 \
               AND entity_kind = 'item' AND local_entity_id = $4 AND tombstoned_at IS NULL FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(item_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        if mapping.as_ref().is_some_and(|row| {
            row.try_get::<String, _>("ownership").ok().as_deref() != Some("dayweave")
        }) {
            return Err(GoogleSyncRepositoryError::ExternalMutationForbidden);
        }
        let current_remote_resource_id: Option<String> = mapping
            .as_ref()
            .map(|row| row.try_get("remote_resource_id").map_err(internal))
            .transpose()?;
        let current_etag: Option<String> = mapping
            .as_ref()
            .map(|row| row.try_get("remote_etag").map_err(internal))
            .transpose()?;
        if current_remote_resource_id
            .as_deref()
            .is_some_and(|remote_id| remote_id.trim().is_empty())
            || (current_remote_resource_id.is_some()
                && current_etag
                    .as_deref()
                    .is_none_or(|etag| etag.trim().is_empty()))
        {
            return Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable);
        }
        if current_remote_resource_id != approved_remote_resource_id
            || current_etag != approved_etag
        {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        if operation == OutboundOperation::Delete && approved_remote_resource_id.is_none() {
            return Err(GoogleSyncRepositoryError::ExternalMutationForbidden);
        }
        if entity_kind == "task"
            && operation == OutboundOperation::Upsert
            && approved_remote_resource_id.is_none()
        {
            // Lock every prior create before deciding whether it is safe to
            // supersede. This serializes against the worker's claim transition:
            // a pending unsent revision may be replaced, but an in-flight or
            // previously attempted markerless create must fail closed.
            let prior_creates = sqlx::query(
                "SELECT last_error_code, remote_resource_id, provider_post_may_have_started \
                 FROM google_sync_outbox WHERE workspace_id = $1 AND user_id = $2 \
                 AND provider_account_id = $3 AND collection_id = $4 AND item_id = $5 \
                 AND entity_kind = 'task' AND operation = 'upsert' FOR UPDATE",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(account_id)
            .bind(collection_id)
            .bind(item_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(internal)?;
            let unsafe_prior_create = prior_creates.iter().any(|row| {
                row.try_get::<Option<String>, _>("last_error_code")
                    .ok()
                    .flatten()
                    .as_deref()
                    == Some("provider_identity_unresolved")
                    || (row
                        .try_get::<Option<String>, _>("remote_resource_id")
                        .ok()
                        .flatten()
                        .is_none()
                        && row
                            .try_get::<bool, _>("provider_post_may_have_started")
                            .ok()
                            == Some(true))
            });
            if unsafe_prior_create {
                return Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable);
            }
        }
        let payload: Value = approval.try_get("payload").map_err(internal)?;
        let intent_hash: Vec<u8> = approval.try_get("intent_hash").map_err(internal)?;
        let recomputed_intent_hash = outbound_intent_hash(
            self.scope.workspace_id,
            self.scope.user_id,
            account_id,
            collection_id,
            i64_to_u64(collection_revision)?,
            &collection_remote_id,
            collection_kind,
            &required_scope,
            item_id,
            item_revision,
            &entity_kind,
            operation,
            &payload,
            approved_remote_resource_id.as_deref(),
            approved_etag.as_deref(),
        )
        .map_err(internal)?;
        if intent_hash.as_slice() != recomputed_intent_hash {
            return Err(GoogleSyncRepositoryError::ApprovalInvalid);
        }
        let superseded = sqlx::query(
            "UPDATE google_sync_outbox SET state = 'superseded', claim_id = NULL, claimed_at = NULL, \
             run_claim_id = NULL, run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
             last_error_code = 'superseded_by_newer_revision', updated_at = $7 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND collection_id = $4 AND item_id = $5 AND item_revision < $6 \
               AND state IN ('pending', 'delivering', 'backoff', 'conflict', 'failed')",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(item_id)
        .bind(u64_to_i64(item_revision)?)
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
            .bind(item_id)
            .bind(u64_to_i64(item_revision)?)
            .bind(json!({"collection_id": collection_id, "superseded_count": superseded}))
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        let outbox_id = Uuid::new_v4();
        let approval_id: Uuid = approval.try_get("id").map_err(internal)?;
        let approval_audit_id: Uuid = approval
            .try_get::<Option<Uuid>, _>("approval_audit_id")
            .map_err(internal)?
            .ok_or(GoogleSyncRepositoryError::ApprovalInvalid)?;
        let inserted = sqlx::query(
            "INSERT INTO google_sync_outbox (id, workspace_id, user_id, provider_account_id, \
             collection_id, item_id, item_revision, entity_kind, operation, remote_resource_id, \
             expected_etag, app_owned, approval_audit_id, approval_id, intent_hash, \
             collection_revision, target_remote_collection_id, required_scope, payload, \
             available_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, true, $12, $13, $14, \
             $15, $16, $17, $18, $19, $19, $19) \
             ON CONFLICT (workspace_id, collection_id, item_id, item_revision, operation) DO NOTHING",
        )
        .bind(outbox_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(item_id)
        .bind(u64_to_i64(item_revision)?)
        .bind(&entity_kind)
        .bind(operation.as_db())
        .bind(approved_remote_resource_id)
        .bind(approved_etag)
        .bind(approval_audit_id)
        .bind(approval_id)
        .bind(&intent_hash)
        .bind(collection_revision)
        .bind(&collection_remote_id)
        .bind(&required_scope)
        .bind(payload)
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
            let existing = sqlx::query(
                "SELECT id, intent_hash FROM google_sync_outbox WHERE workspace_id = $1 \
                 AND collection_id = $2 AND item_id = $3 AND item_revision = $4 AND operation = $5",
            )
            .bind(self.scope.workspace_id)
            .bind(collection_id)
            .bind(item_id)
            .bind(u64_to_i64(item_revision)?)
            .bind(operation.as_db())
            .fetch_one(&mut *transaction)
            .await
            .map_err(internal)?;
            if existing
                .try_get::<Option<Vec<u8>>, _>("intent_hash")
                .map_err(internal)?
                .as_deref()
                != Some(intent_hash.as_slice())
            {
                return Err(GoogleSyncRepositoryError::ApprovalInvalid);
            }
            GoogleOutboundAccepted {
                outbox_id: existing.try_get("id").map_err(internal)?,
                replayed: true,
            }
        };
        sqlx::query(
            "UPDATE google_outbound_previews SET consumed_at = $4, outbox_id = $5, updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND consumed_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(approval_id)
        .bind(now)
        .bind(result.outbox_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        ensure_run_row(&mut transaction, self.scope, account_id, now).await?;
        sqlx::query(
            "UPDATE google_sync_runs SET requested_at = $4, next_attempt_at = LEAST(next_attempt_at, $4), \
             refresh_generation = refresh_generation + 1, revision = revision + 1, \
             updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 \
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
        // A child lease is valid only under the exact current parent run. This
        // also repairs rows left by a crashed worker after a run takeover.
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET \
             state = CASE WHEN outbox.entity_kind = 'task' AND outbox.operation = 'upsert' \
                                AND outbox.remote_resource_id IS NULL \
                                AND outbox.provider_post_may_have_started \
                           THEN 'conflict' ELSE 'backoff' END, \
             claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
             run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, available_at = $4, \
             last_error_code = CASE WHEN outbox.entity_kind = 'task' \
                                          AND outbox.operation = 'upsert' \
                                          AND outbox.remote_resource_id IS NULL \
                                          AND outbox.provider_post_may_have_started \
                                    THEN 'provider_identity_unresolved' \
                                    ELSE 'parent_run_claim_changed_before_send' END, updated_at = $4 \
             WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
               AND outbox.provider_account_id = $3 AND outbox.state = 'delivering' \
               AND (outbox.run_claim_id <> $5 OR outbox.run_claim_generation <> $6)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .bind(claim.claim_id)
        .bind(u64_to_i64(claim.claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET \
             state = CASE WHEN outbox.entity_kind = 'task' AND outbox.operation = 'upsert' \
                                AND outbox.remote_resource_id IS NULL \
                                AND outbox.provider_post_may_have_started \
                           THEN 'conflict' ELSE 'superseded' END, \
             claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
             run_claim_generation = NULL, dispatch_nonce = NULL, dispatch_authorized_at = NULL, \
             dispatch_expires_at = NULL, \
             last_error_code = CASE WHEN outbox.entity_kind = 'task' \
                                          AND outbox.operation = 'upsert' \
                                          AND outbox.remote_resource_id IS NULL \
                                          AND outbox.provider_post_may_have_started \
                                    THEN 'provider_identity_unresolved' \
                                    ELSE 'superseded_by_canonical_revision' END, \
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
            "UPDATE google_sync_outbox SET \
             state = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                                AND remote_resource_id IS NULL \
                                AND provider_post_may_have_started \
                           THEN 'conflict' ELSE 'backoff' END, \
             claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
             run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, available_at = $4, \
             last_error_code = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                                          AND remote_resource_id IS NULL \
                                          AND provider_post_may_have_started \
                                    THEN 'provider_identity_unresolved' \
                                    ELSE 'delivery_lease_expired_before_send' END, \
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
        // A reviewed create is valid only while no identity exists; edits and
        // deletes require the exact DayWeave-owned mapping and reviewed ETag.
        sqlx::query(
            "UPDATE google_sync_outbox outbox SET state = 'conflict', claim_id = NULL, \
             claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
             dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
             last_error_code = 'provider_mapping_changed_before_claim', updated_at = $4 \
             WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
               AND outbox.provider_account_id = $3 AND outbox.state IN ('pending', 'backoff') \
               AND NOT (((outbox.remote_resource_id IS NULL AND outbox.expected_etag IS NULL \
                          AND outbox.operation = 'upsert') \
                         AND NOT EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                           WHERE mapping.workspace_id = outbox.workspace_id \
                             AND mapping.provider_account_id = outbox.provider_account_id \
                             AND mapping.collection_id = outbox.collection_id \
                             AND mapping.entity_kind = 'item' AND mapping.tombstoned_at IS NULL \
                             AND (mapping.local_entity_id = outbox.item_id \
                               OR (outbox.entity_kind = 'calendar_event' \
                                 AND mapping.remote_resource_id = outbox.payload->>'id')))) \
                      OR (outbox.remote_resource_id IS NOT NULL AND outbox.expected_etag IS NOT NULL \
                         AND EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                           WHERE mapping.workspace_id = outbox.workspace_id \
                             AND mapping.provider_account_id = outbox.provider_account_id \
                             AND mapping.collection_id = outbox.collection_id \
                             AND mapping.entity_kind = 'item' AND mapping.local_entity_id = outbox.item_id \
                             AND mapping.remote_resource_id = outbox.remote_resource_id \
                             AND mapping.remote_etag = outbox.expected_etag \
                             AND mapping.ownership = 'dayweave' \
                             AND mapping.tombstoned_at IS NULL)))",
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
               JOIN google_sync_runs run ON run.workspace_id = outbox.workspace_id \
                 AND run.user_id = outbox.user_id \
                 AND run.provider_account_id = outbox.provider_account_id \
               JOIN google_sync_collections collection ON collection.workspace_id = outbox.workspace_id \
                 AND collection.id = outbox.collection_id \
               JOIN items item ON item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
               JOIN google_outbound_previews approval ON approval.workspace_id = outbox.workspace_id \
                 AND approval.id = outbox.approval_id \
               WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
                 AND outbox.provider_account_id = $3 AND outbox.state IN ('pending', 'backoff') \
                 AND collection.user_id = $2 AND collection.provider_account_id = $3 \
                 AND collection.selected AND NOT collection.provider_deleted \
                 AND collection.sync_role = 'writable' \
                 AND collection.revision = outbox.collection_revision \
                 AND collection.remote_collection_id = outbox.target_remote_collection_id \
                 AND item.revision = outbox.item_revision \
                 AND approval.approved_at IS NOT NULL AND approval.consumed_at IS NOT NULL \
                 AND approval.outbox_id = outbox.id AND approval.intent_hash = outbox.intent_hash \
                 AND approval.provider_account_id = outbox.provider_account_id \
                 AND approval.collection_id = outbox.collection_id \
                 AND approval.collection_revision = outbox.collection_revision \
                 AND approval.collection_remote_id = outbox.target_remote_collection_id \
                 AND approval.item_id = outbox.item_id \
                 AND approval.item_revision = outbox.item_revision \
                 AND approval.entity_kind = outbox.entity_kind \
                 AND approval.operation = outbox.operation \
                 AND approval.required_scope = outbox.required_scope \
                 AND approval.payload = outbox.payload \
                 AND approval.provider_resource_id IS NOT DISTINCT FROM outbox.remote_resource_id \
                 AND approval.expected_etag IS NOT DISTINCT FROM outbox.expected_etag \
                 AND run.state = 'running' AND run.claim_id = $6 \
                 AND run.claim_generation = $7 AND run.lease_until > $4 \
                 AND (((outbox.remote_resource_id IS NULL AND outbox.expected_etag IS NULL \
                        AND outbox.operation = 'upsert') \
                       AND NOT EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                         WHERE mapping.workspace_id = outbox.workspace_id \
                           AND mapping.provider_account_id = outbox.provider_account_id \
                           AND mapping.collection_id = outbox.collection_id \
                           AND mapping.entity_kind = 'item' AND mapping.tombstoned_at IS NULL \
                           AND (mapping.local_entity_id = outbox.item_id \
                             OR (outbox.entity_kind = 'calendar_event' \
                               AND mapping.remote_resource_id = outbox.payload->>'id')))) \
                    OR (outbox.remote_resource_id IS NOT NULL AND outbox.expected_etag IS NOT NULL \
                       AND EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                         WHERE mapping.workspace_id = outbox.workspace_id \
                           AND mapping.provider_account_id = outbox.provider_account_id \
                           AND mapping.collection_id = outbox.collection_id \
                           AND mapping.entity_kind = 'item' AND mapping.local_entity_id = outbox.item_id \
                           AND mapping.remote_resource_id = outbox.remote_resource_id \
                           AND mapping.remote_etag = outbox.expected_etag \
                           AND mapping.ownership = 'dayweave' \
                           AND mapping.tombstoned_at IS NULL))) \
                 AND outbox.available_at <= $4 ORDER BY outbox.available_at, outbox.created_at, outbox.id \
               FOR UPDATE OF outbox, run, collection SKIP LOCKED LIMIT 1) \
             UPDATE google_sync_outbox outbox SET state = 'delivering', claim_id = $5, claimed_at = $4, \
               run_claim_id = $6, run_claim_generation = $7, \
               dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
               updated_at = $4 FROM candidate WHERE outbox.id = candidate.id \
             RETURNING outbox.id, outbox.provider_account_id, outbox.collection_id, outbox.item_id, \
               outbox.item_revision, outbox.entity_kind, outbox.operation, outbox.remote_resource_id, \
               outbox.expected_etag, outbox.payload, outbox.attempts, outbox.collection_revision, \
               outbox.target_remote_collection_id AS collection_remote_id, outbox.required_scope, \
               outbox.intent_hash, outbox.approval_id, outbox.run_claim_id, \
               outbox.run_claim_generation, outbox.provider_post_may_have_started",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(claim.account_id)
        .bind(now)
        .bind(claim_id)
        .bind(claim.claim_id)
        .bind(u64_to_i64(claim.claim_generation)?)
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
            if work.required_scope != required_scope
                || !granted_scopes.iter().any(|scope| scope == required_scope)
            {
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
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_run_claim(
            &mut transaction,
            self.scope,
            &SyncClaim {
                account_id: work.account_id,
                claim_id: work.run_claim_id,
                claim_generation: work.run_claim_generation,
            },
            now,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE google_sync_outbox outbox SET claimed_at = $5, updated_at = $5 \
             FROM google_sync_collections collection, provider_accounts account, items item, \
               google_sync_runs run \
             WHERE outbox.workspace_id = $1 AND outbox.id = $2 \
               AND outbox.provider_account_id = $3 AND outbox.state = 'delivering' \
               AND outbox.claim_id = $4 AND collection.workspace_id = outbox.workspace_id \
               AND collection.id = outbox.collection_id AND collection.selected \
               AND NOT collection.provider_deleted AND collection.sync_role = 'writable' \
               AND account.workspace_id = outbox.workspace_id AND account.id = outbox.provider_account_id \
               AND account.user_id = outbox.user_id AND account.provider = 'google' \
               AND account.status = 'active' AND account.sync_enabled \
               AND account.tombstoned_at IS NULL AND item.workspace_id = outbox.workspace_id \
               AND item.id = outbox.item_id AND item.revision = outbox.item_revision \
               AND run.workspace_id = outbox.workspace_id AND run.user_id = outbox.user_id \
               AND run.provider_account_id = outbox.provider_account_id \
               AND run.state = 'running' AND run.claim_id = $6 AND run.claim_generation = $7 \
               AND run.lease_until > $5 AND outbox.run_claim_id = run.claim_id \
               AND outbox.run_claim_generation = run.claim_generation \
               AND ((outbox.remote_resource_id IS NULL AND outbox.expected_etag IS NULL \
                     AND outbox.operation = 'upsert' \
                     AND NOT EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                       WHERE mapping.workspace_id = outbox.workspace_id \
                         AND mapping.provider_account_id = outbox.provider_account_id \
                         AND mapping.collection_id = outbox.collection_id \
                         AND mapping.entity_kind = 'item' AND mapping.tombstoned_at IS NULL \
                         AND (mapping.local_entity_id = outbox.item_id \
                           OR (outbox.entity_kind = 'calendar_event' \
                             AND mapping.remote_resource_id = outbox.payload->>'id')))) \
                 OR (outbox.remote_resource_id IS NOT NULL AND outbox.expected_etag IS NOT NULL \
                     AND EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                       WHERE mapping.workspace_id = outbox.workspace_id \
                         AND mapping.provider_account_id = outbox.provider_account_id \
                         AND mapping.collection_id = outbox.collection_id \
                         AND mapping.entity_kind = 'item' AND mapping.local_entity_id = outbox.item_id \
                         AND mapping.remote_resource_id = outbox.remote_resource_id \
                         AND mapping.remote_etag = outbox.expected_etag \
                         AND mapping.ownership = 'dayweave' AND mapping.tombstoned_at IS NULL)))",
        )
        .bind(self.scope.workspace_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(now)
        .bind(work.run_claim_id)
        .bind(u64_to_i64(work.run_claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            transaction.commit().await.map_err(internal)?;
            Ok(())
        } else {
            Err(GoogleSyncRepositoryError::ClaimLost)
        }
    }

    async fn authorize_outbound_dispatch(
        &self,
        work: &OutboundWork,
        provider_write: bool,
        now: DateTime<Utc>,
    ) -> Result<OutboundDispatchPermit, GoogleSyncRepositoryError> {
        let nonce = Uuid::new_v4();
        let expires_at = now + chrono::Duration::seconds(30);
        let task_post_may_start = provider_write
            && work.entity_kind == "task"
            && work.operation == OutboundOperation::Upsert
            && work.remote_resource_id.is_none();
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_run_identity(
            &mut transaction,
            self.scope,
            work.account_id,
            work.run_claim_id,
            work.run_claim_generation,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE google_sync_outbox outbox SET dispatch_nonce = $6, \
             dispatch_authorized_at = $7, dispatch_expires_at = $8, claimed_at = $7, updated_at = $7, \
             provider_post_may_have_started = \
               outbox.provider_post_may_have_started OR $24, \
             send_started_at = CASE WHEN $24 THEN COALESCE(outbox.send_started_at, $7) \
                                    ELSE outbox.send_started_at END \
             FROM provider_accounts account, google_sync_collections collection, items item, \
               google_outbound_previews approval, google_sync_runs run \
             WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 AND outbox.id = $3 \
               AND outbox.provider_account_id = $4 AND outbox.claim_id = $5 \
               AND outbox.state = 'delivering' AND outbox.approval_id IS NOT NULL \
               AND outbox.intent_hash = $9 AND outbox.collection_revision = $10 \
               AND outbox.target_remote_collection_id = $11 AND outbox.required_scope = $12 \
               AND outbox.approval_id = $15 \
               AND outbox.collection_id = $16 AND outbox.item_id = $17 \
               AND outbox.item_revision = $18 AND outbox.entity_kind = $19 \
               AND outbox.operation = $20 AND outbox.payload = $21 \
               AND outbox.remote_resource_id IS NOT DISTINCT FROM $22 \
               AND outbox.expected_etag IS NOT DISTINCT FROM $23 \
               AND outbox.run_claim_id = $25 AND outbox.run_claim_generation = $26 \
               AND run.workspace_id = outbox.workspace_id AND run.user_id = outbox.user_id \
               AND run.provider_account_id = outbox.provider_account_id \
               AND run.state = 'running' AND run.claim_id = $25 \
               AND run.claim_generation = $26 AND run.lease_until > $7 \
               AND account.workspace_id = outbox.workspace_id AND account.user_id = outbox.user_id \
               AND account.id = outbox.provider_account_id AND account.provider = 'google' \
               AND account.status = 'active' AND account.sync_enabled \
               AND account.tombstoned_at IS NULL AND outbox.required_scope = ANY(account.granted_scopes) \
               AND collection.workspace_id = outbox.workspace_id AND collection.user_id = outbox.user_id \
               AND collection.provider_account_id = outbox.provider_account_id \
               AND collection.id = outbox.collection_id AND collection.selected \
               AND NOT collection.provider_deleted AND collection.sync_role = 'writable' \
               AND collection.revision = outbox.collection_revision \
               AND collection.remote_collection_id = outbox.target_remote_collection_id \
               AND ((collection.collection_kind = 'calendar' AND outbox.entity_kind = 'calendar_event' \
                     AND outbox.required_scope = $13) \
                 OR (collection.collection_kind = 'task_list' AND outbox.entity_kind = 'task' \
                     AND outbox.required_scope = $14)) \
               AND item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
               AND item.revision = outbox.item_revision \
               AND approval.workspace_id = outbox.workspace_id AND approval.id = outbox.approval_id \
               AND approval.approved_at IS NOT NULL AND approval.consumed_at IS NOT NULL \
               AND approval.outbox_id = outbox.id AND approval.intent_hash = outbox.intent_hash \
               AND approval.provider_account_id = outbox.provider_account_id \
               AND approval.collection_id = outbox.collection_id \
               AND approval.collection_revision = outbox.collection_revision \
               AND approval.collection_remote_id = outbox.target_remote_collection_id \
               AND approval.item_id = outbox.item_id AND approval.item_revision = outbox.item_revision \
               AND approval.entity_kind = outbox.entity_kind AND approval.operation = outbox.operation \
               AND approval.required_scope = outbox.required_scope AND approval.payload = outbox.payload \
               AND approval.provider_resource_id IS NOT DISTINCT FROM outbox.remote_resource_id \
               AND approval.expected_etag IS NOT DISTINCT FROM outbox.expected_etag \
               AND ((outbox.remote_resource_id IS NULL AND outbox.expected_etag IS NULL \
                     AND outbox.operation = 'upsert' \
                     AND NOT EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                       WHERE mapping.workspace_id = outbox.workspace_id \
                         AND mapping.provider_account_id = outbox.provider_account_id \
                         AND mapping.collection_id = outbox.collection_id \
                         AND mapping.entity_kind = 'item' AND mapping.tombstoned_at IS NULL \
                         AND (mapping.local_entity_id = outbox.item_id \
                           OR (outbox.entity_kind = 'calendar_event' \
                             AND mapping.remote_resource_id = outbox.payload->>'id')))) \
                 OR (outbox.remote_resource_id IS NOT NULL AND outbox.expected_etag IS NOT NULL \
                     AND EXISTS (SELECT 1 FROM provider_sync_mappings mapping \
                       WHERE mapping.workspace_id = outbox.workspace_id \
                         AND mapping.provider_account_id = outbox.provider_account_id \
                         AND mapping.collection_id = outbox.collection_id \
                         AND mapping.entity_kind = 'item' AND mapping.local_entity_id = outbox.item_id \
                         AND mapping.remote_resource_id = outbox.remote_resource_id \
                         AND mapping.remote_etag = outbox.expected_etag \
                         AND mapping.ownership = 'dayweave' AND mapping.tombstoned_at IS NULL)))",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(nonce)
        .bind(now)
        .bind(expires_at)
        .bind(work.intent_hash.as_slice())
        .bind(u64_to_i64(work.collection_revision)?)
        .bind(&work.collection_remote_id)
        .bind(&work.required_scope)
        .bind(GOOGLE_CALENDAR_SCOPE)
        .bind(GOOGLE_TASKS_SCOPE)
        .bind(work.approval_id)
        .bind(work.collection_id)
        .bind(work.item_id)
        .bind(u64_to_i64(work.item_revision)?)
        .bind(&work.entity_kind)
        .bind(work.operation.as_db())
        .bind(&work.payload)
        .bind(&work.remote_resource_id)
        .bind(&work.expected_etag)
        .bind(task_post_may_start)
        .bind(work.run_claim_id)
        .bind(u64_to_i64(work.run_claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated != 1 {
            // A failed final fence must never strand `delivering`. Only the
            // still-current parent identity may perform this transition; a
            // takeover owns (and already reconciles) the row instead.
            sqlx::query(
                "UPDATE google_sync_outbox outbox SET \
                 state = CASE WHEN EXISTS (SELECT 1 FROM items item \
                                      WHERE item.workspace_id = outbox.workspace_id \
                                        AND item.id = outbox.item_id \
                                        AND item.revision <> outbox.item_revision) \
                              THEN 'superseded' ELSE 'conflict' END, \
                 claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
                 run_claim_generation = NULL, dispatch_nonce = NULL, \
                 dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
                 last_error_code = CASE WHEN EXISTS (SELECT 1 FROM items item \
                                              WHERE item.workspace_id = outbox.workspace_id \
                                                AND item.id = outbox.item_id \
                                                AND item.revision <> outbox.item_revision) \
                                        THEN 'superseded_before_provider_dispatch' \
                                        ELSE 'dispatch_authorization_denied' END, updated_at = $7 \
                 FROM google_sync_runs run WHERE outbox.workspace_id = $1 \
                   AND outbox.user_id = $2 AND outbox.id = $3 \
                   AND outbox.provider_account_id = $4 AND outbox.state = 'delivering' \
                   AND outbox.claim_id = $5 AND outbox.run_claim_id = $6 \
                   AND outbox.run_claim_generation = $8 \
                   AND run.workspace_id = outbox.workspace_id AND run.user_id = outbox.user_id \
                   AND run.provider_account_id = outbox.provider_account_id \
                   AND run.claim_id = $6 AND run.claim_generation = $8",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(work.id)
            .bind(work.account_id)
            .bind(work.claim_id)
            .bind(work.run_claim_id)
            .bind(now)
            .bind(u64_to_i64(work.run_claim_generation)?)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        transaction.commit().await.map_err(internal)?;
        Ok(OutboundDispatchPermit {
            nonce,
            intent_hash: work.intent_hash,
            expires_at,
        })
    }

    async fn cancel_outbound_before_send(
        &self,
        work: &OutboundWork,
        code: &'static str,
        available_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_run_identity(
            &mut transaction,
            self.scope,
            work.account_id,
            work.run_claim_id,
            work.run_claim_generation,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE google_sync_outbox outbox SET state = 'backoff', claim_id = NULL, \
             claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
             dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
             provider_post_may_have_started = false, send_started_at = NULL, \
             available_at = $9, last_error_code = $8, updated_at = $10 \
             FROM google_sync_runs run WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
               AND outbox.id = $3 AND outbox.provider_account_id = $4 \
               AND outbox.state = 'delivering' AND outbox.claim_id = $5 \
               AND outbox.run_claim_id = $6 AND outbox.run_claim_generation = $7 \
               AND run.workspace_id = outbox.workspace_id AND run.user_id = outbox.user_id \
               AND run.provider_account_id = outbox.provider_account_id \
               AND run.claim_id = $6 AND run.claim_generation = $7",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(work.run_claim_id)
        .bind(u64_to_i64(work.run_claim_generation)?)
        .bind(code)
        .bind(available_at)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            transaction.commit().await.map_err(internal)?;
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
        ensure_run_identity(
            &mut transaction,
            self.scope,
            work.account_id,
            work.run_claim_id,
            work.run_claim_generation,
        )
        .await?;
        let authorization_valid = sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM google_sync_outbox outbox \
             JOIN provider_accounts account ON account.workspace_id = outbox.workspace_id \
               AND account.user_id = outbox.user_id AND account.id = outbox.provider_account_id \
             JOIN google_sync_collections collection ON collection.workspace_id = outbox.workspace_id \
               AND collection.user_id = outbox.user_id \
               AND collection.provider_account_id = outbox.provider_account_id \
               AND collection.id = outbox.collection_id \
             JOIN items item ON item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
             JOIN google_outbound_previews approval ON approval.workspace_id = outbox.workspace_id \
               AND approval.id = outbox.approval_id \
             JOIN google_sync_runs run ON run.workspace_id = outbox.workspace_id \
               AND run.user_id = outbox.user_id \
               AND run.provider_account_id = outbox.provider_account_id \
             WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 AND outbox.id = $3 \
               AND outbox.provider_account_id = $4 AND outbox.claim_id = $5 \
               AND outbox.state = 'delivering' AND outbox.dispatch_nonce = $6 \
               AND outbox.intent_hash = $7 AND outbox.collection_revision = $8 \
               AND outbox.target_remote_collection_id = $9 AND outbox.required_scope = $10 \
               AND outbox.run_claim_id = $13 AND outbox.run_claim_generation = $14 \
               AND run.state = 'running' AND run.claim_id = $13 \
               AND run.claim_generation = $14 AND run.lease_until > $15 \
               AND account.provider = 'google' AND account.status = 'active' AND account.sync_enabled \
               AND account.tombstoned_at IS NULL AND outbox.required_scope = ANY(account.granted_scopes) \
               AND collection.selected AND NOT collection.provider_deleted \
               AND collection.sync_role = 'writable' AND collection.revision = outbox.collection_revision \
               AND collection.remote_collection_id = outbox.target_remote_collection_id \
               AND ((collection.collection_kind = 'calendar' AND outbox.entity_kind = 'calendar_event' \
                     AND outbox.required_scope = $11) \
                 OR (collection.collection_kind = 'task_list' AND outbox.entity_kind = 'task' \
                     AND outbox.required_scope = $12)) \
               AND item.revision = outbox.item_revision \
               AND approval.approved_at IS NOT NULL AND approval.consumed_at IS NOT NULL \
               AND approval.outbox_id = outbox.id AND approval.intent_hash = outbox.intent_hash \
               AND approval.provider_account_id = outbox.provider_account_id \
               AND approval.collection_id = outbox.collection_id \
               AND approval.collection_revision = outbox.collection_revision \
               AND approval.collection_remote_id = outbox.target_remote_collection_id \
               AND approval.item_id = outbox.item_id AND approval.item_revision = outbox.item_revision \
               AND approval.entity_kind = outbox.entity_kind AND approval.operation = outbox.operation \
               AND approval.required_scope = outbox.required_scope AND approval.payload = outbox.payload \
               AND approval.provider_resource_id IS NOT DISTINCT FROM outbox.remote_resource_id \
               AND approval.expected_etag IS NOT DISTINCT FROM outbox.expected_etag \
             FOR SHARE OF outbox, account, collection, item, approval, run",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(result.dispatch_nonce)
        .bind(work.intent_hash.as_slice())
        .bind(u64_to_i64(work.collection_revision)?)
        .bind(&work.collection_remote_id)
        .bind(&work.required_scope)
        .bind(GOOGLE_CALENDAR_SCOPE)
        .bind(GOOGLE_TASKS_SCOPE)
        .bind(work.run_claim_id)
        .bind(u64_to_i64(work.run_claim_generation)?)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .is_some();
        if !authorization_valid {
            let current_revision: Option<i64> = sqlx::query_scalar(
                "SELECT revision FROM items WHERE workspace_id = $1 AND id = $2 FOR SHARE",
            )
            .bind(self.scope.workspace_id)
            .bind(work.item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(internal)?;
            let superseded = current_revision != Some(u64_to_i64(work.item_revision)?);
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                if superseded { "superseded" } else { "conflict" },
                if superseded {
                    "superseded_during_delivery"
                } else {
                    "dispatch_authorization_changed"
                },
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
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
        if work.required_scope != required_scope
            || !account_valid
            || !granted_scopes.iter().any(|scope| scope == required_scope)
        {
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
        // Lock both possible identities before publication. The conditional
        // mutation below is still required for the create case because an
        // absent row cannot itself be locked against a concurrent insert.
        let mapping_rows = sqlx::query(
            "SELECT id, local_entity_id, remote_resource_id, remote_etag, ownership \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND provider_account_id = $2 \
               AND collection_id = $3 AND entity_kind = 'item' AND tombstoned_at IS NULL \
               AND (local_entity_id = $4 OR remote_resource_id = $5) ORDER BY id FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(work.account_id)
        .bind(work.collection_id)
        .bind(work.item_id)
        .bind(&result.remote_resource_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(internal)?;
        let mapping = mapping_rows.iter().find(|mapping| {
            mapping
                .try_get::<Option<Uuid>, _>("local_entity_id")
                .ok()
                .flatten()
                == Some(work.item_id)
        });
        let remote_identity_conflict = mapping_rows.iter().any(|candidate| {
            candidate
                .try_get::<String, _>("remote_resource_id")
                .ok()
                .as_deref()
                == Some(result.remote_resource_id.as_str())
                && candidate.try_get::<Uuid, _>("id").ok()
                    != mapping.and_then(|current| current.try_get::<Uuid, _>("id").ok())
        });
        let mapping_valid = match (&work.remote_resource_id, &work.expected_etag, mapping) {
            (None, None, None) => !remote_identity_conflict,
            (Some(expected_remote_id), Some(expected_etag), Some(mapping)) => {
                mapping
                    .try_get::<String, _>("remote_resource_id")
                    .ok()
                    .as_ref()
                    == Some(expected_remote_id)
                    && mapping
                        .try_get::<Option<String>, _>("remote_etag")
                        .ok()
                        .flatten()
                        .as_ref()
                        == Some(expected_etag)
                    && mapping.try_get::<String, _>("ownership").ok().as_deref() == Some("dayweave")
                    && !remote_identity_conflict
            }
            _ => false,
        };
        if !mapping_valid {
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                "conflict",
                "provider_mapping_changed_during_delivery",
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        let mapping_state = if work.operation == OutboundOperation::Delete {
            "deleted_remote"
        } else {
            "synced"
        };
        let mapping_updated = if let Some(mapping) = mapping {
            let mapping_id: Uuid = mapping.try_get("id").map_err(internal)?;
            sqlx::query(
                "UPDATE provider_sync_mappings SET remote_resource_id = $2, remote_etag = $3, \
                 remote_updated_at = $4, remote_payload_hash = $5, local_revision = $6, \
                 sync_state = $7, ownership = 'dayweave', conflict_metadata = NULL, updated_at = $8 \
                 WHERE id = $1 AND workspace_id = $9 AND provider_account_id = $10 \
                   AND collection_id = $11 AND entity_kind = 'item' AND local_entity_id = $12 \
                   AND remote_resource_id = $13 AND remote_etag = $14 AND ownership = 'dayweave' \
                   AND tombstoned_at IS NULL",
            )
            .bind(mapping_id)
            .bind(&result.remote_resource_id)
            .bind(&result.remote_etag)
            .bind(result.remote_updated_at)
            .bind(result.payload_hash.as_slice())
            .bind(u64_to_i64(work.item_revision)?)
            .bind(mapping_state)
            .bind(now)
            .bind(self.scope.workspace_id)
            .bind(work.account_id)
            .bind(work.collection_id)
            .bind(work.item_id)
            .bind(&work.remote_resource_id)
            .bind(&work.expected_etag)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?
            .rows_affected()
        } else {
            sqlx::query(
                "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
                 collection_id, entity_kind, local_entity_id, remote_resource_id, remote_etag, \
                 remote_updated_at, remote_payload_hash, local_revision, sync_state, ownership, \
                 created_at, updated_at) VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, $8, $9, \
                 $10, $11, 'dayweave', $12, $12) ON CONFLICT DO NOTHING",
            )
            .bind(Uuid::new_v4())
            .bind(self.scope.workspace_id)
            .bind(work.account_id)
            .bind(work.collection_id)
            .bind(work.item_id)
            .bind(&result.remote_resource_id)
            .bind(&result.remote_etag)
            .bind(result.remote_updated_at)
            .bind(result.payload_hash.as_slice())
            .bind(u64_to_i64(work.item_revision)?)
            .bind(mapping_state)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?
            .rows_affected()
        };
        if mapping_updated != 1 {
            revoke_outbound_after_provider(
                &mut transaction,
                self.scope,
                work,
                "conflict",
                "provider_mapping_changed_during_delivery",
                now,
            )
            .await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleSyncRepositoryError::ClaimLost);
        }
        let updated = sqlx::query(
            "UPDATE google_sync_outbox SET state = 'published', claim_id = NULL, claimed_at = NULL, \
             run_claim_id = NULL, run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
             attempts = attempts + 1, last_error_code = NULL, updated_at = $5 \
             WHERE workspace_id = $1 AND id = $2 \
             AND provider_account_id = $3 AND state = 'delivering' AND claim_id = $4 \
             AND dispatch_nonce = $6 AND run_claim_id = $7 AND run_claim_generation = $8",
        )
        .bind(self.scope.workspace_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(now)
        .bind(result.dispatch_nonce)
        .bind(work.run_claim_id)
        .bind(u64_to_i64(work.run_claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated != 1 {
            return Err(GoogleSyncRepositoryError::ClaimLost);
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
        let markerless_task_create = work.entity_kind == "task"
            && work.operation == OutboundOperation::Upsert
            && work.remote_resource_id.is_none();
        // Once the final dispatch transaction records a markerless Tasks POST,
        // only an explicit non-success provider response proves that no object
        // was created. Protocol/transport/internal failures after that point
        // retain identity-unresolved evidence even if a caller misclassifies
        // the unusable 2xx response.
        let preserve_task_post_uncertainty = markerless_task_create
            && !matches!(
                code,
                "reauthorization_required"
                    | "rate_limited"
                    | "precondition_failed"
                    | "conditional_write_required"
                    | "provider_not_found"
                    | "provider_rejected"
            );
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        ensure_run_identity(
            &mut transaction,
            self.scope,
            work.account_id,
            work.run_claim_id,
            work.run_claim_generation,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE google_sync_outbox outbox SET \
             state = CASE WHEN $9 AND outbox.provider_post_may_have_started \
                          THEN 'conflict' ELSE $5 END, \
             claim_id = NULL, claimed_at = NULL, \
             run_claim_id = NULL, run_claim_generation = NULL, dispatch_nonce = NULL, \
             dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
             provider_post_may_have_started = $9 AND outbox.provider_post_may_have_started, \
             send_started_at = CASE WHEN $9 AND outbox.provider_post_may_have_started \
                                    THEN outbox.send_started_at ELSE NULL END, \
             attempts = attempts + 1, available_at = $6, \
             last_error_code = CASE WHEN $9 AND outbox.provider_post_may_have_started \
                                    THEN 'provider_identity_unresolved' ELSE $7 END, updated_at = $8 \
             FROM google_sync_runs run WHERE outbox.workspace_id = $1 AND outbox.id = $2 \
               AND outbox.provider_account_id = $3 AND outbox.state = 'delivering' \
               AND outbox.claim_id = $4 AND outbox.run_claim_id = $10 \
               AND outbox.run_claim_generation = $11 \
               AND run.workspace_id = outbox.workspace_id AND run.user_id = outbox.user_id \
               AND run.provider_account_id = outbox.provider_account_id \
               AND run.claim_id = $10 AND run.claim_generation = $11",
        )
        .bind(self.scope.workspace_id)
        .bind(work.id)
        .bind(work.account_id)
        .bind(work.claim_id)
        .bind(terminal_state)
        .bind(available_at)
        .bind(code)
        .bind(now)
        .bind(preserve_task_post_uncertainty)
        .bind(work.run_claim_id)
        .bind(u64_to_i64(work.run_claim_generation)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated == 1 {
            transaction.commit().await.map_err(internal)?;
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
         AND claim_generation = $6 AND lease_until > $5 FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(claim.account_id)
    .bind(claim.claim_id)
    .bind(now)
    .bind(u64_to_i64(claim.claim_generation)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    if retained.is_some() {
        Ok(())
    } else {
        Err(GoogleSyncRepositoryError::ClaimLost)
    }
}

async fn ensure_run_identity(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    claim_id: Uuid,
    claim_generation: u64,
) -> Result<(), GoogleSyncRepositoryError> {
    let retained = sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM google_sync_runs WHERE workspace_id = $1 AND user_id = $2 \
         AND provider_account_id = $3 AND claim_id = $4 AND claim_generation = $5 FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(claim_id)
    .bind(u64_to_i64(claim_generation)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    if retained.is_some() {
        Ok(())
    } else {
        Err(GoogleSyncRepositoryError::ClaimLost)
    }
}

async fn reconcile_outbound_for_parent_end(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    claim: &SyncClaim,
    code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "UPDATE google_sync_outbox SET \
         state = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                            AND remote_resource_id IS NULL \
                            AND provider_post_may_have_started \
                       THEN 'conflict' ELSE 'backoff' END, \
         claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
         run_claim_generation = NULL, dispatch_nonce = NULL, \
         dispatch_authorized_at = NULL, dispatch_expires_at = NULL, available_at = $7, \
         last_error_code = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                                     AND remote_resource_id IS NULL \
                                     AND provider_post_may_have_started \
                                THEN 'provider_identity_unresolved' ELSE $6 END, updated_at = $7 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
           AND state = 'delivering' AND run_claim_id = $4 AND run_claim_generation = $5",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(claim.account_id)
    .bind(claim.claim_id)
    .bind(u64_to_i64(claim.claim_generation)?)
    .bind(code)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

fn validate_calendar_projection_batch(
    claim: &SyncClaim,
    batch: &CalendarProjectionBatch,
) -> Result<(), GoogleSyncRepositoryError> {
    if claim.account_id != batch.account_id
        || batch.window.start >= batch.window.end
        || !batch
            .window
            .start
            .timestamp_subsec_nanos()
            .is_multiple_of(1_000)
        || !batch
            .window
            .end
            .timestamp_subsec_nanos()
            .is_multiple_of(1_000)
        || batch.window.end - batch.window.start
            > chrono::Duration::days(MAX_CALENDAR_PROJECTION_WINDOW_DAYS)
        || batch
            .changes
            .len()
            .checked_add(batch.rejected.len())
            .is_none_or(|count| count > MAX_CALENDAR_PROJECTION_ENTRIES)
    {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    let mut remote_ids = BTreeSet::new();
    let mut new_item_ids = BTreeSet::new();
    for change in &batch.changes {
        if change.account_id != batch.account_id
            || change.collection_id != batch.collection_id
            || change.collection_revision != batch.collection_revision
            || (change.dayweave_item_id.is_none() && change.reviewed_provider_projection.is_some())
            || (change.dayweave_item_id.is_some() && change.reviewed_provider_projection.is_none())
            || !valid_provider_text(&change.remote_id, 1000)
            || change
                .remote_parent_id
                .as_deref()
                .is_some_and(|value| !valid_provider_text(value, 1000))
            || change
                .remote_etag
                .as_deref()
                .is_some_and(|value| !valid_provider_text(value, 1000))
            || !remote_ids.insert(change.remote_id.as_str())
        {
            return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
        }
        if change.dayweave_item_id.is_none()
            && let Some(item) = &change.item
        {
            validate_calendar_occurrence_item(item)?;
            if !new_item_ids.insert(item.id) {
                return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
            }
        }
    }
    for rejected in &batch.rejected {
        if !valid_provider_text(&rejected.remote_id, 1000)
            || !valid_projection_rejection_reason(rejected.reason)
            || !remote_ids.insert(rejected.remote_id.as_str())
        {
            return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
        }
    }
    Ok(())
}

fn validate_calendar_occurrence_item(item: &NewItem) -> Result<(), GoogleSyncRepositoryError> {
    let constraints = item
        .flexible_constraints
        .as_object()
        .ok_or(GoogleSyncRepositoryError::InvalidProjectionBatch)?;
    let typed_constraints = constraints.len() == 1
        && (constraints.contains_key("calendar_event")
            || constraints.contains_key("calendar_context"));
    if item.kind != crate::items::ItemKind::Event
        || item.status != ItemStatus::Scheduled
        || item.recurrence.is_some()
        || item.parent_id.is_some()
        || !matches!(item.split_policy, SplitPolicy::Indivisible)
        || !typed_constraints
        || !valid_database_text(&item.title, 500)
        || (item.is_sensitive && (item.title != "Busy" || item.notes.is_some()))
        || item
            .notes
            .as_deref()
            .is_some_and(|value| !valid_database_text(value, 100_000))
        || !json_is_provider_evidence_safe(&item.flexible_constraints, 0)
        || Item::new(item.clone(), Utc::now()).is_err()
    {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    let (start, end) = if let Some(value) = constraints.get("calendar_event") {
        validate_calendar_event_constraints(value)?
    } else {
        validate_calendar_context_constraints(
            constraints
                .get("calendar_context")
                .ok_or(GoogleSyncRepositoryError::InvalidProjectionBatch)?,
        )?
    };
    let duration = (end - start)
        .num_seconds()
        .try_into()
        .map_err(|_| GoogleSyncRepositoryError::InvalidProjectionBatch)?;
    if item.earliest_start_at != Some(start)
        || item.deadline_at != Some(end)
        || item.duration_seconds != Some(duration)
    {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    Ok(())
}

fn validate_calendar_event_constraints(
    value: &Value,
) -> Result<(DateTime<Utc>, DateTime<Utc>), GoogleSyncRepositoryError> {
    let object = value
        .as_object()
        .ok_or(GoogleSyncRepositoryError::InvalidProjectionBatch)?;
    if object.len() != 5
        || !["start", "end", "immutable", "all_day"]
            .iter()
            .all(|key| object.contains_key(*key))
        || !object.keys().all(|key| {
            matches!(
                key.as_str(),
                "start" | "end" | "immutable" | "all_day" | "source_calendar_id"
            )
        })
        || object.get("immutable").and_then(Value::as_bool) != Some(true)
        || object.get("all_day").and_then(Value::as_bool).is_none()
        || object
            .get("source_calendar_id")
            .is_none_or(|value| !value.is_null())
    {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    projection_bounds(object)
}

fn validate_calendar_context_constraints(
    value: &Value,
) -> Result<(DateTime<Utc>, DateTime<Utc>), GoogleSyncRepositoryError> {
    let object = value
        .as_object()
        .ok_or(GoogleSyncRepositoryError::InvalidProjectionBatch)?;
    if object.len() != 3
        || !["start", "end", "all_day"]
            .iter()
            .all(|key| object.contains_key(*key))
        || object.get("all_day").and_then(Value::as_bool).is_none()
    {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    projection_bounds(object)
}

fn projection_bounds(
    object: &serde_json::Map<String, Value>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), GoogleSyncRepositoryError> {
    let parse = |key| {
        let value = object
            .get(key)
            .and_then(Value::as_str)
            .ok_or(GoogleSyncRepositoryError::InvalidProjectionBatch)?;
        let instant = DateTime::parse_from_rfc3339(value)
            .map_err(|_| GoogleSyncRepositoryError::InvalidProjectionBatch)?
            .with_timezone(&Utc);
        if !instant.timestamp_subsec_nanos().is_multiple_of(1_000) {
            return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
        }
        Ok(instant)
    };
    let start = parse("start")?;
    let end = parse("end")?;
    if start >= end {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    Ok((start, end))
}

fn json_is_provider_evidence_safe(value: &Value, depth: usize) -> bool {
    if depth > 16 {
        return false;
    }
    match value {
        Value::String(value) => valid_database_text(value, 16_384),
        Value::Array(values) => values
            .iter()
            .all(|value| json_is_provider_evidence_safe(value, depth + 1)),
        Value::Object(values) => values.iter().all(|(key, value)| {
            valid_database_text(key, 200)
                && !matches!(
                    key.as_str(),
                    "account_id"
                        | "collection_id"
                        | "remote_id"
                        | "remote_parent_id"
                        | "series_remote_id"
                )
                && json_is_provider_evidence_safe(value, depth + 1)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
    }
}

fn valid_database_text(value: &str, max_chars: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max_chars && !value.chars().any(char::is_control)
}

fn valid_provider_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_projection_rejection_reason(reason: &str) -> bool {
    matches!(
        reason,
        "canonical_item_invalid"
            | "dayweave_marker_invalid"
            | "event_bounds_invalid"
            | "event_bounds_missing"
            | "event_date_invalid"
            | "event_duration_invalid"
            | "event_timezone_invalid"
            | "invalid_remote_id"
            | "provider_metadata_invalid"
            | "provider_payload_invalid"
            | "timestamp_invalid"
            | "unauthenticated_dayweave_marker"
    )
}

fn validate_calendar_series_change(
    claim: &SyncClaim,
    change: &RemoteCalendarSeriesChange,
) -> Result<(), GoogleSyncRepositoryError> {
    if claim.account_id != change.account_id
        || !valid_provider_text(&change.remote_id, 1000)
        || change
            .remote_etag
            .as_deref()
            .is_some_and(|value| !valid_provider_text(value, 1000))
        || (change.dayweave_item_id.is_none() && change.reviewed_provider_projection.is_some())
        || (change.dayweave_item_id.is_some()
            && !change.deleted
            && change.reviewed_provider_projection.is_none())
        || (change.deleted && change.reviewed_provider_projection.is_some())
    {
        return Err(GoogleSyncRepositoryError::InvalidProjectionBatch);
    }
    Ok(())
}

async fn lock_calendar_projection_collection(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    batch: &CalendarProjectionBatch,
) -> Result<u64, GoogleSyncRepositoryError> {
    let row = sqlx::query(
        "SELECT planning_generation, collection_kind, revision, selected, provider_deleted \
         FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
           AND provider_account_id = $3 AND id = $4 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleSyncRepositoryError::CollectionNotFound)?;
    if row
        .try_get::<String, _>("collection_kind")
        .map_err(internal)?
        != "calendar"
        || !row.try_get::<bool, _>("selected").map_err(internal)?
        || row
            .try_get::<bool, _>("provider_deleted")
            .map_err(internal)?
        || i64_to_u64(row.try_get("revision").map_err(internal)?)? != batch.collection_revision
    {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    i64_to_u64(row.try_get("planning_generation").map_err(internal)?)
}

async fn record_projection_rejections(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    batch: &CalendarProjectionBatch,
    generation: u64,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "DELETE FROM google_calendar_projection_rejections WHERE workspace_id = $1 \
         AND user_id = $2 AND provider_account_id = $3 AND collection_id = $4",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    for rejected in &batch.rejected {
        sqlx::query(
            "INSERT INTO google_calendar_projection_rejections (workspace_id, user_id, \
             provider_account_id, collection_id, collection_revision, remote_resource_id, \
             reason_code, observed_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(batch.account_id)
        .bind(batch.collection_id)
        .bind(u64_to_i64(batch.collection_revision)?)
        .bind(&rejected.remote_id)
        .bind(rejected.reason)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    }
    let affected = sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'failed', \
         planning_collection_revision = NULL, planning_window_start = NULL, \
         planning_window_end = NULL, planning_window_refreshed_at = NULL, \
         planning_last_error_code = 'projection_rejected', updated_at = $7 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 AND id = $4 \
           AND revision = $5 AND planning_generation = $6",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .bind(u64_to_i64(batch.collection_revision)?)
    .bind(u64_to_i64(generation)?)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if affected != 1 {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    let reason_codes: BTreeSet<_> = batch
        .rejected
        .iter()
        .map(|rejected| rejected.reason)
        .collect();
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, 'google.calendar_projection_rejected', \
         'google_calendar_projection', $4, 'rejected', $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.collection_id)
    .bind(json!({"count": batch.rejected.len(), "reason_codes": reason_codes}))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn seal_calendar_projection(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    batch: &CalendarProjectionBatch,
    previous_generation: u64,
    next_generation: u64,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "DELETE FROM google_calendar_projection_rejections WHERE workspace_id = $1 \
         AND user_id = $2 AND provider_account_id = $3 AND collection_id = $4",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    let affected = sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'complete', \
         planning_generation = $7, planning_collection_revision = $5, \
         planning_window_start = $8, planning_window_end = $9, \
         planning_window_refreshed_at = $10, planning_last_error_code = NULL, \
         last_import_at = $10, updated_at = $10 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 AND id = $4 \
           AND revision = $5 AND planning_generation = $6",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .bind(u64_to_i64(batch.collection_revision)?)
    .bind(u64_to_i64(previous_generation)?)
    .bind(u64_to_i64(next_generation)?)
    .bind(batch.window.start)
    .bind(batch.window.end)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if affected != 1 {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, result_revision, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, 'google.calendar_projection_replaced', \
         'google_calendar_projection', $4, $5, 'succeeded', \
         jsonb_build_object('generation', $5, 'occurrence_count', $6), $7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.collection_id)
    .bind(u64_to_i64(next_generation)?)
    .bind(i64::try_from(batch.changes.len()).map_err(internal)?)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn finish_projection_semantic_failure(
    mut transaction: Transaction<'_, Postgres>,
    scope: DatabaseScope,
    batch: &CalendarProjectionBatch,
    generation: u64,
    error: GoogleSyncRepositoryError,
    now: DateTime<Utc>,
) -> Result<CalendarProjectionResult, GoogleSyncRepositoryError> {
    if !matches!(
        &error,
        GoogleSyncRepositoryError::CursorConflict
            | GoogleSyncRepositoryError::InvalidProjectionBatch
            | GoogleSyncRepositoryError::ItemNotFound
    ) {
        return Err(error);
    }
    sqlx::query("ROLLBACK TO SAVEPOINT replace_calendar_projection")
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
    let affected = sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'failed', \
         planning_collection_revision = NULL, planning_window_start = NULL, \
         planning_window_end = NULL, planning_window_refreshed_at = NULL, \
         planning_last_error_code = 'projection_conflict', updated_at = $7 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 AND id = $4 \
           AND revision = $5 AND planning_generation = $6",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .bind(u64_to_i64(batch.collection_revision)?)
    .bind(u64_to_i64(generation)?)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if affected != 1 {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, 'google.calendar_projection_conflicted', \
         'google_calendar_projection', $4, 'conflicted', \
         '{\"reason\":\"canonical_projection_conflict\"}'::jsonb, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(batch.collection_id)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(internal)?;
    sqlx::query("RELEASE SAVEPOINT replace_calendar_projection")
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
    transaction.commit().await.map_err(internal)?;
    Ok(CalendarProjectionResult {
        generation,
        complete: false,
        counts: SyncCounts {
            conflicts: 1,
            ..SyncCounts::default()
        },
    })
}

async fn replace_calendar_projection_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    batch: &CalendarProjectionBatch,
    generation: u64,
    now: DateTime<Utc>,
) -> Result<SyncCounts, GoogleSyncRepositoryError> {
    let mut counts = SyncCounts::default();
    let owned_remote_ids = owned_calendar_remote_ids(transaction, scope, batch).await?;
    for change in &batch.changes {
        if (change.dayweave_item_id.is_some() || owned_remote_ids.contains(&change.remote_id))
            && validate_owned_calendar_occurrence(transaction, scope, change).await?
        {
            continue;
        }
        counts.add(
            apply_calendar_occurrence_change(transaction, scope, change, generation, now).await?,
        );
    }
    // The first arm is bounded by the current unresolved occurrence set and a
    // matching partial index. The second arm starts from live canonical items,
    // then uses the active local-identity uniqueness index to catch a user
    // restore behind an otherwise dormant deleted mapping. Historical
    // deleted+trashed mappings are deliberately not rewritten every refresh;
    // an occurrence that reappears is still found by its remote identity and
    // restores the same mapping/item.
    let missing = sqlx::query(
        "WITH candidate_ids AS MATERIALIZED ( \
           SELECT mapping.id FROM provider_sync_mappings mapping \
           WHERE mapping.workspace_id = $1 AND mapping.provider_account_id = $2 \
             AND mapping.collection_id = $3 AND mapping.entity_kind = 'calendar_occurrence' \
             AND mapping.tombstoned_at IS NULL AND mapping.sync_state <> 'deleted_remote' \
             AND mapping.projection_generation <> $4 \
           UNION ALL \
           SELECT restored.id FROM items item \
           CROSS JOIN LATERAL ( \
             SELECT mapping.id FROM provider_sync_mappings mapping \
             WHERE mapping.workspace_id = item.workspace_id \
               AND mapping.provider_account_id = $2 AND mapping.collection_id = $3 \
               AND mapping.entity_kind = 'calendar_occurrence' \
               AND mapping.local_entity_id = item.id AND mapping.tombstoned_at IS NULL \
               AND mapping.sync_state = 'deleted_remote' \
               AND mapping.projection_generation <> $4 LIMIT 1 \
           ) restored \
           WHERE item.workspace_id = $1 AND item.trashed_at IS NULL \
         ) \
         SELECT mapping.id, mapping.local_entity_id, mapping.local_revision \
         FROM provider_sync_mappings mapping JOIN candidate_ids candidate ON candidate.id = mapping.id \
         ORDER BY mapping.remote_resource_id FOR UPDATE OF mapping",
    )
    .bind(scope.workspace_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .bind(u64_to_i64(generation)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    for mapping in &missing {
        counts.add(
            retire_calendar_occurrence_mapping(transaction, scope, mapping, generation, now)
                .await?,
        );
    }
    Ok(counts)
}

async fn owned_calendar_remote_ids(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    batch: &CalendarProjectionBatch,
) -> Result<HashSet<String>, GoogleSyncRepositoryError> {
    // Restrict the identity lookup to this already-bounded provider batch.
    // This catches stripped markers without adding one query per external
    // occurrence or loading an account's entire historical mapping set.
    let remote_ids: Vec<String> = batch
        .changes
        .iter()
        .map(|change| change.remote_id.clone())
        .collect();
    sqlx::query_scalar(
        "SELECT remote_resource_id FROM provider_sync_mappings \
         WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_id = $3 \
           AND entity_kind = 'item' AND ownership = 'dayweave' AND tombstoned_at IS NULL \
           AND remote_resource_id = ANY($4::text[]) ORDER BY remote_resource_id FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(batch.account_id)
    .bind(batch.collection_id)
    .bind(remote_ids)
    .fetch_all(&mut **transaction)
    .await
    .map(|values| values.into_iter().collect())
    .map_err(internal)
}

async fn validate_owned_calendar_occurrence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    change: &RemoteItemChange,
) -> Result<bool, GoogleSyncRepositoryError> {
    // Consult the durable series identity even when Google omits private
    // extended properties from a cancellation tombstone. A markerless live
    // echo must never fall through and become a duplicate external occurrence.
    let mapping = sqlx::query(
        "SELECT local_entity_id, local_revision, ownership, sync_state, \
                remote_payload_hash, remote_etag \
         FROM provider_sync_mappings WHERE workspace_id = $1 AND provider_account_id = $2 \
           AND collection_id = $3 AND entity_kind = 'item' \
           AND remote_resource_id = $4 AND tombstoned_at IS NULL FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(&change.remote_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(mapping) = mapping else {
        if change.dayweave_item_id.is_some() {
            return Err(GoogleSyncRepositoryError::CursorConflict);
        }
        return Ok(false);
    };
    let ownership: String = mapping.try_get("ownership").map_err(internal)?;
    if ownership != "dayweave" {
        if change.dayweave_item_id.is_some() {
            return Err(GoogleSyncRepositoryError::CursorConflict);
        }
        return Ok(false);
    }
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    let mapping_state: String = mapping.try_get("sync_state").map_err(internal)?;
    let remote_payload_hash: Option<Vec<u8>> =
        mapping.try_get("remote_payload_hash").map_err(internal)?;
    let remote_etag: Option<String> = mapping.try_get("remote_etag").map_err(internal)?;
    let marker_matches_or_is_absent = change
        .dayweave_item_id
        .is_none_or(|item_id| Some(item_id) == local_id);
    let deleted_item_matches =
        deleted_owned_item_matches(transaction, scope.workspace_id, local_id, local_revision)
            .await?;

    // Metadata ingestion has already authenticated this exact provider
    // tombstone and advanced the mapping hash/ETag while retaining the durable
    // deleted_remote publication acknowledgement. Exact matching is what
    // distinguishes a cancellation from a policy-ignored live resurrection,
    // since both normalize to `item = None` in the expanded lane.
    if mapping_state == "deleted_remote"
        && change.item.is_none()
        && marker_matches_or_is_absent
        && deleted_item_matches
        && remote_payload_hash.as_deref() == Some(change.remote_payload_hash.as_slice())
        && remote_etag == change.remote_etag
    {
        return Ok(true);
    }

    let Some(item_id) = change.dayweave_item_id else {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    };
    if change.reviewed_provider_projection.is_none()
        || Some(item_id) != local_id
        || mapping_state != "synced"
        || remote_payload_hash.as_deref() != Some(change.remote_payload_hash.as_slice())
        || remote_etag != change.remote_etag
        || deleted_item_matches
    {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    let active_revision_matches: Option<bool> = sqlx::query_scalar(
        "SELECT revision = $3 AND trashed_at IS NULL FROM items \
         WHERE workspace_id = $1 AND id = $2 FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(local_revision)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    if active_revision_matches != Some(true) {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    Ok(true)
}

async fn deleted_owned_item_matches(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    local_id: Option<Uuid>,
    local_revision: Option<i64>,
) -> Result<bool, GoogleSyncRepositoryError> {
    let (Some(item_id), Some(expected_revision)) = (local_id, local_revision) else {
        return Ok(false);
    };
    sqlx::query_scalar(
        "SELECT revision = $3 AND trashed_at IS NOT NULL FROM items \
         WHERE workspace_id = $1 AND id = $2 FOR SHARE",
    )
    .bind(workspace_id)
    .bind(item_id)
    .bind(expected_revision)
    .fetch_optional(&mut **transaction)
    .await
    .map(|value| value == Some(true))
    .map_err(internal)
}

#[allow(clippy::too_many_lines)] // One occurrence's item, mapping, delta, and privacy floor are atomic.
async fn apply_calendar_occurrence_change(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    change: &RemoteItemChange,
    generation: u64,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let mapping = sqlx::query(
        "SELECT id, local_entity_id, local_revision, remote_projection_hash, \
         provider_forced_sensitive FROM provider_sync_mappings \
         WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_id = $3 \
           AND entity_kind = 'calendar_occurrence' AND remote_resource_id = $4 \
           AND tombstoned_at IS NULL FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(&change.remote_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(mapping) = mapping else {
        return insert_calendar_occurrence(transaction, scope, change, generation, now).await;
    };
    let mapping_id: Uuid = mapping.try_get("id").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    let old_projection_hash: Option<Vec<u8>> = mapping
        .try_get("remote_projection_hash")
        .map_err(internal)?;
    let forced_sensitive: bool = mapping
        .try_get("provider_forced_sensitive")
        .map_err(internal)?;
    let current = if let Some(local_id) = local_id {
        fetch_import_item_optional(transaction, scope.workspace_id, local_id).await?
    } else {
        None
    };
    let mapped_item_exists = current.is_some();
    let Some(input) = change.item.as_ref() else {
        if local_id.is_some() && !mapped_item_exists {
            update_dangling_calendar_occurrence_tombstone(
                transaction,
                mapping_id,
                change,
                generation,
                now,
            )
            .await?;
            return Ok(ImportOutcome::Unchanged);
        }
        let outcome = if let Some(local_id) = local_id {
            trash_projected_item(
                transaction,
                scope,
                local_id,
                local_revision,
                "item.google_calendar_occurrence_deleted",
                now,
            )
            .await?
        } else {
            ImportOutcome::Unchanged
        };
        let revision = current_item_revision(transaction, scope.workspace_id, local_id).await?;
        update_calendar_occurrence_mapping(
            transaction,
            mapping_id,
            change,
            local_id,
            revision,
            forced_sensitive,
            generation,
            "deleted_remote",
            now,
        )
        .await?;
        return Ok(outcome);
    };
    let mut input = input.clone();
    if forced_sensitive {
        input.is_sensitive = true;
        "Busy".clone_into(&mut input.title);
        input.notes = None;
    }
    if mapped_item_exists {
        let local_id = local_id.ok_or(GoogleSyncRepositoryError::Internal)?;
        input.id = local_id;
    } else if local_id == Some(input.id) {
        // A hard-deleted item's immutable delta history can still occupy its
        // old UUID. The current complete projection supplies no reason to
        // reuse that stale canonical identity.
        input.id = Uuid::new_v4();
    }
    let candidate =
        Item::new(input, now).map_err(|_| GoogleSyncRepositoryError::InvalidProjectionBatch)?;
    if !mapped_item_exists {
        insert_imported_item(transaction, scope, &candidate).await?;
        record_import_mutation(
            transaction,
            scope,
            candidate.id,
            u64_to_i64(candidate.revision)?,
            "upsert",
            serde_json::to_value(&candidate).map_err(internal)?,
            "item.google_calendar_occurrence_created",
            None,
            now,
        )
        .await?;
        update_calendar_occurrence_mapping(
            transaction,
            mapping_id,
            change,
            Some(candidate.id),
            Some(u64_to_i64(candidate.revision)?),
            candidate.is_sensitive || forced_sensitive,
            generation,
            "synced",
            now,
        )
        .await?;
        return Ok(ImportOutcome::Created);
    }
    let local_id = local_id.ok_or(GoogleSyncRepositoryError::Internal)?;
    let current = current.ok_or(GoogleSyncRepositoryError::Internal)?;
    let expected = local_revision.ok_or(GoogleSyncRepositoryError::Internal)?;
    let local_changed = u64_to_i64(current.revision)? != expected;
    if local_changed && !candidate.is_sensitive && !forced_sensitive {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    if !local_changed
        && current.deleted_at.is_none()
        && old_projection_hash.as_deref() == Some(change.remote_projection_hash.as_slice())
    {
        update_calendar_occurrence_mapping(
            transaction,
            mapping_id,
            change,
            Some(local_id),
            Some(expected),
            current.is_sensitive || forced_sensitive,
            generation,
            "synced",
            now,
        )
        .await?;
        return Ok(ImportOutcome::Unchanged);
    }
    let restored = current.deleted_at.is_some();
    let base_revision = u64_to_i64(current.revision)?;
    let replacement = ReplaceItem {
        is_sensitive: current.is_sensitive || candidate.is_sensitive || forced_sensitive,
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
    let mut updated = current
        .replaced(replacement, now)
        .map_err(|_| GoogleSyncRepositoryError::Internal)?;
    if restored {
        updated.deleted_at = None;
    }
    reject_google_close_for_active_execution(transaction, scope.workspace_id, &current, &updated)
        .await?;
    update_imported_item(transaction, scope.workspace_id, &updated).await?;
    record_import_mutation(
        transaction,
        scope,
        updated.id,
        u64_to_i64(updated.revision)?,
        "upsert",
        serde_json::to_value(&updated).map_err(internal)?,
        if restored {
            "item.google_calendar_occurrence_restored"
        } else {
            "item.google_calendar_occurrence_updated"
        },
        Some(base_revision),
        now,
    )
    .await?;
    update_calendar_occurrence_mapping(
        transaction,
        mapping_id,
        change,
        Some(updated.id),
        Some(u64_to_i64(updated.revision)?),
        updated.is_sensitive || forced_sensitive,
        generation,
        "synced",
        now,
    )
    .await?;
    Ok(ImportOutcome::Updated)
}

async fn insert_calendar_occurrence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    change: &RemoteItemChange,
    generation: u64,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let candidate = change
        .item
        .as_ref()
        .map(|input| Item::new(input.clone(), now))
        .transpose()
        .map_err(|_| GoogleSyncRepositoryError::InvalidProjectionBatch)?;
    if let Some(item) = &candidate {
        insert_imported_item(transaction, scope, item).await?;
        record_import_mutation(
            transaction,
            scope,
            item.id,
            u64_to_i64(item.revision)?,
            "upsert",
            serde_json::to_value(item).map_err(internal)?,
            "item.google_calendar_occurrence_created",
            None,
            now,
        )
        .await?;
    }
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, collection_id, \
         entity_kind, local_entity_id, remote_resource_id, remote_etag, remote_updated_at, \
         remote_parent_id, remote_payload_hash, remote_projection_hash, local_revision, sync_state, \
         ownership, projection_generation, provider_forced_sensitive, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'calendar_occurrence', $5, $6, $7, $8, $9, $10, $11, \
         $12, $13, 'external', $14, $15, $16, $16)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(candidate.as_ref().map(|item| item.id))
    .bind(&change.remote_id)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(&change.remote_parent_id)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(
        candidate
            .as_ref()
            .map(|item| u64_to_i64(item.revision))
            .transpose()?,
    )
    .bind(if candidate.is_some() {
        "synced"
    } else {
        "deleted_remote"
    })
    .bind(u64_to_i64(generation)?)
    .bind(candidate.as_ref().is_some_and(|item| item.is_sensitive))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(if candidate.is_some() {
        ImportOutcome::Created
    } else {
        ImportOutcome::Unchanged
    })
}

#[allow(clippy::too_many_arguments)] // Exact occurrence mapping replacement is one durable fence.
async fn update_calendar_occurrence_mapping(
    transaction: &mut Transaction<'_, Postgres>,
    mapping_id: Uuid,
    change: &RemoteItemChange,
    local_id: Option<Uuid>,
    local_revision: Option<i64>,
    provider_forced_sensitive: bool,
    generation: u64,
    state: &'static str,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    sqlx::query(
        "UPDATE provider_sync_mappings SET local_entity_id = $2, local_revision = $3, \
         remote_etag = $4, remote_updated_at = $5, remote_parent_id = $6, \
         remote_payload_hash = $7, remote_projection_hash = $8, sync_state = $9, \
         projection_generation = $10, provider_forced_sensitive = $11, \
         conflict_metadata = NULL, updated_at = $12 WHERE id = $1",
    )
    .bind(mapping_id)
    .bind(local_id)
    .bind(local_revision)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(&change.remote_parent_id)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(state)
    .bind(u64_to_i64(generation)?)
    .bind(provider_forced_sensitive)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn update_dangling_calendar_occurrence_tombstone(
    transaction: &mut Transaction<'_, Postgres>,
    mapping_id: Uuid,
    change: &RemoteItemChange,
    generation: u64,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    // Do not rewrite local_entity_id or provider_forced_sensitive here. A
    // deleted private occurrence has no provider payload from which to safely
    // reconstruct a sensitive canonical item, and the database deliberately
    // makes that privacy floor monotonic. The next live complete projection
    // can recreate the item and rebind this same mapping.
    sqlx::query(
        "UPDATE provider_sync_mappings SET local_revision = NULL, remote_etag = $2, \
         remote_updated_at = $3, remote_parent_id = $4, remote_payload_hash = $5, \
         remote_projection_hash = $6, sync_state = 'deleted_remote', \
         projection_generation = $7, conflict_metadata = NULL, updated_at = $8 WHERE id = $1",
    )
    .bind(mapping_id)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(&change.remote_parent_id)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(u64_to_i64(generation)?)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn current_item_revision(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Option<Uuid>,
) -> Result<Option<i64>, GoogleSyncRepositoryError> {
    let Some(item_id) = item_id else {
        return Ok(None);
    };
    sqlx::query_scalar("SELECT revision FROM items WHERE workspace_id = $1 AND id = $2")
        .bind(workspace_id)
        .bind(item_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(internal)
}

async fn retire_calendar_occurrence_mapping(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    mapping: &PgRow,
    generation: u64,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let mapping_id: Uuid = mapping.try_get("id").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    let current_revision = current_item_revision(transaction, scope.workspace_id, local_id).await?;
    let outcome = if let (Some(local_id), Some(_)) = (local_id, current_revision) {
        trash_projected_item(
            transaction,
            scope,
            local_id,
            local_revision,
            "item.google_calendar_occurrence_absent",
            now,
        )
        .await?
    } else {
        ImportOutcome::Unchanged
    };
    let revision = current_item_revision(transaction, scope.workspace_id, local_id).await?;
    sqlx::query(
        "UPDATE provider_sync_mappings SET local_revision = $2, sync_state = 'deleted_remote', \
         projection_generation = $3, conflict_metadata = NULL, updated_at = $4 WHERE id = $1",
    )
    .bind(mapping_id)
    .bind(revision)
    .bind(u64_to_i64(generation)?)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(outcome)
}

/// Removes active canonical occurrence projections while retaining their
/// provider mapping identities for a later full refresh. The caller must lock
/// `execution_state` and then the workspace canonical-item advisory space
/// before any account/collection row. An occurrence targeted by the open
/// execution lease is detached as a conflict instead of blocking an authority
/// or configuration fence or silently trashing the running item.
pub(crate) async fn retire_active_calendar_occurrences(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    collection_id: Uuid,
    now: DateTime<Utc>,
) -> Result<SyncCounts, GoogleSyncRepositoryError> {
    let active_item_id = google_active_execution(transaction, scope.workspace_id)
        .await?
        .map(|(_, item_id)| item_id);
    let rows = sqlx::query(
        "SELECT mapping.id, mapping.local_entity_id, mapping.local_revision, item.revision \
         FROM provider_sync_mappings mapping JOIN items item \
           ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
         WHERE mapping.workspace_id = $1 AND mapping.provider_account_id = $2 \
           AND mapping.collection_id = $3 AND mapping.entity_kind = 'calendar_occurrence' \
           AND mapping.tombstoned_at IS NULL AND item.trashed_at IS NULL \
         ORDER BY item.id FOR UPDATE OF mapping, item",
    )
    .bind(scope.workspace_id)
    .bind(account_id)
    .bind(collection_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    let mut counts = SyncCounts::default();
    for row in rows {
        let mapping_id: Uuid = row.try_get("id").map_err(internal)?;
        let item_id: Uuid = row.try_get("local_entity_id").map_err(internal)?;
        let imported_revision: Option<i64> = row.try_get("local_revision").map_err(internal)?;
        let current_revision: i64 = row.try_get("revision").map_err(internal)?;
        if active_item_id == Some(item_id) {
            sqlx::query(
                "UPDATE provider_sync_mappings SET sync_state = 'conflict', \
                 conflict_metadata = jsonb_build_object( \
                   'reason', 'calendar_occurrence_configuration_retired_execution_active', \
                   'local_item_id', $2, 'mapping_local_revision', $3, \
                   'item_revision', $4), tombstoned_at = $5, updated_at = $5 \
                 WHERE id = $1",
            )
            .bind(mapping_id)
            .bind(item_id)
            .bind(imported_revision)
            .bind(current_revision)
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(internal)?;
            counts.add(ImportOutcome::Conflict);
            continue;
        }
        if imported_revision != Some(current_revision) {
            // Configuration teardown must never turn a locally edited provider
            // projection into a tombstone. Retire the provider association so
            // it no longer participates in the Calendar safety fence, while
            // retaining both the canonical fork and the historical mapping.
            // Tombstoning also safely releases a provider sensitivity floor
            // without weakening the preserved item's current sensitivity.
            sqlx::query(
                "UPDATE provider_sync_mappings SET sync_state = 'conflict', \
                 conflict_metadata = jsonb_build_object( \
                   'reason', 'calendar_occurrence_configuration_retired_local_changed', \
                   'local_item_id', $2, 'mapping_local_revision', $3, \
                   'item_revision', $4), tombstoned_at = $5, updated_at = $5 \
                 WHERE id = $1",
            )
            .bind(mapping_id)
            .bind(item_id)
            .bind(imported_revision)
            .bind(current_revision)
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(internal)?;
            counts.add(ImportOutcome::Conflict);
            continue;
        }
        counts.add(
            trash_projected_item(
                transaction,
                scope,
                item_id,
                imported_revision,
                "item.google_calendar_occurrence_retired_for_configuration",
                now,
            )
            .await?,
        );
        let new_revision: i64 =
            sqlx::query_scalar("SELECT revision FROM items WHERE workspace_id = $1 AND id = $2")
                .bind(scope.workspace_id)
                .bind(item_id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(internal)?;
        sqlx::query(
            "UPDATE provider_sync_mappings SET local_revision = $2, sync_state = 'pending_pull', \
             conflict_metadata = NULL, updated_at = $3 WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(new_revision)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    }
    Ok(counts)
}

/// Account-wide companion for OAuth pause/revocation transactions. Callers
/// must lock `execution_state`, then the canonical-item advisory space, and
/// only then provider/account rows before disabling the account.
pub(crate) async fn retire_active_calendar_occurrences_for_account(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    now: DateTime<Utc>,
) -> Result<SyncCounts, GoogleSyncRepositoryError> {
    sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'uninitialized', \
         planning_collection_revision = NULL, planning_window_start = NULL, \
         planning_window_end = NULL, planning_window_refreshed_at = NULL, \
         planning_last_error_code = NULL, updated_at = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
           AND collection_kind = 'calendar'",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    let collection_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM google_sync_collections WHERE workspace_id = $1 AND user_id = $2 \
         AND provider_account_id = $3 AND collection_kind = 'calendar' ORDER BY id FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    let mut counts = SyncCounts::default();
    for collection_id in collection_ids {
        counts.merge(
            &retire_active_calendar_occurrences(transaction, scope, account_id, collection_id, now)
                .await?,
        );
    }
    Ok(counts)
}

async fn trash_projected_item(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_id: Uuid,
    expected_revision: Option<i64>,
    event: &'static str,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let current = fetch_import_item(transaction, scope.workspace_id, item_id).await?;
    let expected = expected_revision.ok_or(GoogleSyncRepositoryError::Internal)?;
    if u64_to_i64(current.revision)? != expected {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    if current.deleted_at.is_some() {
        return Ok(ImportOutcome::Unchanged);
    }
    let deleted = current
        .trashed(now)
        .map_err(|_| GoogleSyncRepositoryError::Internal)?;
    reject_google_close_for_active_execution(transaction, scope.workspace_id, &current, &deleted)
        .await?;
    update_imported_item(transaction, scope.workspace_id, &deleted).await?;
    let tombstone = ItemTombstone {
        id: deleted.id,
        revision: deleted.revision,
        deleted_at: now,
        parent_id: deleted.parent_id,
    };
    record_import_mutation(
        transaction,
        scope,
        deleted.id,
        u64_to_i64(deleted.revision)?,
        "tombstone",
        serde_json::to_value(tombstone).map_err(internal)?,
        event,
        Some(expected),
        now,
    )
    .await?;
    Ok(ImportOutcome::Deleted)
}

#[allow(clippy::too_many_lines)] // Metadata retirement and owned recovery share one mapping fence.
async fn apply_calendar_series_metadata_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    claim: &SyncClaim,
    change: &RemoteCalendarSeriesChange,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let mapping = sqlx::query(
        "SELECT id, local_entity_id, local_revision, ownership, sync_state, remote_payload_hash \
         FROM provider_sync_mappings WHERE workspace_id = $1 AND provider_account_id = $2 \
           AND collection_id = $3 AND entity_kind = 'item' AND remote_resource_id = $4 \
           AND tombstoned_at IS NULL FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(&change.remote_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(mapping) = mapping else {
        if change.dayweave_item_id.is_some() {
            if !change.deleted {
                let recovery = RemoteItemChange {
                    account_id: change.account_id,
                    collection_id: change.collection_id,
                    collection_revision: change.collection_revision,
                    dayweave_item_id: change.dayweave_item_id,
                    remote_id: change.remote_id.clone(),
                    remote_parent_id: None,
                    remote_etag: change.remote_etag.clone(),
                    remote_updated_at: change.remote_updated_at,
                    remote_payload_hash: change.remote_payload_hash,
                    remote_projection_hash: change.remote_projection_hash,
                    reviewed_provider_projection: change.reviewed_provider_projection.clone(),
                    item: None,
                };
                if recover_dayweave_mapping(transaction, scope, claim, &recovery, true, now)
                    .await?
                    .is_some()
                {
                    return Ok(ImportOutcome::Unchanged);
                }
            }
            sqlx::query(
                "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
                 collection_id, entity_kind, remote_resource_id, remote_etag, remote_updated_at, \
                 remote_payload_hash, remote_projection_hash, sync_state, ownership, \
                 conflict_metadata, created_at, updated_at) VALUES ($1, $2, $3, $4, 'item', $5, \
                 $6, $7, $8, $9, 'conflict', 'external', \
                 '{\"reason\":\"unrecognized_or_changed_dayweave_marker\"}'::jsonb, $10, $10)",
            )
            .bind(Uuid::new_v4())
            .bind(scope.workspace_id)
            .bind(change.account_id)
            .bind(change.collection_id)
            .bind(&change.remote_id)
            .bind(&change.remote_etag)
            .bind(change.remote_updated_at)
            .bind(change.remote_payload_hash.as_slice())
            .bind(change.remote_projection_hash.as_slice())
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(internal)?;
            mark_calendar_projection_failed(
                transaction,
                scope,
                change.account_id,
                change.collection_id,
                change.collection_revision,
                "owned_provider_conflict",
                now,
            )
            .await?;
            return Ok(ImportOutcome::Conflict);
        }
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
             collection_id, entity_kind, remote_resource_id, remote_etag, remote_updated_at, \
             remote_payload_hash, remote_projection_hash, sync_state, ownership, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, $8, $9, $10, \
             'external', $11, $11)",
        )
        .bind(Uuid::new_v4())
        .bind(scope.workspace_id)
        .bind(change.account_id)
        .bind(change.collection_id)
        .bind(&change.remote_id)
        .bind(&change.remote_etag)
        .bind(change.remote_updated_at)
        .bind(change.remote_payload_hash.as_slice())
        .bind(change.remote_projection_hash.as_slice())
        .bind(if change.deleted {
            "deleted_remote"
        } else {
            "synced"
        })
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        return Ok(ImportOutcome::Unchanged);
    };
    let mapping_id: Uuid = mapping.try_get("id").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    let ownership: String = mapping.try_get("ownership").map_err(internal)?;
    let mapping_state: String = mapping.try_get("sync_state").map_err(internal)?;
    let old_hash: Option<Vec<u8>> = mapping.try_get("remote_payload_hash").map_err(internal)?;
    if ownership == "dayweave" {
        let marker_matches_or_is_absent = change
            .dayweave_item_id
            .is_none_or(|item_id| Some(item_id) == local_id);
        if change.deleted
            && marker_matches_or_is_absent
            && mapping_state == "deleted_remote"
            && deleted_owned_item_matches(transaction, scope.workspace_id, local_id, local_revision)
                .await?
        {
            sqlx::query(
                "UPDATE provider_sync_mappings SET remote_etag = $2, remote_updated_at = $3, \
                 remote_payload_hash = $4, remote_projection_hash = $5, \
                 sync_state = 'deleted_remote', conflict_metadata = NULL, updated_at = $6 \
                 WHERE id = $1",
            )
            .bind(mapping_id)
            .bind(&change.remote_etag)
            .bind(change.remote_updated_at)
            .bind(change.remote_payload_hash.as_slice())
            .bind(change.remote_projection_hash.as_slice())
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(internal)?;
            return Ok(ImportOutcome::Unchanged);
        }
        if !marker_matches_or_is_absent || (!change.deleted && change.dayweave_item_id != local_id)
        {
            return Err(GoogleSyncRepositoryError::CursorConflict);
        }
        let state = if change.deleted
            || old_hash.as_deref() != Some(change.remote_payload_hash.as_slice())
        {
            "conflict"
        } else {
            "synced"
        };
        sqlx::query(
            "UPDATE provider_sync_mappings SET remote_etag = $2, remote_updated_at = $3, \
             remote_payload_hash = $4, remote_projection_hash = $5, sync_state = $6, \
             conflict_metadata = CASE WHEN $6 = 'conflict' \
               THEN '{\"reason\":\"provider_changed_dayweave_owned_item\"}'::jsonb \
               ELSE NULL END, updated_at = $7 WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(&change.remote_etag)
        .bind(change.remote_updated_at)
        .bind(change.remote_payload_hash.as_slice())
        .bind(change.remote_projection_hash.as_slice())
        .bind(state)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        if state == "conflict" {
            mark_calendar_projection_failed(
                transaction,
                scope,
                change.account_id,
                change.collection_id,
                change.collection_revision,
                "owned_provider_conflict",
                now,
            )
            .await?;
        }
        return Ok(if state == "synced" {
            ImportOutcome::Unchanged
        } else {
            ImportOutcome::Conflict
        });
    }
    if change.dayweave_item_id.is_some() {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    if let Some(local_id) = local_id
        && has_active_dayweave_mapping(transaction, scope.workspace_id, local_id).await?
    {
        // Migration 0014 deliberately preserves a canonical item when a
        // corrupt/legacy external Calendar mapping shadows an active
        // DayWeave-owned mapping. The metadata lane must finish that repair by
        // detaching only the external shadow; retiring the shared item would
        // delete the user's owned block on the first post-upgrade sync.
        sqlx::query(
            "UPDATE provider_sync_mappings SET local_entity_id = NULL, local_revision = NULL, \
             remote_etag = $2, remote_updated_at = $3, remote_payload_hash = $4, \
             remote_projection_hash = $5, sync_state = $6, conflict_metadata = NULL, \
             updated_at = $7 WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(&change.remote_etag)
        .bind(change.remote_updated_at)
        .bind(change.remote_payload_hash.as_slice())
        .bind(change.remote_projection_hash.as_slice())
        .bind(if change.deleted {
            "deleted_remote"
        } else {
            "synced"
        })
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        return Ok(ImportOutcome::Unchanged);
    }
    let outcome = if let Some(local_id) = local_id {
        trash_projected_item(
            transaction,
            scope,
            local_id,
            local_revision,
            "item.google_calendar_legacy_projection_retired",
            now,
        )
        .await?
    } else {
        ImportOutcome::Unchanged
    };
    let revision = current_item_revision(transaction, scope.workspace_id, local_id).await?;
    sqlx::query(
        "UPDATE provider_sync_mappings SET local_revision = $2, remote_etag = $3, \
         remote_updated_at = $4, remote_payload_hash = $5, remote_projection_hash = $6, \
         sync_state = $7, conflict_metadata = NULL, updated_at = $8 WHERE id = $1",
    )
    .bind(mapping_id)
    .bind(revision)
    .bind(&change.remote_etag)
    .bind(change.remote_updated_at)
    .bind(change.remote_payload_hash.as_slice())
    .bind(change.remote_projection_hash.as_slice())
    .bind(if change.deleted {
        "deleted_remote"
    } else {
        "synced"
    })
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(outcome)
}

async fn has_active_dayweave_mapping(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<bool, GoogleSyncRepositoryError> {
    // The canonical-item advisory lock is already held by the caller, and the
    // row lock makes the dependency explicit for direct SQL maintenance paths.
    let mapping = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
         AND local_entity_id = $2 AND ownership = 'dayweave' \
         AND tombstoned_at IS NULL ORDER BY id LIMIT 1 FOR SHARE",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(mapping.is_some())
}

#[allow(clippy::too_many_arguments)]
async fn mark_calendar_projection_failed(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    collection_id: Uuid,
    collection_revision: u64,
    code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), GoogleSyncRepositoryError> {
    let affected = sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'failed', \
         planning_collection_revision = NULL, planning_window_start = NULL, \
         planning_window_end = NULL, planning_window_refreshed_at = NULL, \
         planning_last_error_code = $6, updated_at = $7 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
           AND id = $4 AND revision = $5",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(collection_id)
    .bind(u64_to_i64(collection_revision)?)
    .bind(code)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if affected != 1 {
        return Err(GoogleSyncRepositoryError::CursorConflict);
    }
    Ok(())
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
           AND run.state = 'running' AND run.claim_id = $4 \
           AND run.claim_generation = $7 AND run.lease_until > $6 \
           AND collection.id = $5 FOR SHARE OF account, run, collection",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(claim.account_id)
    .bind(claim.claim_id)
    .bind(collection_id)
    .bind(now)
    .bind(u64_to_i64(claim.claim_generation)?)
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
    claim: &SyncClaim,
    change: &RemoteItemChange,
    mapping: Option<&PgRow>,
    now: DateTime<Utc>,
) -> Result<ImportOutcome, GoogleSyncRepositoryError> {
    let Some(mapping) = mapping else {
        if let Some((local_id, local_revision)) =
            recover_dayweave_mapping(transaction, scope, claim, change, false, now).await?
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
    let old_etag: Option<String> = mapping.try_get("remote_etag").map_err(internal)?;
    let mapping_state: String = mapping.try_get("sync_state").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    if local_id.is_none() && change.dayweave_item_id.is_some() {
        if recover_dayweave_mapping(transaction, scope, claim, change, false, now)
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
        if old_etag != change.remote_etag
            && let Some(local_id) = local_id
        {
            conflict_active_outbox(
                transaction,
                scope,
                change.account_id,
                change.collection_id,
                local_id,
                "provider_version_changed_during_delivery",
                now,
            )
            .await?;
        }
        return Ok(ImportOutcome::Unchanged);
    }
    if let Some(local_id) = local_id
        && has_active_dayweave_mapping(transaction, scope.workspace_id, local_id).await?
    {
        // A cursorless post-upgrade sweep can observe an external legacy
        // shadow as absent before the metadata lane has detached it. Retire
        // only that provider shadow; the shared DayWeave-owned canonical item
        // is authoritative and must never be treated as an external deletion.
        sqlx::query(
            "UPDATE provider_sync_mappings SET local_entity_id = NULL, local_revision = NULL, \
             remote_etag = $2, remote_updated_at = $3, remote_parent_id = $4, \
             remote_payload_hash = $5, remote_projection_hash = $6, \
             sync_state = 'deleted_remote', conflict_metadata = NULL, updated_at = $7 \
             WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(&change.remote_etag)
        .bind(change.remote_updated_at)
        .bind(&change.remote_parent_id)
        .bind(change.remote_payload_hash.as_slice())
        .bind(change.remote_projection_hash.as_slice())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
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
        reject_google_item_for_active_execution(transaction, scope.workspace_id, local_id).await?;
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
    claim: &SyncClaim,
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
            if recover_dayweave_mapping(transaction, scope, claim, &change, true, now)
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
    let old_etag: Option<String> = mapping.try_get("remote_etag").map_err(internal)?;
    let old_hash: Option<Vec<u8>> = mapping.try_get("remote_payload_hash").map_err(internal)?;
    let old_projection_hash: Option<Vec<u8>> = mapping
        .try_get("remote_projection_hash")
        .map_err(internal)?;
    let mapping_state: String = mapping.try_get("sync_state").map_err(internal)?;
    let local_id: Option<Uuid> = mapping.try_get("local_entity_id").map_err(internal)?;
    let local_revision: Option<i64> = mapping.try_get("local_revision").map_err(internal)?;
    if local_id.is_none() && change.dayweave_item_id.is_some() {
        if recover_dayweave_mapping(transaction, scope, claim, &change, true, now)
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
            if old_etag != change.remote_etag
                && let Some(local_id) = local_id
            {
                // Even a semantically unchanged provider record received a
                // new conditional-write version. Revoke any in-flight permit
                // approved against the former ETag in this same transaction.
                conflict_active_outbox(
                    transaction,
                    scope,
                    change.account_id,
                    change.collection_id,
                    local_id,
                    "provider_version_changed_during_delivery",
                    now,
                )
                .await?;
            }
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
        // Provider refreshes may promote privacy but never declassify an item. Only an explicit
        // first-party edit can clear the canonical flag.
        is_sensitive: current.is_sensitive || candidate.is_sensitive,
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
    reject_google_close_for_active_execution(transaction, scope.workspace_id, &current, &updated)
        .await?;
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
    claim: &SyncClaim,
    change: &RemoteItemChange,
    live_resource: bool,
    now: DateTime<Utc>,
) -> Result<Option<(Uuid, i64)>, GoogleSyncRepositoryError> {
    let Some(item_id) = change.dayweave_item_id else {
        return Ok(None);
    };
    if !live_resource
        || change.remote_etag.is_none()
        || change.reviewed_provider_projection.is_none()
        || claim.account_id != change.account_id
    {
        // Tombstones and partial representations cannot prove the complete
        // reviewed create. A live object without an ETag also cannot safely
        // become DayWeave-owned for the next conditional mutation.
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT outbox.id, outbox.item_revision, outbox.entity_kind, outbox.payload, \
           outbox.remote_resource_id, outbox.expected_etag \
         FROM google_sync_outbox outbox \
         JOIN items item \
           ON item.workspace_id = outbox.workspace_id AND item.id = outbox.item_id \
         JOIN google_outbound_previews approval ON approval.workspace_id = outbox.workspace_id \
           AND approval.id = outbox.approval_id \
         JOIN google_sync_runs run ON run.workspace_id = outbox.workspace_id \
           AND run.user_id = outbox.user_id \
           AND run.provider_account_id = outbox.provider_account_id \
         WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
           AND outbox.provider_account_id = $3 AND outbox.collection_id = $4 \
           AND outbox.item_id = $5 AND outbox.operation = 'upsert' AND outbox.app_owned \
           AND outbox.entity_kind = 'calendar_event' AND item.trashed_at IS NULL \
           AND outbox.remote_resource_id IS NULL AND outbox.expected_etag IS NULL \
           AND outbox.payload->>'id' = $9 \
           AND (outbox.state IN ('pending', 'delivering', 'backoff', 'superseded') \
             OR (outbox.state = 'conflict' \
               AND outbox.last_error_code = 'provider_identity_unresolved')) \
           AND approval.approved_at IS NOT NULL AND approval.consumed_at IS NOT NULL \
           AND approval.outbox_id = outbox.id AND approval.intent_hash = outbox.intent_hash \
           AND approval.provider_account_id = outbox.provider_account_id \
           AND approval.collection_id = outbox.collection_id \
           AND approval.collection_revision = outbox.collection_revision \
           AND approval.collection_remote_id = outbox.target_remote_collection_id \
           AND approval.item_id = outbox.item_id \
           AND approval.item_revision = outbox.item_revision \
           AND approval.entity_kind = outbox.entity_kind AND approval.operation = outbox.operation \
           AND approval.required_scope = outbox.required_scope AND approval.payload = outbox.payload \
           AND approval.provider_resource_id IS NULL AND approval.expected_etag IS NULL \
           AND run.state = 'running' AND run.claim_id = $6 AND run.claim_generation = $7 \
           AND run.lease_until > $8 \
           AND (outbox.state <> 'delivering' \
             OR (outbox.run_claim_id = run.claim_id \
               AND outbox.run_claim_generation = run.claim_generation)) \
         ORDER BY outbox.item_revision DESC, outbox.created_at DESC, outbox.id DESC \
         FOR UPDATE OF outbox, item, approval, run",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(item_id)
    .bind(claim.claim_id)
    .bind(u64_to_i64(claim.claim_generation)?)
    .bind(now)
    .bind(&change.remote_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(latest_row) = row.first() else {
        return Ok(None);
    };
    let reviewed_projection = change
        .reviewed_provider_projection
        .as_ref()
        .ok_or(GoogleSyncRepositoryError::Internal)?;
    let exact_row = row.iter().find(|candidate| {
        candidate.try_get::<Value, _>("payload").ok().as_ref() == Some(reviewed_projection)
    });
    let row = exact_row.unwrap_or(latest_row);
    let outbox_id: Uuid = row.try_get("id").map_err(internal)?;
    let local_revision: i64 = row.try_get("item_revision").map_err(internal)?;
    let payload: Value = row.try_get("payload").map_err(internal)?;
    let identity_matches =
        payload.get("id").and_then(Value::as_str) == Some(change.remote_id.as_str());
    if !identity_matches {
        return Ok(None);
    }
    let mappings = sqlx::query(
        "SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
         AND provider_account_id = $2 AND collection_id = $3 AND entity_kind = 'item' \
         AND tombstoned_at IS NULL AND (local_entity_id = $4 OR remote_resource_id = $5) \
         ORDER BY id FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(change.account_id)
    .bind(change.collection_id)
    .bind(item_id)
    .bind(&change.remote_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    if !mappings.is_empty() || exact_row.is_none() {
        // A proof-bearing provider object is not sufficient: every reviewed
        // semantic field must still match. Never upgrade a provider edit to
        // DayWeave ownership merely because the authenticated ID survived.
        sqlx::query(
            "UPDATE google_sync_outbox SET state = 'conflict', claim_id = NULL, \
             claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
             dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
             last_error_code = 'provider_semantics_changed_before_recovery', updated_at = $2 \
             WHERE id = $1",
        )
        .bind(outbox_id)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        return Ok(None);
    }
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
    let published = sqlx::query(
        "UPDATE google_sync_outbox SET state = 'published', claim_id = NULL, claimed_at = NULL, \
         run_claim_id = NULL, run_claim_generation = NULL, dispatch_nonce = NULL, \
         dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
         last_error_code = NULL, updated_at = $2 WHERE id = $1 \
           AND remote_resource_id IS NULL AND expected_etag IS NULL",
    )
    .bind(outbox_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if published != 1 {
        return Err(GoogleSyncRepositoryError::ClaimLost);
    }
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
    .bind(json!({"collection_id": change.collection_id}))
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
         run_claim_id = NULL, run_claim_generation = NULL, dispatch_nonce = NULL, \
         dispatch_authorized_at = NULL, dispatch_expires_at = NULL, available_at = $7, \
         last_error_code = CASE WHEN entity_kind = 'task' AND operation = 'upsert' \
                                     AND remote_resource_id IS NULL \
                                     AND provider_post_may_have_started \
                                THEN 'provider_identity_unresolved' ELSE $6 END, updated_at = $7 \
         WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
           AND collection_id = $4 AND item_id = $5 \
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
        "UPDATE google_sync_outbox outbox SET \
         state = CASE WHEN outbox.entity_kind = 'task' AND outbox.operation = 'upsert' \
                           AND outbox.remote_resource_id IS NULL \
                           AND outbox.provider_post_may_have_started \
                      THEN 'conflict' ELSE $5 END, \
         claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
         dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
         provider_post_may_have_started = outbox.entity_kind = 'task' \
           AND outbox.operation = 'upsert' AND outbox.remote_resource_id IS NULL \
           AND outbox.provider_post_may_have_started, \
         send_started_at = CASE WHEN outbox.entity_kind = 'task' AND outbox.operation = 'upsert' \
                                     AND outbox.remote_resource_id IS NULL \
                                     AND outbox.provider_post_may_have_started \
                                THEN outbox.send_started_at ELSE NULL END, \
         attempts = attempts + 1, available_at = $6, \
         last_error_code = CASE WHEN outbox.entity_kind = 'task' AND outbox.operation = 'upsert' \
                                     AND outbox.remote_resource_id IS NULL \
                                     AND outbox.provider_post_may_have_started \
                                THEN 'provider_identity_unresolved' ELSE $7 END, updated_at = $6 \
         FROM google_sync_runs run WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
           AND outbox.id = $3 AND outbox.provider_account_id = $4 \
           AND outbox.state = 'delivering' AND outbox.claim_id = $8 \
           AND outbox.run_claim_id = $9 AND outbox.run_claim_generation = $10 \
           AND run.workspace_id = outbox.workspace_id AND run.user_id = outbox.user_id \
           AND run.provider_account_id = outbox.provider_account_id \
           AND run.claim_id = $9 AND run.claim_generation = $10",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(work.id)
    .bind(work.account_id)
    .bind(state)
    .bind(now)
    .bind(code)
    .bind(work.claim_id)
    .bind(work.run_claim_id)
    .bind(u64_to_i64(work.run_claim_generation)?)
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
        "INSERT INTO items (id, workspace_id, created_by_user_id, is_sensitive, kind, status, title, notes, \
         timezone_name, duration_seconds, deadline_at, earliest_start_at, recurrence, \
         scheduling_constraints, split_allowed, minimum_chunk_seconds, maximum_chunk_seconds, \
         importance, urgency, sibling_order, revision, created_at, updated_at, completed_at, trashed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
         $16, $17, $18, $19, $20, $21, $22, $23, $24, $25)",
    )
    .bind(item.id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(item.is_sensitive)
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
        "UPDATE items SET is_sensitive = $3, kind = $4, status = $5, title = $6, notes = $7, timezone_name = $8, \
         duration_seconds = $9, deadline_at = $10, earliest_start_at = $11, recurrence = $12, \
         scheduling_constraints = $13, split_allowed = $14, minimum_chunk_seconds = $15, \
         maximum_chunk_seconds = $16, importance = $17, urgency = $18, revision = $19, \
         updated_at = $20, completed_at = $21, trashed_at = $22 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(item.id)
    .bind(item.is_sensitive)
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

async fn reject_google_close_for_active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    current: &Item,
    replacement: &Item,
) -> Result<(), GoogleSyncRepositoryError> {
    let becomes_terminal = !current.status.is_terminal() && replacement.status.is_terminal();
    let becomes_trashed = current.deleted_at.is_none() && replacement.deleted_at.is_some();
    if !becomes_terminal && !becomes_trashed {
        return Ok(());
    }
    reject_google_item_for_active_execution(transaction, workspace_id, current.id).await
}

async fn reject_google_item_for_active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<(), GoogleSyncRepositoryError> {
    let Some((_session_id, active_item_id)) =
        google_active_execution(transaction, workspace_id).await?
    else {
        return Ok(());
    };
    if active_item_id == item_id {
        return Err(GoogleSyncRepositoryError::ItemExecutionActive);
    }
    Ok(())
}

async fn google_active_execution(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, GoogleSyncRepositoryError> {
    let active_session_id: Option<Uuid> =
        sqlx::query_scalar("SELECT active_session_id FROM execution_state WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(internal)?
            .flatten();
    let Some(session_id) = active_session_id else {
        return Ok(None);
    };
    let row = sqlx::query(
        "SELECT item_id, state FROM execution_sessions WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace_id)
    .bind(session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleSyncRepositoryError::Internal)?;
    let state: String = row.try_get("state").map_err(internal)?;
    if !matches!(state.as_str(), "active" | "paused") {
        return Err(GoogleSyncRepositoryError::Internal);
    }
    let active_item_id: Uuid = row.try_get("item_id").map_err(internal)?;
    Ok(Some((session_id, active_item_id)))
}

async fn fetch_import_item(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<Item, GoogleSyncRepositoryError> {
    fetch_import_item_optional(transaction, workspace_id, item_id)
        .await?
        .ok_or(GoogleSyncRepositoryError::ItemNotFound)
}

async fn fetch_import_item_optional(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<Option<Item>, GoogleSyncRepositoryError> {
    let row = sqlx::query(
        "SELECT item.id, item.is_sensitive, item.kind, item.status, item.title, item.notes, item.timezone_name, \
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
    .map_err(internal)?;
    row.as_ref().map(item_from_row).transpose()
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
        is_sensitive: row.try_get("is_sensitive").map_err(internal)?,
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
        calendar_policy: GoogleCalendarPolicy {
            confirmed_busy: parse_event_disposition(
                &row.try_get::<String, _>("confirmed_busy_policy")
                    .map_err(internal)?,
            )?,
            tentative: parse_event_disposition(
                &row.try_get::<String, _>("tentative_policy")
                    .map_err(internal)?,
            )?,
            free: parse_event_disposition(
                &row.try_get::<String, _>("free_policy").map_err(internal)?,
            )?,
            all_day: parse_event_disposition(
                &row.try_get::<String, _>("all_day_policy")
                    .map_err(internal)?,
            )?,
            publish_all_day: row.try_get("publish_all_day").map_err(internal)?,
            publish_tentative: row.try_get("publish_tentative").map_err(internal)?,
            publish_free: row.try_get("publish_free").map_err(internal)?,
        },
        revision: i64_to_u64(row.try_get("revision").map_err(internal)?)?,
        discovered_at: row.try_get("discovered_at").map_err(internal)?,
        configured_at: row.try_get("configured_at").map_err(internal)?,
        last_import_at: row.try_get("last_import_at").map_err(internal)?,
        planning_projection_state: parse_projection_state(
            &row.try_get::<String, _>("planning_projection_state")
                .map_err(internal)?,
        )?,
        planning_generation: i64_to_u64(row.try_get("planning_generation").map_err(internal)?)?,
        planning_collection_revision: row
            .try_get::<Option<i64>, _>("planning_collection_revision")
            .map_err(internal)?
            .map(i64_to_u64)
            .transpose()?,
        planning_window_start: row.try_get("planning_window_start").map_err(internal)?,
        planning_window_end: row.try_get("planning_window_end").map_err(internal)?,
        planning_window_refreshed_at: row
            .try_get("planning_window_refreshed_at")
            .map_err(internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
    })
}

fn parse_projection_state(
    value: &str,
) -> Result<CalendarProjectionState, GoogleSyncRepositoryError> {
    match value {
        "uninitialized" => Ok(CalendarProjectionState::Uninitialized),
        "complete" => Ok(CalendarProjectionState::Complete),
        "failed" => Ok(CalendarProjectionState::Failed),
        _ => Err(GoogleSyncRepositoryError::Internal),
    }
}

fn parse_event_disposition(
    value: &str,
) -> Result<GoogleEventDisposition, GoogleSyncRepositoryError> {
    match value {
        "ignore" => Ok(GoogleEventDisposition::Ignore),
        "visible_nonblocking" => Ok(GoogleEventDisposition::VisibleNonblocking),
        "blocking" => Ok(GoogleEventDisposition::Blocking),
        _ => Err(GoogleSyncRepositoryError::Internal),
    }
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
        refresh_generation: i64_to_u64(row.try_get("refresh_generation").map_err(internal)?)?,
        claimed_refresh_generation: i64_to_u64(
            row.try_get("claimed_refresh_generation")
                .map_err(internal)?,
        )?,
        completed_refresh_generation: i64_to_u64(
            row.try_get("completed_refresh_generation")
                .map_err(internal)?,
        )?,
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
        collection_revision: i64_to_u64(row.try_get("collection_revision").map_err(internal)?)?,
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
        required_scope: row.try_get("required_scope").map_err(internal)?,
        intent_hash: fixed_hash(&row.try_get::<Vec<u8>, _>("intent_hash").map_err(internal)?)?,
        approval_id: row.try_get("approval_id").map_err(internal)?,
        claim_id,
        run_claim_id: row.try_get("run_claim_id").map_err(internal)?,
        run_claim_generation: i64_to_u64(row.try_get("run_claim_generation").map_err(internal)?)?,
        provider_post_may_have_started: row
            .try_get("provider_post_may_have_started")
            .map_err(internal)?,
        attempts: i32_to_u32(row.try_get("attempts").map_err(internal)?)?,
    })
}

fn fixed_hash(value: &[u8]) -> Result<[u8; 32], GoogleSyncRepositoryError> {
    value
        .try_into()
        .map_err(|_| GoogleSyncRepositoryError::Internal)
}

fn parse_outbound_operation(value: &str) -> Result<OutboundOperation, GoogleSyncRepositoryError> {
    match value {
        "upsert" => Ok(OutboundOperation::Upsert),
        "delete" => Ok(OutboundOperation::Delete),
        _ => Err(GoogleSyncRepositoryError::Internal),
    }
}

fn encode_hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

#[cfg(test)]
fn decode_hex_bytes(value: &str) -> Result<[u8; 32], GoogleSyncRepositoryError> {
    if value.len() != 64 {
        return Err(GoogleSyncRepositoryError::Internal);
    }
    let mut result = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let nibble = |byte| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let high = nibble(pair[0]).ok_or(GoogleSyncRepositoryError::Internal)?;
        let low = nibble(pair[1]).ok_or(GoogleSyncRepositoryError::Internal)?;
        result[index] = (high << 4) | low;
    }
    Ok(result)
}

fn review_payload(payload: &Value) -> Value {
    let mut payload = payload.clone();
    if let Some(private) = payload
        .get_mut("extendedProperties")
        .and_then(|properties| properties.get_mut("private"))
        .and_then(Value::as_object_mut)
    {
        for value in private.values_mut() {
            *value = Value::String("[server-managed]".to_owned());
        }
    }
    payload
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
    use std::{str::FromStr, sync::Arc, time::Duration as StdDuration};

    use chrono::Duration;
    use serde_json::json;
    use sqlx::{
        ConnectOptions, Executor,
        postgres::{PgConnectOptions, PgPoolOptions},
    };

    use crate::{
        execution::{
            ExecutionCommand, ExecutionIdempotencyKey, ExecutionRepositoryError, ExecutionService,
            ExecutionServiceError, StartExecution,
        },
        google_oauth::{GoogleOAuthRepository, OAuthIdempotency},
        google_sync::{CalendarProjectionWindow, OutboundOperation, RejectedRemoteItem},
        items::{ItemKind, ItemService, NewItem},
        persistence::{
            MIGRATOR, PostgresExecutionRepository, PostgresGoogleOAuthRepository,
            PostgresItemRepository,
        },
        proposals::SystemClock,
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
                GoogleCalendarPolicy::default(),
                now,
            )
            .await
            .expect("writable owner calendar");
        repository
            .request_refresh(account_id, Uuid::new_v4(), now)
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

    async fn seed_execution_lease(
        pool: &PgPool,
        scope: DatabaseScope,
        item_id: Uuid,
        state: &str,
        now: DateTime<Utc>,
    ) -> Uuid {
        let item_revision: i64 =
            sqlx::query_scalar("SELECT revision FROM items WHERE workspace_id = $1 AND id = $2")
                .bind(scope.workspace_id)
                .bind(item_id)
                .fetch_one(pool)
                .await
                .expect("execution target revision");
        let session_id = Uuid::new_v4();
        match state {
            "active" => {
                sqlx::query(
                    "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
                     session_index, source_device_id, state, revision, accumulated_seconds, \
                     started_at, running_since, observed_running_since, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, 0, $5, 'active', 1, 0, $6, $6, $6, $6, $6)",
                )
                .bind(session_id)
                .bind(scope.workspace_id)
                .bind(item_id)
                .bind(item_revision)
                .bind(Uuid::new_v4())
                .bind(now)
                .execute(pool)
                .await
                .expect("active execution fixture");
            }
            "paused" => {
                sqlx::query(
                    "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
                     session_index, source_device_id, state, revision, accumulated_seconds, \
                     started_at, paused_at, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, 0, $5, 'paused', 1, 0, $6, $6, $6, $6)",
                )
                .bind(session_id)
                .bind(scope.workspace_id)
                .bind(item_id)
                .bind(item_revision)
                .bind(Uuid::new_v4())
                .bind(now)
                .execute(pool)
                .await
                .expect("paused execution fixture");
            }
            _ => panic!("unsupported execution fixture state"),
        }
        sqlx::query(
            "INSERT INTO execution_state (workspace_id, revision, active_session_id, updated_at) \
             VALUES ($1, 1, $2, $3) ON CONFLICT (workspace_id) DO UPDATE SET \
             revision = execution_state.revision + 1, active_session_id = EXCLUDED.active_session_id, \
             updated_at = EXCLUDED.updated_at",
        )
        .bind(scope.workspace_id)
        .bind(session_id)
        .bind(now)
        .execute(pool)
        .await
        .expect("execution state fixture");
        session_id
    }

    async fn close_execution_lease(
        pool: &PgPool,
        scope: DatabaseScope,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) {
        sqlx::query(
            "UPDATE execution_sessions SET state = 'completed', revision = revision + 1, \
             actual_seconds = accumulated_seconds, running_since = NULL, observed_running_since = NULL, \
             ended_at = $3, updated_at = $3 WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(session_id)
        .bind(now)
        .execute(pool)
        .await
        .expect("close execution fixture");
        sqlx::query(
            "UPDATE execution_state SET revision = revision + 1, active_session_id = NULL, \
             updated_at = $2 WHERE workspace_id = $1 AND active_session_id = $3",
        )
        .bind(scope.workspace_id)
        .bind(now)
        .bind(session_id)
        .execute(pool)
        .await
        .expect("release execution state fixture");
    }

    async fn wait_until_execution_state_is_locked(pool: &PgPool, scope: DatabaseScope) {
        tokio::time::timeout(StdDuration::from_secs(10), async {
            loop {
                let mut probe = pool.begin().await.expect("begin execution lock probe");
                let result = sqlx::query(
                    "SELECT workspace_id FROM execution_state WHERE workspace_id = $1 FOR UPDATE NOWAIT",
                )
                .bind(scope.workspace_id)
                .fetch_one(&mut *probe)
                .await;
                let locked = result
                    .as_ref()
                    .err()
                    .and_then(sqlx::Error::as_database_error)
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref()
                    == Some("55P03");
                probe
                    .rollback()
                    .await
                    .expect("release execution lock probe");
                if locked {
                    break;
                }
                result.expect("unexpected execution lock probe failure");
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("execution state was not locked before timeout");
    }

    async fn canonical_mutation_counts(
        pool: &PgPool,
        workspace_id: Uuid,
    ) -> (i64, i64, i64, i64, i64, i64) {
        sqlx::query_as(
            "SELECT \
             (SELECT count(*) FROM items WHERE workspace_id = $1), \
             (SELECT count(*) FROM provider_sync_mappings WHERE workspace_id = $1), \
             (SELECT count(*) FROM item_changes WHERE workspace_id = $1), \
             (SELECT count(*) FROM provider_sync_cursors WHERE workspace_id = $1), \
             (SELECT count(*) FROM google_sync_outbox WHERE workspace_id = $1), \
             (SELECT count(*) FROM audit_operations WHERE workspace_id = $1)",
        )
        .bind(workspace_id)
        .fetch_one(pool)
        .await
        .expect("canonical mutation counts")
    }

    fn local_firm_block(id: Uuid, title: &str, now: DateTime<Utc>) -> Item {
        Item::new(
            NewItem {
                id,
                is_sensitive: false,
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

    fn local_task(id: Uuid, title: &str, now: DateTime<Utc>) -> Item {
        Item::new(
            NewItem {
                id,
                is_sensitive: false,
                kind: ItemKind::Task,
                status: ItemStatus::Planned,
                title: title.to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(1_800),
                deadline_at: None,
                earliest_start_at: Some(now),
                recurrence: None,
                flexible_constraints: json!({}),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            },
            now,
        )
        .expect("local task")
    }

    fn projected_occurrence(
        fixture: &SyncFixture,
        remote_id: &str,
        title: &str,
        sensitive: bool,
        hash_marker: u8,
    ) -> RemoteItemChange {
        let start = fixture.now + Duration::hours(1);
        let end = start + Duration::hours(1);
        RemoteItemChange {
            account_id: fixture.account_id,
            collection_id: fixture.collection.id,
            collection_revision: fixture.collection.revision,
            dayweave_item_id: None,
            remote_id: remote_id.to_owned(),
            remote_parent_id: Some("restricted-series-identity".to_owned()),
            remote_etag: Some(format!("etag-{hash_marker}")),
            remote_updated_at: Some(fixture.now),
            remote_payload_hash: [hash_marker; 32],
            remote_projection_hash: [hash_marker.wrapping_add(1); 32],
            reviewed_provider_projection: None,
            item: Some(NewItem {
                id: Uuid::new_v4(),
                is_sensitive: sensitive,
                kind: ItemKind::Event,
                status: ItemStatus::Scheduled,
                title: title.to_owned(),
                notes: (!sensitive).then(|| "Public context".to_owned()),
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(3600),
                deadline_at: Some(end),
                earliest_start_at: Some(start),
                recurrence: None,
                flexible_constraints: json!({
                    "calendar_event": {
                        "start": start.to_rfc3339(),
                        "end": end.to_rfc3339(),
                        "immutable": true,
                        "all_day": false,
                        "source_calendar_id": null
                    }
                }),
                split_policy: SplitPolicy::Indivisible,
                importance: 0,
                urgency: 0,
                parent_id: None,
                sibling_order: 0,
            }),
        }
    }

    fn projection_batch(
        fixture: &SyncFixture,
        changes: Vec<RemoteItemChange>,
    ) -> CalendarProjectionBatch {
        CalendarProjectionBatch {
            account_id: fixture.account_id,
            collection_id: fixture.collection.id,
            collection_revision: fixture.collection.revision,
            changes,
            rejected: Vec::new(),
            window: CalendarProjectionWindow {
                start: fixture.now - Duration::days(30),
                end: fixture.now + Duration::days(120),
            },
        }
    }

    #[test]
    fn calendar_projection_validation_enforces_window_precision_and_exact_shapes() {
        let claim = SyncClaim {
            account_id: Uuid::new_v4(),
            claim_id: Uuid::new_v4(),
            claim_generation: 1,
        };
        let fixture_like = SyncFixtureValidation {
            account_id: claim.account_id,
            collection_id: Uuid::new_v4(),
            revision: 1,
            now: "2026-08-29T10:00:00Z".parse().expect("time"),
        };
        let valid = validation_projection_batch(&fixture_like);
        assert_eq!(valid.window.end - valid.window.start, Duration::days(150));
        assert!(validate_calendar_projection_batch(&claim, &valid).is_ok());

        let mut too_wide = valid.clone();
        too_wide.window.end += Duration::microseconds(1);
        assert_eq!(
            validate_calendar_projection_batch(&claim, &too_wide),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );
        let mut nanosecond = valid.clone();
        nanosecond.window.start += Duration::nanoseconds(1);
        assert_eq!(
            validate_calendar_projection_batch(&claim, &nanosecond),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );

        let mut missing_source = valid.clone();
        missing_source.changes[0]
            .item
            .as_mut()
            .expect("item")
            .flexible_constraints["calendar_event"]
            .as_object_mut()
            .expect("event object")
            .remove("source_calendar_id");
        assert_eq!(
            validate_calendar_projection_batch(&claim, &missing_source),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );
        let mut provider_source = valid;
        provider_source.changes[0]
            .item
            .as_mut()
            .expect("item")
            .flexible_constraints["calendar_event"]["source_calendar_id"] =
            Value::String("must-not-persist".to_owned());
        assert_eq!(
            validate_calendar_projection_batch(&claim, &provider_source),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );
    }

    #[test]
    fn calendar_projection_validation_accepts_sanitized_display_text_but_rejects_controls() {
        let claim = SyncClaim {
            account_id: Uuid::new_v4(),
            claim_id: Uuid::new_v4(),
            claim_generation: 1,
        };
        let fixture = SyncFixtureValidation {
            account_id: claim.account_id,
            collection_id: Uuid::new_v4(),
            revision: 1,
            now: "2026-08-29T10:00:00Z".parse().expect("time"),
        };
        let mut sanitized = validation_projection_batch(&fixture);
        let item = sanitized.changes[0].item.as_mut().expect("item");
        item.title = "Planning review".to_owned();
        item.notes = Some("Agenda first second third".to_owned());
        assert!(validate_calendar_projection_batch(&claim, &sanitized).is_ok());

        let mut multiline_title = sanitized.clone();
        multiline_title.changes[0]
            .item
            .as_mut()
            .expect("item")
            .title = "Planning\nreview".to_owned();
        assert_eq!(
            validate_calendar_projection_batch(&claim, &multiline_title),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );

        let mut multiline_notes = sanitized.clone();
        multiline_notes.changes[0]
            .item
            .as_mut()
            .expect("item")
            .notes = Some("Agenda\r\nfirst\tsecond".to_owned());
        assert_eq!(
            validate_calendar_projection_batch(&claim, &multiline_notes),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );

        sanitized.changes[0].item.as_mut().expect("item").notes =
            Some("Agenda\0details".to_owned());
        assert_eq!(
            validate_calendar_projection_batch(&claim, &sanitized),
            Err(GoogleSyncRepositoryError::InvalidProjectionBatch)
        );
    }

    struct SyncFixtureValidation {
        account_id: Uuid,
        collection_id: Uuid,
        revision: u64,
        now: DateTime<Utc>,
    }

    fn validation_projection_batch(fixture: &SyncFixtureValidation) -> CalendarProjectionBatch {
        let start = fixture.now + Duration::hours(1);
        let end = start + Duration::hours(1);
        CalendarProjectionBatch {
            account_id: fixture.account_id,
            collection_id: fixture.collection_id,
            collection_revision: fixture.revision,
            changes: vec![RemoteItemChange {
                account_id: fixture.account_id,
                collection_id: fixture.collection_id,
                collection_revision: fixture.revision,
                dayweave_item_id: None,
                remote_id: "restricted-id".to_owned(),
                remote_parent_id: None,
                remote_etag: Some("etag".to_owned()),
                remote_updated_at: Some(fixture.now),
                remote_payload_hash: [1; 32],
                remote_projection_hash: [2; 32],
                reviewed_provider_projection: None,
                item: Some(NewItem {
                    id: Uuid::new_v4(),
                    is_sensitive: false,
                    kind: ItemKind::Event,
                    status: ItemStatus::Scheduled,
                    title: "Valid occurrence".to_owned(),
                    notes: None,
                    timezone_name: "UTC".to_owned(),
                    duration_seconds: Some(3600),
                    deadline_at: Some(end),
                    earliest_start_at: Some(start),
                    recurrence: None,
                    flexible_constraints: json!({"calendar_event": {
                        "start": start.to_rfc3339(),
                        "end": end.to_rfc3339(),
                        "immutable": true,
                        "all_day": false,
                        "source_calendar_id": null
                    }}),
                    split_policy: SplitPolicy::Indivisible,
                    importance: 0,
                    urgency: 0,
                    parent_id: None,
                    sibling_order: 0,
                }),
            }],
            rejected: Vec::new(),
            window: CalendarProjectionWindow {
                start: fixture.now - Duration::days(30),
                end: fixture.now + Duration::days(120),
            },
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_projection_is_atomic_stable_private_and_invalidated_by_local_edits()
    {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar projection test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let remote_id = "restricted-occurrence-identity";
        let initial = projected_occurrence(&fixture, remote_id, "Visible meeting", false, 11);
        let initial_candidate_id = initial.item.as_ref().expect("candidate").id;
        let first = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![initial]),
                fixture.now,
            )
            .await
            .expect("first complete projection");
        assert!(first.complete);
        assert_eq!(first.generation, 1);
        assert_eq!(first.counts.imported, 1);
        let coverage: (String, i64, i64, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            "SELECT planning_projection_state, planning_generation, planning_collection_revision, \
             planning_window_start, planning_window_end FROM google_sync_collections \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("projection coverage");
        assert_eq!(coverage.0, "complete");
        assert_eq!(coverage.1, 1);
        assert_eq!(
            coverage.2,
            i64::try_from(fixture.collection.revision).expect("fixture revision")
        );
        assert_eq!(coverage.3, fixture.now - Duration::days(30));
        assert_eq!(coverage.4, fixture.now + Duration::days(120));
        let mapping: (Uuid, i64, bool, i64) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, provider_forced_sensitive, \
             projection_generation FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("occurrence mapping");
        assert_eq!(mapping.0, initial_candidate_id);
        assert_eq!((mapping.1, mapping.2, mapping.3), (1, false, 1));
        let raw_identity_leaked: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM items WHERE workspace_id = $1 \
                  AND (scheduling_constraints::text LIKE '%' || $2 || '%' \
                    OR title LIKE '%' || $2 || '%' OR coalesce(notes, '') LIKE '%' || $2 || '%')
                UNION ALL SELECT 1 FROM item_changes WHERE workspace_id = $1 \
                  AND payload::text LIKE '%' || $2 || '%'
                UNION ALL SELECT 1 FROM outbox_messages WHERE workspace_id = $1 \
                  AND payload::text LIKE '%' || $2 || '%'
                UNION ALL SELECT 1 FROM audit_operations WHERE workspace_id = $1 \
                  AND metadata::text LIKE '%' || $2 || '%'
             )",
        )
        .bind(fixture.scope.workspace_id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("privacy scan");
        assert!(!raw_identity_leaked);

        fixture
            .repository
            .begin_calendar_projection_refresh(
                &fixture.claim,
                fixture.now + Duration::milliseconds(500),
            )
            .await
            .expect("refresh-start fence");
        let refresh_fence: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT planning_projection_state, planning_generation, planning_window_start \
             FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("refresh-start coverage fence");
        assert_eq!(refresh_fence, ("uninitialized".to_owned(), 1, None));

        let same_semantics =
            projected_occurrence(&fixture, remote_id, "Visible meeting", false, 11);
        assert_ne!(
            same_semantics.item.as_ref().expect("candidate").id,
            initial_candidate_id
        );
        let second = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![same_semantics]),
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("overlapping refresh");
        assert!(second.complete);
        assert_eq!(second.generation, 2);
        let stable: (Uuid, i64, i64) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, projection_generation \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
               AND entity_kind = 'calendar_occurrence' AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("stable occurrence identity");
        assert_eq!(stable, (initial_candidate_id, 1, 2));
        let swept = fixture
            .repository
            .sweep_full_snapshot(
                &fixture.claim,
                fixture.collection.id,
                fixture.collection.revision,
                &[],
                fixture.now + Duration::milliseconds(1500),
            )
            .await
            .expect("metadata-lane full sweep");
        assert_eq!(swept, SyncCounts::default());
        let occurrence_survived_series_sweep: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.entity_kind = 'calendar_occurrence' AND mapping.remote_resource_id = $3 \
               AND item.trashed_at IS NULL)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("occurrence survives metadata sweep");
        assert!(occurrence_survived_series_sweep);

        sqlx::query(
            "UPDATE items SET title = 'Locally edited', revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(initial_candidate_id)
        .bind(fixture.now + Duration::seconds(2))
        .execute(&fixture.database.pool)
        .await
        .expect("local item edit");
        let invalidated: (String, Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT planning_projection_state, planning_window_start, planning_window_end \
                 FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("invalidated coverage");
        assert_eq!(invalidated, ("uninitialized".to_owned(), None, None));

        let private_refresh = projected_occurrence(&fixture, remote_id, "Busy", true, 12);
        let private_result = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![private_refresh]),
                fixture.now + Duration::seconds(3),
            )
            .await
            .expect("provider sensitivity floor overrides stale local projection");
        assert!(private_result.complete);
        let protected: (bool, String, Option<String>, bool) = sqlx::query_as(
            "SELECT item.is_sensitive, item.title, item.notes, mapping.provider_forced_sensitive \
             FROM items item JOIN provider_sync_mappings mapping \
               ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
             WHERE item.workspace_id = $1 AND item.id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(initial_candidate_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("private canonical projection");
        assert_eq!(protected, (true, "Busy".to_owned(), None, true));
        assert!(
            sqlx::query(
                "UPDATE items SET is_sensitive = false WHERE workspace_id = $1 AND id = $2"
            )
            .bind(fixture.scope.workspace_id)
            .bind(initial_candidate_id)
            .execute(&fixture.database.pool)
            .await
            .is_err(),
            "the provider privacy floor cannot be locally cleared"
        );

        let before_rejected: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM items WHERE workspace_id = $1), \
                    (SELECT count(*) FROM item_changes WHERE workspace_id = $1), \
                    (SELECT planning_generation FROM google_sync_collections \
                     WHERE workspace_id = $1 AND id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("pre-rejection counts");
        let mut rejected_batch = projection_batch(
            &fixture,
            vec![projected_occurrence(&fixture, remote_id, "Busy", true, 13)],
        );
        rejected_batch.rejected.push(RejectedRemoteItem {
            remote_id: "restricted-invalid-occurrence".to_owned(),
            reason: "unauthenticated_dayweave_marker",
        });
        let rejected = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                rejected_batch,
                fixture.now + Duration::seconds(4),
            )
            .await
            .expect("rejected batch is durably fail closed");
        assert!(!rejected.complete);
        assert_eq!(rejected.generation, private_result.generation);
        assert_eq!(rejected.counts.rejected, 1);
        let after_rejected: (i64, i64, i64, String, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM items WHERE workspace_id = $1), \
                    (SELECT count(*) FROM item_changes WHERE workspace_id = $1), \
                    planning_generation, planning_projection_state, planning_window_start \
             FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("post-rejection state");
        assert_eq!(
            (after_rejected.0, after_rejected.1, after_rejected.2),
            before_rejected
        );
        assert_eq!(
            (after_rejected.3.as_str(), after_rejected.4),
            ("failed", None)
        );

        let resealed = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![projected_occurrence(&fixture, remote_id, "Busy", true, 12)],
                ),
                fixture.now + Duration::seconds(5),
            )
            .await
            .expect("clean generation replaces rejection");
        assert!(resealed.complete);
        let removed_rejections: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM google_calendar_projection_rejections \
             WHERE workspace_id = $1 AND collection_id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("rejection cleanup");
        assert_eq!(removed_rejections, 0);
        let absent = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now + Duration::seconds(6),
            )
            .await
            .expect("complete absence generation");
        assert!(absent.complete);
        assert_eq!(absent.counts.deleted, 1);
        let absent_mapping: (Uuid, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT mapping.local_entity_id, item.trashed_at \
             FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.entity_kind = 'calendar_occurrence' \
               AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("retained absent mapping");
        assert_eq!(absent_mapping.0, initial_candidate_id);
        assert!(absent_mapping.1.is_some());
        let restored = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![projected_occurrence(&fixture, remote_id, "Busy", true, 12)],
                ),
                fixture.now + Duration::seconds(7),
            )
            .await
            .expect("overlapping-window occurrence reentry");
        assert!(restored.complete);
        let restored_identity: (Uuid, Uuid, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT mapping.id, mapping.local_entity_id, item.trashed_at \
             FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.entity_kind = 'calendar_occurrence' \
               AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("restored stable mapping");
        assert_eq!(
            (restored_identity.1, restored_identity.2),
            (initial_candidate_id, None)
        );
        let delete_error = sqlx::query("DELETE FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(fixture.scope.workspace_id)
            .bind(initial_candidate_id)
            .execute(&fixture.database.pool)
            .await
            .expect_err("forced-sensitive occurrence hard deletion must fail closed");
        assert_eq!(
            delete_error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23503")
        );
        let protected_after_delete: (bool, Option<DateTime<Utc>>, String) = sqlx::query_as(
            "SELECT item.is_sensitive, item.trashed_at, collection.planning_projection_state \
             FROM items item JOIN provider_sync_mappings mapping \
               ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
             JOIN google_sync_collections collection \
               ON collection.workspace_id = mapping.workspace_id \
              AND collection.id = mapping.collection_id \
             WHERE item.workspace_id = $1 AND item.id = $2 \
               AND mapping.entity_kind = 'calendar_occurrence'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(initial_candidate_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("forced-sensitive item remains intact");
        assert_eq!(protected_after_delete, (true, None, "complete".to_owned()));
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_projection_self_heals_non_sensitive_hard_delete() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar self-heal test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let remote_id = "hard-delete-self-heal-canary";
        let initial_change =
            projected_occurrence(&fixture, remote_id, "Visible provider event", false, 71);
        let initial_item_id = initial_change.item.as_ref().expect("initial item").id;
        let initial = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![initial_change]),
                fixture.now,
            )
            .await
            .expect("initial non-sensitive projection");
        assert!(initial.complete);
        let mapping_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("initial occurrence mapping");

        sqlx::query("DELETE FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(fixture.scope.workspace_id)
            .bind(initial_item_id)
            .execute(&fixture.database.pool)
            .await
            .expect("non-sensitive hard delete");
        let invalidated: String = sqlx::query_scalar(
            "SELECT planning_projection_state FROM google_sync_collections \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("hard delete invalidates coverage");
        assert_eq!(invalidated, "uninitialized");

        let recreated_change =
            projected_occurrence(&fixture, remote_id, "Visible provider event", false, 71);
        let recreated_item_id = recreated_change.item.as_ref().expect("recreated item").id;
        assert_ne!(recreated_item_id, initial_item_id);
        let recreated = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![recreated_change]),
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("complete projection recreates missing occurrence");
        assert!(recreated.complete);
        assert_eq!(recreated.generation, initial.generation + 1);
        assert_eq!(recreated.counts.imported, 1);
        let rebound: (Uuid, Uuid, Option<i64>, String, bool, bool, String) = sqlx::query_as(
            "SELECT mapping.id, mapping.local_entity_id, mapping.local_revision, \
                    mapping.sync_state, mapping.provider_forced_sensitive, item.is_sensitive, \
                    item.title FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.entity_kind = 'calendar_occurrence' \
               AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mapping rebound to recreated item");
        assert_eq!(
            rebound,
            (
                mapping_id,
                recreated_item_id,
                Some(1),
                "synced".to_owned(),
                false,
                false,
                "Visible provider event".to_owned(),
            )
        );
        let raw_identity_leaked: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM items WHERE workspace_id = $1 \
                  AND (scheduling_constraints::text LIKE '%' || $2 || '%' \
                    OR title LIKE '%' || $2 || '%' OR coalesce(notes, '') LIKE '%' || $2 || '%')
                UNION ALL SELECT 1 FROM item_changes WHERE workspace_id = $1 \
                  AND payload::text LIKE '%' || $2 || '%'
                UNION ALL SELECT 1 FROM outbox_messages WHERE workspace_id = $1 \
                  AND payload::text LIKE '%' || $2 || '%'
                UNION ALL SELECT 1 FROM audit_operations WHERE workspace_id = $1 \
                  AND metadata::text LIKE '%' || $2 || '%'
             )",
        )
        .bind(fixture.scope.workspace_id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("self-heal privacy scan");
        assert!(!raw_identity_leaked);

        sqlx::query("DELETE FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(fixture.scope.workspace_id)
            .bind(recreated_item_id)
            .execute(&fixture.database.pool)
            .await
            .expect("second non-sensitive hard delete");
        let retired = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now + Duration::seconds(2),
            )
            .await
            .expect("complete absence retires dangling mapping");
        assert!(retired.complete);
        assert_eq!(retired.generation, recreated.generation + 1);
        assert_eq!(retired.counts, SyncCounts::default());
        let retired_mapping: (Uuid, Option<Uuid>, Option<i64>, String, bool) = sqlx::query_as(
            "SELECT id, local_entity_id, local_revision, sync_state, provider_forced_sensitive \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
               AND entity_kind = 'calendar_occurrence' AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("dangling mapping retired");
        assert_eq!(
            retired_mapping,
            (
                mapping_id,
                Some(recreated_item_id),
                None,
                "deleted_remote".to_owned(),
                false,
            )
        );
        let repeated_absence = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now + Duration::seconds(3),
            )
            .await
            .expect("retired dangling mapping remains replaceable");
        assert!(repeated_absence.complete);
        assert_eq!(repeated_absence.generation, retired.generation + 1);
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_series_metadata_recovers_lost_response_and_fails_closed_on_conflict()
    {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar series test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now,
            )
            .await
            .expect("initial complete coverage");
        let item = local_firm_block(Uuid::new_v4(), "Owned series", fixture.now);
        let mut transaction = fixture.database.pool.begin().await.expect("item tx");
        insert_imported_item(&mut transaction, fixture.scope, &item)
            .await
            .expect("owned item fixture");
        transaction.commit().await.expect("item commit");
        let remote_id = "SYNTHETIC-RECOVERY-REMOTE-ID-CANARY";
        let reviewed = json!({
            "id": remote_id,
            "summary": "Owned series",
            "description": "Reviewed representation"
        });
        fixture
            .repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: reviewed.clone(),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("reviewed create queued");
        let recovered = fixture
            .repository
            .apply_calendar_series_metadata(
                &fixture.claim,
                RemoteCalendarSeriesChange {
                    account_id: fixture.account_id,
                    collection_id: fixture.collection.id,
                    collection_revision: fixture.collection.revision,
                    dayweave_item_id: Some(item.id),
                    remote_id: remote_id.to_owned(),
                    remote_etag: Some("etag-owned-1".to_owned()),
                    remote_updated_at: Some(fixture.now),
                    remote_payload_hash: [31; 32],
                    remote_projection_hash: [32; 32],
                    reviewed_provider_projection: Some(reviewed.clone()),
                    deleted: false,
                },
                fixture.now,
            )
            .await
            .expect("lost response identity recovery");
        assert_eq!(recovered, ImportOutcome::Unchanged);
        let recovered_mapping: (Option<Uuid>, String, String) = sqlx::query_as(
            "SELECT local_entity_id, ownership, sync_state FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 AND entity_kind = 'item' \
               AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("recovered mapping");
        assert_eq!(
            recovered_mapping,
            (Some(item.id), "dayweave".to_owned(), "synced".to_owned())
        );
        let recovery_audit: Value = sqlx::query_scalar(
            "SELECT metadata FROM audit_operations WHERE workspace_id = $1 \
             AND operation_type = 'google.sync.outbound_identity_recovered' \
             AND entity_type = 'item' AND entity_id = $2 ORDER BY occurred_at DESC, id DESC LIMIT 1",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("content-free recovery audit");
        let serialized_audit = serde_json::to_string(&recovery_audit).expect("serialize audit");
        assert!(
            !serialized_audit.contains(remote_id),
            "provider remote IDs must not be copied into general audit metadata"
        );
        assert_eq!(
            recovery_audit,
            json!({"collection_id": fixture.collection.id})
        );

        let owned_echo = RemoteItemChange {
            account_id: fixture.account_id,
            collection_id: fixture.collection.id,
            collection_revision: fixture.collection.revision,
            dayweave_item_id: Some(item.id),
            remote_id: remote_id.to_owned(),
            remote_parent_id: None,
            remote_etag: Some("etag-owned-1".to_owned()),
            remote_updated_at: Some(fixture.now),
            remote_payload_hash: [31; 32],
            remote_projection_hash: [91; 32],
            reviewed_provider_projection: Some(reviewed.clone()),
            item: None,
        };
        let ignored_echo = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![owned_echo.clone()]),
                fixture.now,
            )
            .await
            .expect("owned context-only echo validates without a duplicate item");
        assert!(ignored_echo.complete);
        let mut stale_echo = owned_echo.clone();
        stale_echo.remote_payload_hash = [99; 32];
        let stale = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![stale_echo]),
                fixture.now + Duration::milliseconds(500),
            )
            .await
            .expect("cross-lane mismatch fails closed");
        assert!(!stale.complete);
        let resealed = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![owned_echo]),
                fixture.now + Duration::milliseconds(750),
            )
            .await
            .expect("matching owned echo reseals coverage");
        assert!(resealed.complete);

        let conflict = fixture
            .repository
            .apply_calendar_series_metadata(
                &fixture.claim,
                RemoteCalendarSeriesChange {
                    account_id: fixture.account_id,
                    collection_id: fixture.collection.id,
                    collection_revision: fixture.collection.revision,
                    dayweave_item_id: Some(item.id),
                    remote_id: remote_id.to_owned(),
                    remote_etag: Some("etag-owned-2".to_owned()),
                    remote_updated_at: Some(fixture.now + Duration::seconds(1)),
                    remote_payload_hash: [33; 32],
                    remote_projection_hash: [34; 32],
                    reviewed_provider_projection: Some(json!({
                        "id": remote_id,
                        "summary": "Provider edited"
                    })),
                    deleted: false,
                },
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("owned provider conflict is committed");
        assert_eq!(conflict, ImportOutcome::Conflict);
        let failed: (String, Option<DateTime<Utc>>, Option<String>, String) = sqlx::query_as(
            "SELECT collection.planning_projection_state, collection.planning_window_start, \
                    collection.planning_last_error_code, mapping.sync_state \
             FROM google_sync_collections collection JOIN provider_sync_mappings mapping \
               ON mapping.workspace_id = collection.workspace_id \
              AND mapping.collection_id = collection.id \
             WHERE collection.workspace_id = $1 AND collection.id = $2 \
               AND mapping.entity_kind = 'item' AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("failed coverage");
        assert_eq!(
            failed,
            (
                "failed".to_owned(),
                None,
                Some("owned_provider_conflict".to_owned()),
                "conflict".to_owned()
            )
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_series_detaches_external_shadow_without_trashing_owned_item() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar shadow test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let item = local_firm_block(
            Uuid::new_v4(),
            "Owned block with legacy shadow",
            fixture.now,
        );
        let discovered = fixture
            .repository
            .replace_discovered(
                fixture.account_id,
                Some(&fixture.claim),
                GoogleCollectionKind::Calendar,
                vec![
                    DiscoveredCollection {
                        kind: GoogleCollectionKind::Calendar,
                        remote_id: "primary@example.test".to_owned(),
                        display_name: "Primary".to_owned(),
                        provider_access_role: Some("owner".to_owned()),
                        provider_primary: true,
                        provider_selected: true,
                        provider_hidden: false,
                        provider_deleted: false,
                    },
                    DiscoveredCollection {
                        kind: GoogleCollectionKind::Calendar,
                        remote_id: "legacy-shadow@example.test".to_owned(),
                        display_name: "Legacy shadow".to_owned(),
                        provider_access_role: Some("owner".to_owned()),
                        provider_primary: false,
                        provider_selected: true,
                        provider_hidden: false,
                        provider_deleted: false,
                    },
                ],
                fixture.now,
            )
            .await
            .expect("shadow collection discovery");
        let shadow = discovered
            .into_iter()
            .find(|collection| collection.remote_collection_id == "legacy-shadow@example.test")
            .expect("shadow collection");
        let shadow = fixture
            .repository
            .configure_collection(
                fixture.account_id,
                shadow.id,
                shadow.revision,
                true,
                true,
                GoogleSyncRole::Blocking,
                GoogleCalendarPolicy::default(),
                fixture.now,
            )
            .await
            .expect("selected shadow collection");

        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("shadow fixture tx");
        insert_imported_item(&mut transaction, fixture.scope, &item)
            .await
            .expect("owned item fixture");
        for (collection_id, remote_id, ownership, hash_marker) in [
            (
                fixture.collection.id,
                "owned-calendar-identity",
                "dayweave",
                61_u8,
            ),
            (shadow.id, "external-legacy-shadow", "external", 63_u8),
        ] {
            sqlx::query(
                "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
                 collection_id, entity_kind, local_entity_id, remote_resource_id, \
                 remote_payload_hash, remote_projection_hash, local_revision, sync_state, \
                 ownership, created_at, updated_at) VALUES ($1, $2, $3, $4, 'item', $5, $6, \
                 $7, $8, $9, 'synced', $10, $11, $11)",
            )
            .bind(Uuid::new_v4())
            .bind(fixture.scope.workspace_id)
            .bind(fixture.account_id)
            .bind(collection_id)
            .bind(item.id)
            .bind(remote_id)
            .bind(vec![hash_marker; 32])
            .bind(vec![hash_marker.wrapping_add(1); 32])
            .bind(u64_to_i64(item.revision).expect("item revision"))
            .bind(ownership)
            .bind(fixture.now)
            .execute(&mut *transaction)
            .await
            .expect("dual mapping fixture");
        }
        transaction.commit().await.expect("shadow fixture commit");

        let outcome = fixture
            .repository
            .apply_calendar_series_metadata(
                &fixture.claim,
                RemoteCalendarSeriesChange {
                    account_id: fixture.account_id,
                    collection_id: shadow.id,
                    collection_revision: shadow.revision,
                    dayweave_item_id: None,
                    remote_id: "external-legacy-shadow".to_owned(),
                    remote_etag: Some("etag-shadow-refreshed".to_owned()),
                    remote_updated_at: Some(fixture.now + Duration::seconds(1)),
                    remote_payload_hash: [65; 32],
                    remote_projection_hash: [66; 32],
                    reviewed_provider_projection: None,
                    deleted: false,
                },
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("external shadow metadata refresh");
        assert_eq!(outcome, ImportOutcome::Unchanged);

        let preserved: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("preserved owned item");
        assert_eq!(preserved, (u64_to_i64(item.revision).unwrap(), None));
        let owned_mapping: (Option<Uuid>, Option<i64>, String) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, sync_state FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind("owned-calendar-identity")
        .fetch_one(&fixture.database.pool)
        .await
        .expect("owned mapping retained");
        assert_eq!(
            owned_mapping,
            (
                Some(item.id),
                Some(u64_to_i64(item.revision).unwrap()),
                "synced".to_owned()
            )
        );
        let shadow_mapping: (Option<Uuid>, Option<i64>, String, String) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, sync_state, ownership \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
               AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(shadow.id)
        .bind("external-legacy-shadow")
        .fetch_one(&fixture.database.pool)
        .await
        .expect("external shadow detached");
        assert_eq!(
            shadow_mapping,
            (None, None, "synced".to_owned(), "external".to_owned())
        );

        // Migration 0014 forces one cursorless metadata scan. Recreate the
        // legacy attachment to prove an absent provider shadow takes the same
        // preservation path instead of deleting the shared owned item.
        sqlx::query(
            "UPDATE provider_sync_mappings SET local_entity_id = $3, local_revision = $4, \
             sync_state = 'synced' WHERE workspace_id = $1 AND id = ( \
               SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
                 AND collection_id = $2 AND remote_resource_id = 'external-legacy-shadow')",
        )
        .bind(fixture.scope.workspace_id)
        .bind(shadow.id)
        .bind(item.id)
        .bind(u64_to_i64(item.revision).unwrap())
        .execute(&fixture.database.pool)
        .await
        .expect("recreate absent legacy shadow");
        let swept = fixture
            .repository
            .sweep_full_snapshot(
                &fixture.claim,
                shadow.id,
                shadow.revision,
                &[],
                fixture.now + Duration::seconds(2),
            )
            .await
            .expect("cursorless absence detaches external shadow");
        assert_eq!(swept, SyncCounts::default());
        let preserved_after_sweep: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("owned item survives absent shadow");
        assert_eq!(preserved_after_sweep, preserved);
        let absent_shadow: (Option<Uuid>, Option<i64>, String) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, sync_state FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(shadow.id)
        .bind("external-legacy-shadow")
        .fetch_one(&fixture.database.pool)
        .await
        .expect("absent external shadow detached");
        assert_eq!(absent_shadow, (None, None, "deleted_remote".to_owned()));
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_owned_calendar_delete_echo_requires_exact_durable_acknowledgement() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; owned delete echo test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let repository = &fixture.repository;
        let item = local_firm_block(Uuid::new_v4(), "Published then deleted", fixture.now);
        let mut transaction = fixture.database.pool.begin().await.expect("item tx");
        insert_imported_item(&mut transaction, fixture.scope, &item)
            .await
            .expect("owned item fixture");
        transaction.commit().await.expect("item commit");

        let remote_id = "owned-published-delete";
        repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": remote_id, "summary": item.title}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("owned create queued");
        let create_work = repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("create claim")
            .expect("create work");
        let create_permit = repository
            .authorize_outbound_dispatch(&create_work, true, fixture.now)
            .await
            .expect("create permit");
        repository
            .complete_outbound(
                &create_work,
                OutboundResult {
                    remote_resource_id: remote_id.to_owned(),
                    remote_etag: Some("etag-before-delete".to_owned()),
                    remote_updated_at: Some(fixture.now),
                    payload_hash: [71; 32],
                    dispatch_nonce: create_permit.nonce,
                },
                fixture.now,
            )
            .await
            .expect("create acknowledgement");

        let deleted = item
            .trashed(fixture.now + Duration::seconds(1))
            .expect("local trash");
        let mut transaction = fixture.database.pool.begin().await.expect("trash tx");
        update_imported_item(&mut transaction, fixture.scope.workspace_id, &deleted)
            .await
            .expect("persist local trash");
        transaction.commit().await.expect("trash commit");
        repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: deleted.clone(),
                    operation: OutboundOperation::Delete,
                    payload: json!({}),
                },
                fixture.collection.id,
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("owned delete queued");
        let delete_work = repository
            .claim_outbound(&fixture.claim, fixture.now + Duration::seconds(1))
            .await
            .expect("delete claim")
            .expect("delete work");
        let delete_permit = repository
            .authorize_outbound_dispatch(&delete_work, true, fixture.now + Duration::seconds(1))
            .await
            .expect("delete permit");
        repository
            .complete_outbound(
                &delete_work,
                OutboundResult {
                    remote_resource_id: remote_id.to_owned(),
                    remote_etag: None,
                    remote_updated_at: None,
                    payload_hash: [72; 32],
                    dispatch_nonce: delete_permit.nonce,
                },
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("delete acknowledgement");
        let durable_delete: (String, Option<String>, i64, bool) = sqlx::query_as(
            "SELECT mapping.sync_state, mapping.remote_etag, mapping.local_revision, \
                    item.trashed_at IS NOT NULL \
             FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.entity_kind = 'item' AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("durable delete fence");
        assert_eq!(
            durable_delete,
            (
                "deleted_remote".to_owned(),
                None,
                u64_to_i64(deleted.revision).expect("deleted revision"),
                true,
            )
        );

        let markerless_hash = [73; 32];
        assert_eq!(
            repository
                .apply_calendar_series_metadata(
                    &fixture.claim,
                    RemoteCalendarSeriesChange {
                        account_id: fixture.account_id,
                        collection_id: fixture.collection.id,
                        collection_revision: fixture.collection.revision,
                        dayweave_item_id: None,
                        remote_id: remote_id.to_owned(),
                        remote_etag: Some("etag-tombstone-1".to_owned()),
                        remote_updated_at: Some(fixture.now + Duration::seconds(2)),
                        remote_payload_hash: markerless_hash,
                        remote_projection_hash: [74; 32],
                        reviewed_provider_projection: None,
                        deleted: true,
                    },
                    fixture.now + Duration::seconds(2),
                )
                .await
                .expect("markerless expected tombstone"),
            ImportOutcome::Unchanged
        );
        let markerless_expanded = RemoteItemChange {
            account_id: fixture.account_id,
            collection_id: fixture.collection.id,
            collection_revision: fixture.collection.revision,
            dayweave_item_id: None,
            remote_id: remote_id.to_owned(),
            remote_parent_id: None,
            remote_etag: Some("etag-tombstone-1".to_owned()),
            remote_updated_at: Some(fixture.now + Duration::seconds(2)),
            remote_payload_hash: markerless_hash,
            remote_projection_hash: [75; 32],
            reviewed_provider_projection: None,
            item: None,
        };
        let markerless_projection = repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![markerless_expanded]),
                fixture.now + Duration::seconds(2),
            )
            .await
            .expect("markerless expanded tombstone");
        assert!(markerless_projection.complete);
        let occurrence_mapping_created: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = $3 AND tombstoned_at IS NULL)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("no duplicate occurrence identity");
        assert!(!occurrence_mapping_created);

        let marked_hash = [76; 32];
        assert_eq!(
            repository
                .apply_calendar_series_metadata(
                    &fixture.claim,
                    RemoteCalendarSeriesChange {
                        account_id: fixture.account_id,
                        collection_id: fixture.collection.id,
                        collection_revision: fixture.collection.revision,
                        dayweave_item_id: Some(item.id),
                        remote_id: remote_id.to_owned(),
                        remote_etag: Some("etag-tombstone-2".to_owned()),
                        remote_updated_at: Some(fixture.now + Duration::seconds(3)),
                        remote_payload_hash: marked_hash,
                        remote_projection_hash: [77; 32],
                        reviewed_provider_projection: None,
                        deleted: true,
                    },
                    fixture.now + Duration::seconds(3),
                )
                .await
                .expect("marked expected tombstone"),
            ImportOutcome::Unchanged
        );
        let marked_projection = repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![RemoteItemChange {
                        account_id: fixture.account_id,
                        collection_id: fixture.collection.id,
                        collection_revision: fixture.collection.revision,
                        dayweave_item_id: Some(item.id),
                        remote_id: remote_id.to_owned(),
                        remote_parent_id: None,
                        remote_etag: Some("etag-tombstone-2".to_owned()),
                        remote_updated_at: Some(fixture.now + Duration::seconds(3)),
                        remote_payload_hash: marked_hash,
                        remote_projection_hash: [78; 32],
                        reviewed_provider_projection: Some(json!({"id": remote_id})),
                        item: None,
                    }],
                ),
                fixture.now + Duration::seconds(3),
            )
            .await
            .expect("marked expanded tombstone");
        assert!(marked_projection.complete);

        // A local revision advance invalidates the deletion acknowledgement;
        // the same provider identity can no longer be silently accepted.
        sqlx::query(
            "UPDATE items SET revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item.id)
        .bind(fixture.now + Duration::seconds(4))
        .execute(&fixture.database.pool)
        .await
        .expect("advance deleted local revision");
        assert_eq!(
            repository
                .apply_calendar_series_metadata(
                    &fixture.claim,
                    RemoteCalendarSeriesChange {
                        account_id: fixture.account_id,
                        collection_id: fixture.collection.id,
                        collection_revision: fixture.collection.revision,
                        dayweave_item_id: None,
                        remote_id: remote_id.to_owned(),
                        remote_etag: Some("etag-tombstone-stale".to_owned()),
                        remote_updated_at: Some(fixture.now + Duration::seconds(4)),
                        remote_payload_hash: [79; 32],
                        remote_projection_hash: [80; 32],
                        reviewed_provider_projection: None,
                        deleted: true,
                    },
                    fixture.now + Duration::seconds(4),
                )
                .await
                .expect("stale durable deletion becomes a conflict"),
            ImportOutcome::Conflict
        );

        // A provider-side deletion of a still-active, synced owned event is a
        // conflict, including when Google's tombstone omits the private marker.
        let active = local_firm_block(Uuid::new_v4(), "Provider deleted active", fixture.now);
        let mut transaction = fixture.database.pool.begin().await.expect("active item tx");
        insert_imported_item(&mut transaction, fixture.scope, &active)
            .await
            .expect("active owned item");
        transaction.commit().await.expect("active item commit");
        repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: active.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "owned-active-delete", "summary": active.title}),
                },
                fixture.collection.id,
                fixture.now + Duration::seconds(5),
            )
            .await
            .expect("active create queued");
        let active_work = repository
            .claim_outbound(&fixture.claim, fixture.now + Duration::seconds(5))
            .await
            .expect("active claim")
            .expect("active work");
        let active_permit = repository
            .authorize_outbound_dispatch(&active_work, true, fixture.now + Duration::seconds(5))
            .await
            .expect("active permit");
        repository
            .complete_outbound(
                &active_work,
                OutboundResult {
                    remote_resource_id: "owned-active-delete".to_owned(),
                    remote_etag: Some("etag-active".to_owned()),
                    remote_updated_at: Some(fixture.now + Duration::seconds(5)),
                    payload_hash: [81; 32],
                    dispatch_nonce: active_permit.nonce,
                },
                fixture.now + Duration::seconds(5),
            )
            .await
            .expect("active acknowledgement");
        let active_tombstone_hash = [82; 32];
        assert_eq!(
            repository
                .apply_calendar_series_metadata(
                    &fixture.claim,
                    RemoteCalendarSeriesChange {
                        account_id: fixture.account_id,
                        collection_id: fixture.collection.id,
                        collection_revision: fixture.collection.revision,
                        dayweave_item_id: None,
                        remote_id: "owned-active-delete".to_owned(),
                        remote_etag: Some("etag-active-tombstone".to_owned()),
                        remote_updated_at: Some(fixture.now + Duration::seconds(6)),
                        remote_payload_hash: active_tombstone_hash,
                        remote_projection_hash: [83; 32],
                        reviewed_provider_projection: None,
                        deleted: true,
                    },
                    fixture.now + Duration::seconds(6),
                )
                .await
                .expect("active provider deletion is retained as conflict"),
            ImportOutcome::Conflict
        );
        let active_projection = repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![RemoteItemChange {
                        account_id: fixture.account_id,
                        collection_id: fixture.collection.id,
                        collection_revision: fixture.collection.revision,
                        dayweave_item_id: None,
                        remote_id: "owned-active-delete".to_owned(),
                        remote_parent_id: None,
                        remote_etag: Some("etag-active-tombstone".to_owned()),
                        remote_updated_at: Some(fixture.now + Duration::seconds(6)),
                        remote_payload_hash: active_tombstone_hash,
                        remote_projection_hash: [84; 32],
                        reviewed_provider_projection: None,
                        item: None,
                    }],
                ),
                fixture.now + Duration::seconds(6),
            )
            .await
            .expect("expanded active tombstone fails semantically");
        assert!(!active_projection.complete);
        let active_mapping: (String, bool) = sqlx::query_as(
            "SELECT mapping.sync_state, item.trashed_at IS NULL \
             FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.entity_kind = 'item' AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind("owned-active-delete")
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active owned conflict retained");
        assert_eq!(active_mapping, ("conflict".to_owned(), true));
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::type_complexity)]
    async fn postgres_calendar_deselect_preserves_a_locally_edited_private_fork() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar fork test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let remote_id = "configuration-retired-local-private-fork";
        let projected = projected_occurrence(&fixture, remote_id, "Busy", true, 101);
        let projected_item_id = projected.item.as_ref().expect("projected item").id;
        fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![projected]),
                fixture.now,
            )
            .await
            .expect("initial private projection");
        let original_mapping: (Uuid, Option<i64>) = sqlx::query_as(
            "SELECT id, local_revision FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 \
               AND entity_kind = 'calendar_occurrence' AND remote_resource_id = $3 \
               AND tombstoned_at IS NULL",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("original private mapping");
        assert_eq!(original_mapping.1, Some(1));

        sqlx::query(
            "UPDATE items SET title = 'Private local fork', revision = revision + 1, \
             updated_at = $3 WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(projected_item_id)
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("local edit of private projection");
        let edited: (String, i64, Option<DateTime<Utc>>, bool) = sqlx::query_as(
            "SELECT title, revision, trashed_at, is_sensitive FROM items \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(projected_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("locally edited private fork");
        assert_eq!(edited, ("Private local fork".to_owned(), 2, None, true));
        let mutation_counts_before: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(projected_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mutation counts before deselection");

        let deselected = fixture
            .repository
            .configure_collection(
                fixture.account_id,
                fixture.collection.id,
                fixture.collection.revision,
                false,
                true,
                GoogleSyncRole::Writable,
                GoogleCalendarPolicy::default(),
                fixture.now + Duration::seconds(2),
            )
            .await
            .expect("deselect locally diverged calendar");
        assert!(!deselected.selected);
        let preserved: (String, i64, Option<DateTime<Utc>>, bool) = sqlx::query_as(
            "SELECT title, revision, trashed_at, is_sensitive FROM items \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(projected_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("private fork after deselection");
        assert_eq!(preserved, edited);
        let mutation_counts_after: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(projected_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mutation counts after deselection");
        assert_eq!(mutation_counts_after, mutation_counts_before);
        assert_eq!(mutation_counts_after.1, 0);

        let retired_mapping: (
            Option<Uuid>,
            Option<i64>,
            String,
            Option<DateTime<Utc>>,
            Option<Value>,
        ) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, sync_state, tombstoned_at, conflict_metadata \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(original_mapping.0)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("historical mapping for local fork");
        assert_eq!(retired_mapping.0, Some(projected_item_id));
        assert_eq!(retired_mapping.1, Some(1));
        assert_eq!(retired_mapping.2, "conflict");
        assert_eq!(retired_mapping.3, Some(fixture.now + Duration::seconds(2)));
        let conflict_metadata = retired_mapping.4.expect("local-only conflict metadata");
        assert_eq!(
            conflict_metadata,
            json!({
                "reason": "calendar_occurrence_configuration_retired_local_changed",
                "local_item_id": projected_item_id,
                "mapping_local_revision": 1,
                "item_revision": 2
            })
        );
        assert!(
            !conflict_metadata.to_string().contains(remote_id),
            "provider identity must not leak into local conflict metadata"
        );
        let invalidated: (
            String,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
        ) = sqlx::query_as(
            "SELECT planning_projection_state, planning_window_start, planning_window_end, \
                    planning_window_refreshed_at FROM google_sync_collections \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("invalidated projection after deselection");
        assert_eq!(invalidated, ("uninitialized".to_owned(), None, None, None));

        let reselected = fixture
            .repository
            .configure_collection(
                fixture.account_id,
                deselected.id,
                deselected.revision,
                true,
                true,
                GoogleSyncRole::Writable,
                GoogleCalendarPolicy::default(),
                fixture.now + Duration::seconds(3),
            )
            .await
            .expect("reselect calendar containing a preserved fork");
        let mut reappeared = projected_occurrence(&fixture, remote_id, "Busy", true, 102);
        reappeared.collection_revision = reselected.revision;
        let replacement_item_id = reappeared.item.as_ref().expect("replacement item").id;
        let mut reappearance_batch = projection_batch(&fixture, vec![reappeared]);
        reappearance_batch.collection_revision = reselected.revision;
        let reappearance = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                reappearance_batch,
                fixture.now + Duration::seconds(4),
            )
            .await
            .expect("same provider occurrence creates a fresh projection");
        assert!(reappearance.complete);
        assert_eq!(reappearance.counts.imported, 1);
        let active: (Uuid, Uuid, String, i64, Option<DateTime<Utc>>, bool, bool) = sqlx::query_as(
            "SELECT mapping.id, mapping.local_entity_id, item.title, item.revision, \
                        item.trashed_at, item.is_sensitive, mapping.provider_forced_sensitive \
                 FROM provider_sync_mappings mapping JOIN items item \
                   ON item.workspace_id = mapping.workspace_id \
                  AND item.id = mapping.local_entity_id \
                 WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
                   AND mapping.entity_kind = 'calendar_occurrence' \
                   AND mapping.remote_resource_id = $3 AND mapping.tombstoned_at IS NULL",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("fresh active mapping after provider reappearance");
        assert_ne!(active.0, original_mapping.0);
        assert_eq!(active.1, replacement_item_id);
        assert_ne!(active.1, projected_item_id);
        assert_eq!(
            (&active.2, active.3, active.4, active.5, active.6),
            (&"Busy".to_owned(), 1, None, true, true)
        );
        let fork_after_reappearance: (String, i64, Option<DateTime<Utc>>, bool) = sqlx::query_as(
            "SELECT title, revision, trashed_at, is_sensitive FROM items \
                 WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(projected_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("preserved fork after provider reappearance");
        assert_eq!(fork_after_reappearance, edited);
        let mapping_counts: (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE tombstoned_at IS NULL) \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
               AND entity_kind = 'calendar_occurrence' AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("historical and active provider mappings");
        assert_eq!(mapping_counts, (2, 1));
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_absence_history_quiesces_and_guards_a_local_restore() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar history test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let remote_id = "quiescent-calendar-occurrence";
        let projected = projected_occurrence(&fixture, remote_id, "Stable occurrence", false, 111);
        let item_id = projected.item.as_ref().expect("projected item").id;
        let initial = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![projected]),
                fixture.now,
            )
            .await
            .expect("initial occurrence projection");
        assert!(initial.complete);
        let mapping_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = $3 AND tombstoned_at IS NULL",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("initial occurrence mapping");
        let projection_index_predicate: String = sqlx::query_scalar(
            "SELECT pg_get_expr(index.indpred, index.indrelid) \
             FROM pg_index index JOIN pg_class class ON class.oid = index.indexrelid \
             JOIN pg_namespace namespace ON namespace.oid = class.relnamespace \
             WHERE class.relname = 'provider_sync_mappings_calendar_projection_idx' \
               AND namespace.nspname = current_schema()",
        )
        .fetch_one(&fixture.database.pool)
        .await
        .expect("projection work index predicate");
        assert!(projection_index_predicate.contains("sync_state"));
        assert!(projection_index_predicate.contains("deleted_remote"));

        let first_absence = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("first complete absence");
        assert!(first_absence.complete);
        assert_eq!(first_absence.counts.deleted, 1);
        let first_mapping_snapshot: (i64, DateTime<Utc>, Option<i64>, String) = sqlx::query_as(
            "SELECT projection_generation, updated_at, local_revision, sync_state \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(mapping_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mapping after first absence");
        assert_eq!(
            first_mapping_snapshot,
            (
                i64::try_from(first_absence.generation).expect("generation"),
                fixture.now + Duration::seconds(1),
                Some(2),
                "deleted_remote".to_owned()
            )
        );
        let first_item_snapshot: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("item after first absence");
        assert_eq!(
            first_item_snapshot,
            (2, Some(fixture.now + Duration::seconds(1)))
        );
        let first_mutation_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mutations after first absence");
        assert_eq!(first_mutation_counts.1, 1);

        for offset in 2..=4 {
            let quiescent = fixture
                .repository
                .replace_calendar_projection(
                    &fixture.claim,
                    projection_batch(&fixture, Vec::new()),
                    fixture.now + Duration::seconds(offset),
                )
                .await
                .expect("repeated empty projection");
            assert!(quiescent.complete);
            assert_eq!(quiescent.counts, SyncCounts::default());
        }
        let mapping_after_repeated_absence: (i64, DateTime<Utc>, Option<i64>, String) =
            sqlx::query_as(
                "SELECT projection_generation, updated_at, local_revision, sync_state \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(mapping_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("quiescent mapping after repeated absences");
        assert_eq!(mapping_after_repeated_absence, first_mapping_snapshot);
        let item_after_repeated_absence: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("quiescent item after repeated absences");
        assert_eq!(item_after_repeated_absence, first_item_snapshot);
        let mutations_after_repeated_absence: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("quiescent mutations after repeated absences");
        assert_eq!(mutations_after_repeated_absence, first_mutation_counts);

        let reappeared = projected_occurrence(
            &fixture,
            remote_id,
            "Stable occurrence reappeared",
            false,
            112,
        );
        let unused_candidate_id = reappeared.item.as_ref().expect("reappearance item").id;
        assert_ne!(unused_candidate_id, item_id);
        let reappearance = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, vec![reappeared]),
                fixture.now + Duration::seconds(5),
            )
            .await
            .expect("occurrence reappearance");
        assert!(reappearance.complete);
        assert_eq!(reappearance.counts.updated, 1);
        let restored_identity: (Uuid, Uuid, i64, Option<DateTime<Utc>>, String) = sqlx::query_as(
            "SELECT mapping.id, mapping.local_entity_id, item.revision, item.trashed_at, \
                        mapping.sync_state FROM provider_sync_mappings mapping JOIN items item \
                   ON item.workspace_id = mapping.workspace_id \
                  AND item.id = mapping.local_entity_id \
                 WHERE mapping.workspace_id = $1 AND mapping.id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(mapping_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("restored occurrence identity");
        assert_eq!(
            restored_identity,
            (mapping_id, item_id, 3, None, "synced".to_owned())
        );

        let second_absence = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now + Duration::seconds(6),
            )
            .await
            .expect("second complete absence");
        assert!(second_absence.complete);
        assert_eq!(second_absence.counts.deleted, 1);
        let second_mapping_snapshot: (i64, DateTime<Utc>, Option<i64>, String) = sqlx::query_as(
            "SELECT projection_generation, updated_at, local_revision, sync_state \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(mapping_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mapping after second absence");
        assert_eq!(
            second_mapping_snapshot,
            (
                i64::try_from(second_absence.generation).expect("generation"),
                fixture.now + Duration::seconds(6),
                Some(4),
                "deleted_remote".to_owned()
            )
        );
        let second_item_snapshot: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("item after second absence");
        assert_eq!(
            second_item_snapshot,
            (4, Some(fixture.now + Duration::seconds(6)))
        );
        let second_mutation_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mutations after second absence");
        assert_eq!(second_mutation_counts.1, 2);
        assert_eq!(second_mutation_counts.0, first_mutation_counts.0 + 2);
        assert_eq!(second_mutation_counts.2, first_mutation_counts.2 + 2);

        for offset in 7..=8 {
            let quiescent = fixture
                .repository
                .replace_calendar_projection(
                    &fixture.claim,
                    projection_batch(&fixture, Vec::new()),
                    fixture.now + Duration::seconds(offset),
                )
                .await
                .expect("empty projection after second retirement");
            assert!(quiescent.complete);
            assert_eq!(quiescent.counts, SyncCounts::default());
        }
        let mapping_after_second_quiescence: (i64, DateTime<Utc>, Option<i64>, String) =
            sqlx::query_as(
                "SELECT projection_generation, updated_at, local_revision, sync_state \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(mapping_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("mapping after second quiescence");
        assert_eq!(mapping_after_second_quiescence, second_mapping_snapshot);
        let item_after_second_quiescence: (i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("item after second quiescence");
        assert_eq!(item_after_second_quiescence, second_item_snapshot);
        let mutations_after_second_quiescence: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mutations after second quiescence");
        assert_eq!(mutations_after_second_quiescence, second_mutation_counts);

        sqlx::query(
            "UPDATE items SET title = 'Locally restored occurrence fork', trashed_at = NULL, \
             revision = revision + 1, updated_at = $3 WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .bind(fixture.now + Duration::seconds(9))
        .execute(&fixture.database.pool)
        .await
        .expect("local restore behind deleted provider mapping");
        let restored_fork_before_failure: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("locally restored fork before empty projection");
        assert_eq!(
            restored_fork_before_failure,
            ("Locally restored occurrence fork".to_owned(), 5, None)
        );
        let failed_absence = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(&fixture, Vec::new()),
                fixture.now + Duration::seconds(10),
            )
            .await
            .expect("local restore is reported as a durable projection conflict");
        assert!(!failed_absence.complete);
        assert_eq!(failed_absence.counts.conflicts, 1);
        let restored_fork_after_failure: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("locally restored fork after failed empty projection");
        assert_eq!(restored_fork_after_failure, restored_fork_before_failure);
        let mapping_after_failure: (i64, DateTime<Utc>, Option<i64>, String) = sqlx::query_as(
            "SELECT projection_generation, updated_at, local_revision, sync_state \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(mapping_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mapping after failed empty projection");
        assert_eq!(mapping_after_failure, second_mapping_snapshot);
        let mutations_after_failure: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2), \
               (SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                  AND change_kind = 'tombstone'), \
               (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
                  AND aggregate_type = 'item' AND aggregate_id = $2)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mutations after failed empty projection");
        assert_eq!(mutations_after_failure, second_mutation_counts);
        let failed_projection_state: (String, Option<String>) = sqlx::query_as(
            "SELECT planning_projection_state, planning_last_error_code \
             FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("failed projection state after local restore");
        assert_eq!(
            failed_projection_state,
            ("failed".to_owned(), Some("projection_conflict".to_owned()))
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Covers both authority-fence variants and durable no-cleanup behavior.
    async fn postgres_calendar_authority_fences_detach_an_executing_occurrence() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; active Calendar teardown test skipped");
            return;
        };
        for provider_delete in [false, true] {
            let fixture = sync_fixture(&database_url).await;
            let remote_id = if provider_delete {
                "active-provider-delete"
            } else {
                "active-deselect"
            };
            fixture
                .repository
                .replace_calendar_projection(
                    &fixture.claim,
                    projection_batch(
                        &fixture,
                        vec![projected_occurrence(
                            &fixture,
                            remote_id,
                            "Executing authority fence",
                            false,
                            if provider_delete { 119 } else { 118 },
                        )],
                    ),
                    fixture.now,
                )
                .await
                .expect("initial authority-fence projection");
            let item_id: Uuid = sqlx::query_scalar(
                "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
                 AND collection_id = $2 AND remote_resource_id = $3",
            )
            .bind(fixture.scope.workspace_id)
            .bind(fixture.collection.id)
            .bind(remote_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("authority-fence occurrence identity");
            let item_before: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
                "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(item_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("item before authority fence");
            let session_id = seed_execution_lease(
                &fixture.database.pool,
                fixture.scope,
                item_id,
                if provider_delete { "paused" } else { "active" },
                fixture.now + Duration::seconds(1),
            )
            .await;

            if provider_delete {
                fixture
                    .repository
                    .replace_discovered(
                        fixture.account_id,
                        Some(&fixture.claim),
                        GoogleCollectionKind::Calendar,
                        Vec::new(),
                        fixture.now + Duration::seconds(2),
                    )
                    .await
                    .expect("provider deletion commits during execution");
            } else {
                fixture
                    .repository
                    .configure_collection(
                        fixture.account_id,
                        fixture.collection.id,
                        fixture.collection.revision,
                        false,
                        true,
                        GoogleSyncRole::Writable,
                        GoogleCalendarPolicy::default(),
                        fixture.now + Duration::seconds(2),
                    )
                    .await
                    .expect("deselection commits during execution");
            }
            let item_after: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
                "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(item_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("item after authority fence");
            let mapping: (String, Option<DateTime<Utc>>, Option<i64>, Option<String>) =
                sqlx::query_as(
                    "SELECT sync_state, tombstoned_at, local_revision, \
                     conflict_metadata->>'reason' FROM provider_sync_mappings \
                     WHERE workspace_id = $1 AND collection_id = $2 AND remote_resource_id = $3",
                )
                .bind(fixture.scope.workspace_id)
                .bind(fixture.collection.id)
                .bind(remote_id)
                .fetch_one(&fixture.database.pool)
                .await
                .expect("detached authority-fence mapping");
            let lease: (String, Option<Uuid>) = sqlx::query_as(
                "SELECT session.state, state.active_session_id FROM execution_sessions session \
                 JOIN execution_state state ON state.workspace_id = session.workspace_id \
                 WHERE session.workspace_id = $1 AND session.id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(session_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("authority fence leaves lease open");
            assert_eq!(item_after, item_before);
            assert_eq!(mapping.0, "conflict");
            assert_eq!(mapping.1, Some(fixture.now + Duration::seconds(2)));
            assert_eq!(mapping.2, Some(item_before.1));
            assert_eq!(
                mapping.3.as_deref(),
                Some("calendar_occurrence_configuration_retired_execution_active")
            );
            assert_eq!(lease.1, Some(session_id));

            // The detached mapping is durable cleanup debt. Repeating the authority state must
            // not silently trash the preserved item after the lease disappears or without an
            // explicit cleanup journal.
            if provider_delete {
                fixture
                    .repository
                    .replace_discovered(
                        fixture.account_id,
                        Some(&fixture.claim),
                        GoogleCollectionKind::Calendar,
                        Vec::new(),
                        fixture.now + Duration::seconds(3),
                    )
                    .await
                    .expect("steady provider deletion");
            } else {
                let current = fixture
                    .repository
                    .collection(fixture.account_id, fixture.collection.id)
                    .await
                    .expect("deselected collection");
                fixture
                    .repository
                    .configure_collection(
                        fixture.account_id,
                        current.id,
                        current.revision,
                        false,
                        true,
                        GoogleSyncRole::Writable,
                        GoogleCalendarPolicy::default(),
                        fixture.now + Duration::seconds(3),
                    )
                    .await
                    .expect("steady deselection");
            }
            let item_after_repeat: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
                "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(item_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("preserved item after steady authority state");
            assert_eq!(item_after_repeat, item_before);
            close_execution_lease(
                &fixture.database.pool,
                fixture.scope,
                session_id,
                fixture.now + Duration::seconds(4),
            )
            .await;
            fixture.database.destroy().await;
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum CalendarTeardownCase {
        Deselect,
        ProviderDelete,
        RoleDowngrade,
        PolicyDowngrade,
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        clippy::type_complexity,
        clippy::single_match_else
    )] // One table-driven PostgreSQL lifecycle proof keeps each transition identical.
    async fn postgres_calendar_lifecycle_transitions_retire_occurrences_without_identity_churn() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar teardown test skipped");
            return;
        };
        for case in [
            CalendarTeardownCase::Deselect,
            CalendarTeardownCase::ProviderDelete,
            CalendarTeardownCase::RoleDowngrade,
            CalendarTeardownCase::PolicyDowngrade,
        ] {
            let fixture = sync_fixture(&database_url).await;
            let remote_id = format!("lifecycle-{case:?}");
            fixture
                .repository
                .replace_calendar_projection(
                    &fixture.claim,
                    projection_batch(
                        &fixture,
                        vec![projected_occurrence(
                            &fixture,
                            &remote_id,
                            "Lifecycle block",
                            false,
                            51,
                        )],
                    ),
                    fixture.now,
                )
                .await
                .expect("initial projection");
            let original_mapping: (Uuid, i64) = sqlx::query_as(
                "SELECT local_entity_id, local_revision FROM provider_sync_mappings \
                 WHERE workspace_id = $1 AND collection_id = $2 \
                   AND entity_kind = 'calendar_occurrence' AND remote_resource_id = $3",
            )
            .bind(fixture.scope.workspace_id)
            .bind(fixture.collection.id)
            .bind(&remote_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("mapping before lifecycle transition");

            let mut selected = true;
            let mut role = GoogleSyncRole::Writable;
            let mut policy = GoogleCalendarPolicy::default();
            match case {
                CalendarTeardownCase::Deselect => selected = false,
                CalendarTeardownCase::RoleDowngrade => role = GoogleSyncRole::ReadOnly,
                CalendarTeardownCase::PolicyDowngrade => {
                    policy.confirmed_busy = GoogleEventDisposition::VisibleNonblocking;
                }
                CalendarTeardownCase::ProviderDelete => {}
            }
            match case {
                CalendarTeardownCase::ProviderDelete => {
                    fixture
                        .repository
                        .replace_discovered(
                            fixture.account_id,
                            Some(&fixture.claim),
                            GoogleCollectionKind::Calendar,
                            Vec::new(),
                            fixture.now + Duration::seconds(1),
                        )
                        .await
                        .expect("provider deletion discovery");
                }
                _ => {
                    fixture
                        .repository
                        .configure_collection(
                            fixture.account_id,
                            fixture.collection.id,
                            fixture.collection.revision,
                            selected,
                            true,
                            role,
                            policy,
                            fixture.now + Duration::seconds(1),
                        )
                        .await
                        .expect("projection-affecting configuration");
                }
            }
            let retired: (
                Uuid,
                i64,
                String,
                Option<DateTime<Utc>>,
                String,
                Option<DateTime<Utc>>,
            ) = sqlx::query_as(
                "SELECT mapping.local_entity_id, mapping.local_revision, mapping.sync_state, \
                            item.trashed_at, collection.planning_projection_state, \
                            collection.planning_window_start \
                     FROM provider_sync_mappings mapping JOIN items item \
                       ON item.workspace_id = mapping.workspace_id \
                      AND item.id = mapping.local_entity_id \
                     JOIN google_sync_collections collection \
                       ON collection.workspace_id = mapping.workspace_id \
                      AND collection.id = mapping.collection_id \
                     WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
                       AND mapping.entity_kind = 'calendar_occurrence' \
                       AND mapping.remote_resource_id = $3",
            )
            .bind(fixture.scope.workspace_id)
            .bind(fixture.collection.id)
            .bind(&remote_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("retired occurrence");
            assert_eq!(retired.0, original_mapping.0, "{case:?}");
            assert_eq!(retired.1, original_mapping.1 + 1, "{case:?}");
            assert_eq!(retired.2, "pending_pull", "{case:?}");
            assert!(retired.3.is_some(), "{case:?}");
            assert_eq!((retired.4.as_str(), retired.5), ("uninitialized", None));
            let tombstones: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
                 AND change_kind = 'tombstone'",
            )
            .bind(fixture.scope.workspace_id)
            .bind(original_mapping.0)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("teardown delta");
            assert_eq!(tombstones, 1, "{case:?}");

            // Repeating the same steady-state transition must not churn the
            // retained item identity, revision, audit, or delta stream.
            match case {
                CalendarTeardownCase::ProviderDelete => {
                    fixture
                        .repository
                        .replace_discovered(
                            fixture.account_id,
                            Some(&fixture.claim),
                            GoogleCollectionKind::Calendar,
                            Vec::new(),
                            fixture.now + Duration::seconds(2),
                        )
                        .await
                        .expect("steady deleted discovery");
                }
                _ => {
                    let current = fixture
                        .repository
                        .collection(fixture.account_id, fixture.collection.id)
                        .await
                        .expect("current collection");
                    fixture
                        .repository
                        .configure_collection(
                            fixture.account_id,
                            current.id,
                            current.revision,
                            selected,
                            true,
                            role,
                            policy,
                            fixture.now + Duration::seconds(2),
                        )
                        .await
                        .expect("steady configuration");
                }
            }
            let unchanged: (i64, i64) = sqlx::query_as(
                "SELECT mapping.local_revision, \
                        (SELECT count(*) FROM item_changes change \
                         WHERE change.workspace_id = mapping.workspace_id \
                           AND change.item_id = mapping.local_entity_id \
                           AND change.change_kind = 'tombstone') \
                 FROM provider_sync_mappings mapping WHERE mapping.workspace_id = $1 \
                   AND mapping.collection_id = $2 AND mapping.entity_kind = 'calendar_occurrence' \
                   AND mapping.remote_resource_id = $3",
            )
            .bind(fixture.scope.workspace_id)
            .bind(fixture.collection.id)
            .bind(&remote_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("steady identity");
            assert_eq!(unchanged, (original_mapping.1 + 1, 1), "{case:?}");
            fixture.database.destroy().await;
        }
    }

    #[tokio::test]
    async fn postgres_identity_root_binding_survives_restart_and_rejects_config_drift() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; identity-root test skipped");
            return;
        };
        let database = TestDatabase::create(&database_url).await;
        MIGRATOR
            .run(&database.pool)
            .await
            .expect("migrations apply");
        let scope = seed_scope(&database.pool).await;
        let first = PostgresGoogleSyncRepository::new(database.pool.clone(), scope);
        let now: DateTime<Utc> = "2026-08-29T10:00:00Z".parse().expect("time");
        first
            .verify_or_initialize_identity_root(1, [71; 32], now)
            .await
            .expect("first startup pins verifier");

        // A new repository instance models a process restart. The exact root
        // remains valid and advances only the non-security timestamp.
        let restarted = PostgresGoogleSyncRepository::new(database.pool.clone(), scope);
        restarted
            .verify_or_initialize_identity_root(1, [71; 32], now + Duration::seconds(1))
            .await
            .expect("same root survives restart");
        assert_eq!(
            restarted
                .verify_or_initialize_identity_root(2, [71; 32], now + Duration::seconds(2))
                .await,
            Err(GoogleSyncRepositoryError::IdentityRootMismatch)
        );
        assert_eq!(
            restarted
                .verify_or_initialize_identity_root(1, [72; 32], now + Duration::seconds(3))
                .await,
            Err(GoogleSyncRepositoryError::IdentityRootMismatch)
        );
        let stored: (i64, bool, DateTime<Utc>) = sqlx::query_as(
            "SELECT identity_key_version, root_verifier = $4, last_verified_at \
             FROM google_provider_identity_roots WHERE workspace_id = $1 AND user_id = $2 \
               AND provider = $3",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind("google")
        .bind(vec![71_u8; 32])
        .fetch_one(&database.pool)
        .await
        .expect("durable identity root");
        assert_eq!(stored, (1, true, now + Duration::seconds(1)));
        database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_markerless_task_create_fence_survives_revision_changes() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; task-create fence test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let task_lists = fixture
            .repository
            .replace_discovered(
                fixture.account_id,
                None,
                GoogleCollectionKind::TaskList,
                vec![DiscoveredCollection {
                    kind: GoogleCollectionKind::TaskList,
                    remote_id: "tasks@example.test".to_owned(),
                    display_name: "Tasks".to_owned(),
                    provider_access_role: None,
                    provider_primary: false,
                    provider_selected: true,
                    provider_hidden: false,
                    provider_deleted: false,
                }],
                fixture.now,
            )
            .await
            .expect("task-list discovery");
        let task_list_discovered = task_lists
            .iter()
            .find(|collection| collection.kind == GoogleCollectionKind::TaskList)
            .expect("discovered task list");
        let task_list = fixture
            .repository
            .configure_collection(
                fixture.account_id,
                task_list_discovered.id,
                task_list_discovered.revision,
                true,
                true,
                GoogleSyncRole::Writable,
                GoogleCalendarPolicy::default(),
                fixture.now,
            )
            .await
            .expect("writable task list");
        let task = local_task(Uuid::new_v4(), "Markerless create", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("task transaction");
        insert_imported_item(&mut transaction, fixture.scope, &task)
            .await
            .expect("task fixture");
        transaction.commit().await.expect("task commit");
        fixture
            .repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "task",
                    item: task.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"title": "Markerless create"}),
                },
                task_list.id,
                fixture.now,
            )
            .await
            .expect("initial task create queued");
        let work = fixture
            .repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("task create claim")
            .expect("task create work");
        assert_eq!(work.entity_kind, "task");
        assert!(work.remote_resource_id.is_none());

        sqlx::query(
            "UPDATE items SET title = 'Markerless create revised', revision = 2, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(task.id)
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("task revision");
        let mut revised = task.clone();
        revised.title = "Markerless create revised".to_owned();
        revised.revision = 2;
        revised.updated_at = fixture.now + Duration::seconds(1);
        let revised_prepared = PreparedOutbound {
            entity_kind: "task",
            item: revised,
            operation: OutboundOperation::Upsert,
            payload: json!({"title": "Markerless create revised"}),
        };
        let revised_accepted = fixture
            .repository
            .enqueue_test_outbound(
                fixture.account_id,
                revised_prepared.clone(),
                task_list.id,
                fixture.now + Duration::seconds(1),
            )
            .await
            .expect("pre-send claim does not consume the one safe POST");
        assert_ne!(revised_accepted.outbox_id, work.id);
        assert_eq!(
            fixture
                .repository
                .fail_outbound(
                    &work,
                    "backoff",
                    "provider_temporary",
                    fixture.now + Duration::seconds(2),
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        let revised_work = fixture
            .repository
            .claim_outbound(&fixture.claim, fixture.now + Duration::seconds(2))
            .await
            .expect("revised task claim")
            .expect("revised task work");
        fixture
            .repository
            .authorize_outbound_dispatch(&revised_work, true, fixture.now + Duration::seconds(2))
            .await
            .expect("task POST initiation fenced");
        sqlx::query(
            "UPDATE items SET title = 'Markerless create third', revision = 3, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(task.id)
        .bind(fixture.now + Duration::seconds(3))
        .execute(&fixture.database.pool)
        .await
        .expect("third task revision");
        let mut third = revised_prepared;
        third.item.title = "Markerless create third".to_owned();
        third.item.revision = 3;
        third.item.updated_at = fixture.now + Duration::seconds(3);
        third.payload = json!({"title": "Markerless create third"});
        assert_eq!(
            fixture
                .repository
                .enqueue_test_outbound(
                    fixture.account_id,
                    third.clone(),
                    task_list.id,
                    fixture.now + Duration::seconds(3),
                )
                .await,
            Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable)
        );
        fixture
            .repository
            .fail_outbound(
                &revised_work,
                "failed",
                "provider_protocol",
                fixture.now + Duration::seconds(4),
                fixture.now + Duration::seconds(4),
            )
            .await
            .expect("unusable success is normalized to ambiguous create evidence");
        let retained_uncertainty: (String, Option<String>, bool, bool) = sqlx::query_as(
            "SELECT state, last_error_code, provider_post_may_have_started, \
                    send_started_at IS NOT NULL \
             FROM google_sync_outbox WHERE id = $1",
        )
        .bind(revised_work.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("retained markerless create uncertainty");
        assert_eq!(
            retained_uncertainty,
            (
                "conflict".to_owned(),
                Some("provider_identity_unresolved".to_owned()),
                true,
                true,
            ),
            "the live send-start marker, not a caller-supplied protocol label, is authoritative",
        );
        assert_eq!(
            fixture
                .repository
                .enqueue_test_outbound(
                    fixture.account_id,
                    third,
                    task_list.id,
                    fixture.now + Duration::seconds(5),
                )
                .await,
            Err(GoogleSyncRepositoryError::ConditionalWriteUnavailable)
        );
        let outbox_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM google_sync_outbox WHERE workspace_id = $1 \
             AND collection_id = $2 AND item_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(task_list.id)
        .bind(task.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("task outbox count");
        assert_eq!(outbox_count, 2);
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_outbound_approval_is_bound_expiring_replay_safe_and_dispatch_fenced() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL approval test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let repository = &fixture.repository;
        let item = local_firm_block(Uuid::new_v4(), "Approval fixture", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &item)
            .await
            .expect("approval item fixture");
        transaction.commit().await.expect("approval item commit");

        let preview_id = Uuid::new_v4();
        let preview = repository
            .create_outbound_preview(
                OutboundPreviewSpec {
                    id: preview_id,
                    account_id: fixture.account_id,
                    collection_id: fixture.collection.id,
                    collection_revision: fixture.collection.revision,
                    collection_remote_id: fixture.collection.remote_collection_id.clone(),
                    collection_display_name: fixture.collection.display_name.clone(),
                    required_scope: GOOGLE_CALENDAR_SCOPE,
                    prepared: PreparedOutbound {
                        entity_kind: "calendar_event",
                        item: item.clone(),
                        operation: OutboundOperation::Upsert,
                        payload: json!({"id": "synthetic-reviewed-event", "summary": "Approval fixture"}),
                    },
                    expires_at: fixture.now + Duration::minutes(10),
                },
                fixture.now,
            )
            .await
            .expect("preview created");
        assert!(preview.provider_resource_id.is_none());
        assert!(preview.provider_etag.is_none());
        let preview_hash = decode_hex_bytes(&preview.preview_hash).expect("preview hash");
        let capability_hash = [91_u8; 32];
        assert_eq!(
            repository
                .approve_outbound(
                    OutboundApprovalSpec {
                        account_id: fixture.account_id,
                        preview_id,
                        expected_preview_hash: [92_u8; 32],
                        capability_hash,
                    },
                    fixture.now,
                )
                .await,
            Err(GoogleSyncRepositoryError::ApprovalInvalid)
        );
        repository
            .approve_outbound(
                OutboundApprovalSpec {
                    account_id: fixture.account_id,
                    preview_id,
                    expected_preview_hash: preview_hash,
                    capability_hash,
                },
                fixture.now,
            )
            .await
            .expect("exact preview approved");
        assert_eq!(
            repository
                .approve_outbound(
                    OutboundApprovalSpec {
                        account_id: fixture.account_id,
                        preview_id,
                        expected_preview_hash: preview_hash,
                        capability_hash: [93_u8; 32],
                    },
                    fixture.now,
                )
                .await,
            Err(GoogleSyncRepositoryError::ApprovalAlreadyIssued)
        );

        let request = crate::google_sync::OutboundRequest {
            collection_id: fixture.collection.id,
            item_id: item.id,
            expected_item_revision: item.revision,
            operation: OutboundOperation::Upsert,
        };
        for swapped in [
            OutboundEnqueueSpec {
                account_id: Uuid::new_v4(),
                request: request.clone(),
                capability_hash,
            },
            OutboundEnqueueSpec {
                account_id: fixture.account_id,
                request: crate::google_sync::OutboundRequest {
                    collection_id: Uuid::new_v4(),
                    ..request.clone()
                },
                capability_hash,
            },
            OutboundEnqueueSpec {
                account_id: fixture.account_id,
                request: crate::google_sync::OutboundRequest {
                    operation: OutboundOperation::Delete,
                    ..request.clone()
                },
                capability_hash,
            },
        ] {
            assert_eq!(
                repository.enqueue_outbound(swapped, fixture.now).await,
                Err(GoogleSyncRepositoryError::ApprovalInvalid)
            );
        }

        let enqueue = OutboundEnqueueSpec {
            account_id: fixture.account_id,
            request: request.clone(),
            capability_hash,
        };
        let accepted = repository
            .enqueue_outbound(enqueue.clone(), fixture.now)
            .await
            .expect("capability consumed exactly once");
        assert!(!accepted.replayed);
        let replay = repository
            .enqueue_outbound(enqueue.clone(), fixture.now + Duration::seconds(1))
            .await
            .expect("exact immediate retry is idempotent");
        assert!(replay.replayed);
        assert_eq!(replay.outbox_id, accepted.outbox_id);
        let post_expiry = fixture.now + Duration::minutes(11);
        for post_expiry_swapped in [
            OutboundEnqueueSpec {
                account_id: Uuid::new_v4(),
                request: request.clone(),
                capability_hash,
            },
            OutboundEnqueueSpec {
                account_id: fixture.account_id,
                request: crate::google_sync::OutboundRequest {
                    collection_id: Uuid::new_v4(),
                    ..request.clone()
                },
                capability_hash,
            },
            OutboundEnqueueSpec {
                account_id: fixture.account_id,
                request: crate::google_sync::OutboundRequest {
                    item_id: Uuid::new_v4(),
                    ..request.clone()
                },
                capability_hash,
            },
            OutboundEnqueueSpec {
                account_id: fixture.account_id,
                request: crate::google_sync::OutboundRequest {
                    expected_item_revision: request.expected_item_revision + 1,
                    ..request.clone()
                },
                capability_hash,
            },
            OutboundEnqueueSpec {
                account_id: fixture.account_id,
                request: crate::google_sync::OutboundRequest {
                    operation: OutboundOperation::Delete,
                    ..request.clone()
                },
                capability_hash,
            },
        ] {
            assert_eq!(
                repository
                    .enqueue_outbound(post_expiry_swapped, post_expiry)
                    .await,
                Err(GoogleSyncRepositoryError::ApprovalInvalid)
            );
        }
        let expired_replay = repository
            .enqueue_outbound(enqueue, post_expiry)
            .await
            .expect("exact consumed retry remains a receipt after expiry");
        assert!(expired_replay.replayed);
        assert_eq!(expired_replay.outbox_id, accepted.outbox_id);

        let work = repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("claim approved work")
            .expect("approved work exists");
        sqlx::query(
            "UPDATE google_sync_outbox SET payload = '{\"summary\":\"mutated after review\"}'::jsonb \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(work.id)
        .execute(&fixture.database.pool)
        .await
        .expect("synthetic TOCTOU mutation");
        assert!(matches!(
            repository
                .authorize_outbound_dispatch(&work, true, fixture.now)
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        ));

        let expiring_item = local_firm_block(Uuid::new_v4(), "Expiry fixture", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("expiry item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &expiring_item)
            .await
            .expect("expiry item fixture");
        transaction.commit().await.expect("expiry item commit");
        let expiring_preview_id = Uuid::new_v4();
        let expiring_preview = repository
            .create_outbound_preview(
                OutboundPreviewSpec {
                    id: expiring_preview_id,
                    account_id: fixture.account_id,
                    collection_id: fixture.collection.id,
                    collection_revision: fixture.collection.revision,
                    collection_remote_id: fixture.collection.remote_collection_id.clone(),
                    collection_display_name: fixture.collection.display_name.clone(),
                    required_scope: GOOGLE_CALENDAR_SCOPE,
                    prepared: PreparedOutbound {
                        entity_kind: "calendar_event",
                        item: expiring_item.clone(),
                        operation: OutboundOperation::Upsert,
                        payload: json!({"id": "synthetic-expiring-event"}),
                    },
                    expires_at: fixture.now + Duration::seconds(1),
                },
                fixture.now,
            )
            .await
            .expect("expiring preview");
        repository
            .approve_outbound(
                OutboundApprovalSpec {
                    account_id: fixture.account_id,
                    preview_id: expiring_preview_id,
                    expected_preview_hash: decode_hex_bytes(&expiring_preview.preview_hash)
                        .expect("expiring preview hash"),
                    capability_hash: [94_u8; 32],
                },
                fixture.now,
            )
            .await
            .expect("expiring capability issued");
        assert_eq!(
            repository
                .enqueue_outbound(
                    OutboundEnqueueSpec {
                        account_id: fixture.account_id,
                        request: crate::google_sync::OutboundRequest {
                            collection_id: fixture.collection.id,
                            item_id: expiring_item.id,
                            expected_item_revision: expiring_item.revision,
                            operation: OutboundOperation::Upsert,
                        },
                        capability_hash: [94_u8; 32],
                    },
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ApprovalExpired)
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_post_provider_mapping_guard_rejects_create_and_existing_identity_races() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL mapping race test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let repository = &fixture.repository;

        let create_item = local_firm_block(Uuid::new_v4(), "Create mapping race", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("create item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &create_item)
            .await
            .expect("create item fixture");
        transaction.commit().await.expect("create item commit");
        let create_outbox = repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: create_item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "reviewed-create-id"}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("create work queued");
        let create_work = repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("create work claim")
            .expect("create work");
        assert!(create_work.remote_resource_id.is_none());
        let create_permit = repository
            .authorize_outbound_dispatch(&create_work, true, fixture.now)
            .await
            .expect("create dispatch permit");
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, collection_id, \
             entity_kind, local_entity_id, remote_resource_id, remote_etag, local_revision, sync_state, \
             ownership, created_at, updated_at) VALUES ($1, $2, $3, $4, 'item', NULL, $5, $6, NULL, \
             'synced', 'external', $7, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .bind(fixture.collection.id)
        // Simulate an inbound mapping for the provider identity returned by
        // the write while the create dispatch was in flight. It has no local
        // identity, so guarding only `local_entity_id` would miss it.
        .bind("reviewed-create-id")
        .bind("etag-race-create")
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("concurrent create mapping fixture");
        assert_eq!(
            repository
                .complete_outbound(
                    &create_work,
                    OutboundResult {
                        remote_resource_id: "reviewed-create-id".to_owned(),
                        remote_etag: Some("etag-provider-response".to_owned()),
                        remote_updated_at: Some(fixture.now),
                        payload_hash: [81_u8; 32],
                        dispatch_nonce: create_permit.nonce,
                    },
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        let create_state: (String, String, Option<Uuid>) = sqlx::query_as(
            "SELECT state, last_error_code, dispatch_nonce FROM google_sync_outbox \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(create_outbox.outbox_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("create race state");
        assert_eq!(
            create_state,
            (
                "conflict".to_owned(),
                "provider_mapping_changed_during_delivery".to_owned(),
                None,
            )
        );

        let existing_item = local_firm_block(Uuid::new_v4(), "ETag mapping race", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("existing item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &existing_item)
            .await
            .expect("existing item fixture");
        transaction.commit().await.expect("existing item commit");
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, collection_id, \
             entity_kind, local_entity_id, remote_resource_id, remote_etag, local_revision, sync_state, \
             ownership, created_at, updated_at) VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, $8, \
             'synced', 'dayweave', $9, $9)",
        )
        .bind(Uuid::new_v4())
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .bind(fixture.collection.id)
        .bind(existing_item.id)
        .bind("provider-existing")
        .bind("etag-reviewed")
        .bind(u64_to_i64(existing_item.revision).expect("revision"))
        .bind(fixture.now)
        .execute(&fixture.database.pool)
        .await
        .expect("existing mapping fixture");
        let existing_outbox = repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: existing_item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "reviewed-existing-id"}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("existing work queued");
        let existing_work = repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("existing work claim")
            .expect("existing work");
        assert_eq!(
            existing_work.remote_resource_id.as_deref(),
            Some("provider-existing")
        );
        assert_eq!(
            existing_work.expected_etag.as_deref(),
            Some("etag-reviewed")
        );
        let existing_permit = repository
            .authorize_outbound_dispatch(&existing_work, true, fixture.now)
            .await
            .expect("existing dispatch permit");
        sqlx::query(
            "UPDATE provider_sync_mappings SET remote_etag = 'etag-raced', updated_at = $5 \
             WHERE workspace_id = $1 AND provider_account_id = $2 AND collection_id = $3 \
               AND local_entity_id = $4 AND tombstoned_at IS NULL",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .bind(fixture.collection.id)
        .bind(existing_item.id)
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("concurrent ETag mutation fixture");
        assert_eq!(
            repository
                .complete_outbound(
                    &existing_work,
                    OutboundResult {
                        remote_resource_id: "provider-existing".to_owned(),
                        remote_etag: Some("etag-provider-response".to_owned()),
                        remote_updated_at: Some(fixture.now),
                        payload_hash: [82_u8; 32],
                        dispatch_nonce: existing_permit.nonce,
                    },
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        let existing_state: (String, String, Option<Uuid>) = sqlx::query_as(
            "SELECT state, last_error_code, dispatch_nonce FROM google_sync_outbox \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(existing_outbox.outbox_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("existing race state");
        assert_eq!(
            existing_state,
            (
                "conflict".to_owned(),
                "provider_mapping_changed_during_delivery".to_owned(),
                None,
            )
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    async fn postgres_refresh_clears_stale_non_running_completion_timestamps() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; refresh timestamp reset test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let retry_at = fixture.now + Duration::minutes(1);
        let stale_started_at = retry_at + Duration::minutes(1);
        let stale_completed_at = retry_at + Duration::minutes(2);
        sqlx::query(
            "UPDATE google_sync_runs SET state = 'idle', claim_id = NULL, lease_until = NULL, \
             started_at = $4, completed_at = $5, next_attempt_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(fixture.account_id)
        .bind(stale_started_at)
        .bind(stale_completed_at)
        .execute(&fixture.database.pool)
        .await
        .expect("stale completed run fixture");

        fixture
            .repository
            .request_refresh(fixture.account_id, Uuid::new_v4(), retry_at)
            .await
            .expect("refresh accepted");

        let refreshed = sqlx::query(
            "SELECT state, requested_at, started_at, completed_at FROM google_sync_runs \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(fixture.account_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("refreshed run");
        assert_eq!(refreshed.get::<String, _>("state"), "idle");
        assert_eq!(
            refreshed.get::<Option<DateTime<Utc>>, _>("requested_at"),
            Some(retry_at)
        );
        assert_eq!(
            refreshed.get::<Option<DateTime<Utc>>, _>("started_at"),
            None
        );
        assert_eq!(
            refreshed.get::<Option<DateTime<Utc>>, _>("completed_at"),
            None
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Keeps the complete causal lifecycle in one regression.
    async fn postgres_refresh_generation_is_exact_and_clock_independent() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; refresh generation test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let initial = fixture
            .repository
            .run_status(fixture.account_id)
            .await
            .expect("initial status")
            .expect("running fixture status");
        assert_eq!(initial.state, GoogleSyncRunState::Running);
        assert_eq!(
            initial.claimed_refresh_generation,
            initial.refresh_generation
        );

        let request_id = Uuid::new_v4();
        let skewed_request_time = fixture.now - Duration::hours(1);
        let accepted = fixture
            .repository
            .request_refresh(fixture.account_id, request_id, skewed_request_time)
            .await
            .expect("clock-skewed refresh accepted");
        assert_eq!(accepted.request_id, request_id);
        assert_eq!(accepted.refresh_generation, initial.refresh_generation + 1);
        let replay = fixture
            .repository
            .request_refresh(
                fixture.account_id,
                request_id,
                fixture.now + Duration::hours(4),
            )
            .await
            .expect("exact refresh replay");
        assert_eq!(replay, accepted);

        let during_run = fixture
            .repository
            .run_status(fixture.account_id)
            .await
            .expect("during-run status")
            .expect("during-run row");
        assert_eq!(during_run.refresh_generation, accepted.refresh_generation);
        assert_eq!(
            during_run.claimed_refresh_generation,
            initial.claimed_refresh_generation
        );

        let first_completed_at = fixture.now + Duration::seconds(1);
        fixture
            .repository
            .complete_claim(
                &fixture.claim,
                &SyncCounts::default(),
                first_completed_at,
                fixture.now + Duration::hours(1),
            )
            .await
            .expect("first claim completes");
        let after_first = fixture
            .repository
            .run_status(fixture.account_id)
            .await
            .expect("first completion status")
            .expect("first completion row");
        assert_eq!(after_first.state, GoogleSyncRunState::Idle);
        assert_eq!(after_first.next_attempt_at, first_completed_at);
        assert_eq!(
            after_first.completed_refresh_generation,
            initial.claimed_refresh_generation
        );
        assert!(
            after_first.completed_refresh_generation < accepted.refresh_generation,
            "the pre-request run must not satisfy the accepted refresh"
        );

        let follow_up = fixture
            .repository
            .claim_due(
                first_completed_at,
                first_completed_at + Duration::minutes(10),
            )
            .await
            .expect("follow-up claim query")
            .expect("follow-up claim");
        let follow_up_running = fixture
            .repository
            .run_status(fixture.account_id)
            .await
            .expect("follow-up status")
            .expect("follow-up row");
        assert_eq!(
            follow_up_running.claimed_refresh_generation,
            accepted.refresh_generation
        );
        assert_eq!(follow_up_running.completed_at, None);

        fixture
            .repository
            .complete_claim(
                &follow_up,
                &SyncCounts::default(),
                first_completed_at + Duration::seconds(1),
                fixture.now + Duration::hours(1),
            )
            .await
            .expect("follow-up completes");
        let completed = fixture
            .repository
            .run_status(fixture.account_id)
            .await
            .expect("final status")
            .expect("final row");
        assert_eq!(
            completed.completed_refresh_generation,
            accepted.refresh_generation
        );

        let request_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM google_sync_refresh_requests \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
               AND request_id = $4",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(fixture.account_id)
        .bind(request_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("refresh request count");
        assert_eq!(request_count, 1);
        fixture.database.destroy().await;
    }

    #[tokio::test]
    async fn postgres_refresh_acceptance_outlives_pause_and_disconnect_without_authorizing_new_requests()
     {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; refresh replay lifecycle test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let request_id = Uuid::new_v4();
        let accepted = fixture
            .repository
            .request_refresh(fixture.account_id, request_id, fixture.now)
            .await
            .expect("initial refresh is durably accepted");

        for status in ["paused", "revoked"] {
            let lifecycle_update = if status == "revoked" {
                "UPDATE provider_accounts SET status = $4, sync_enabled = false, \
                 encrypted_credentials = NULL, credential_key_version = NULL, \
                 granted_scopes = '{}', token_expires_at = NULL, is_default = false, \
                 disconnected_at = $5, revision = revision + 1, updated_at = $5 \
                 WHERE workspace_id = $1 AND user_id = $2 AND id = $3"
            } else {
                "UPDATE provider_accounts SET status = $4, sync_enabled = false, \
                 revision = revision + 1, updated_at = $5 \
                 WHERE workspace_id = $1 AND user_id = $2 AND id = $3"
            };
            sqlx::query(lifecycle_update)
                .bind(fixture.scope.workspace_id)
                .bind(fixture.scope.user_id)
                .bind(fixture.account_id)
                .bind(status)
                .bind(fixture.now + Duration::minutes(1))
                .execute(&fixture.database.pool)
                .await
                .expect("account lifecycle mutation");

            let lookup = fixture
                .repository
                .refresh_request(fixture.account_id, request_id)
                .await
                .expect("acceptance lookup succeeds")
                .expect("accepted request remains durable");
            assert_eq!(lookup, accepted);
            let replay = fixture
                .repository
                .request_refresh(
                    fixture.account_id,
                    request_id,
                    fixture.now + Duration::hours(1),
                )
                .await
                .expect("exact request replays before account-state validation");
            assert_eq!(replay, accepted);

            let new_request = fixture
                .repository
                .request_refresh(
                    fixture.account_id,
                    Uuid::new_v4(),
                    fixture.now + Duration::hours(1),
                )
                .await;
            assert_eq!(new_request, Err(GoogleSyncRepositoryError::AccountNotFound));
        }

        let other_user = PostgresGoogleSyncRepository::new(
            fixture.database.pool.clone(),
            DatabaseScope {
                workspace_id: fixture.scope.workspace_id,
                user_id: Uuid::new_v4(),
            },
        );
        assert_eq!(
            other_user
                .refresh_request(fixture.account_id, request_id)
                .await
                .expect("cross-user lookup is safely empty"),
            None
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    async fn postgres_full_snapshot_sweep_waits_for_canonical_item_lock() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; snapshot lock test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let mut blocker = fixture
            .database
            .pool
            .begin()
            .await
            .expect("canonical lock blocker");
        super::super::database::lock_canonical_item_space(&mut blocker, fixture.scope.workspace_id)
            .await
            .expect("hold canonical item lock");

        let repository = fixture.repository.clone();
        let claim = fixture.claim.clone();
        let collection_id = fixture.collection.id;
        let collection_revision = fixture.collection.revision;
        let now = fixture.now;
        let mut sweep = tokio::spawn(async move {
            repository
                .sweep_full_snapshot(&claim, collection_id, collection_revision, &[], now)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut sweep)
                .await
                .is_err(),
            "snapshot sweep must acquire the canonical lock before account/collection rows"
        );
        blocker.commit().await.expect("release canonical item lock");
        let counts = tokio::time::timeout(std::time::Duration::from_secs(2), sweep)
            .await
            .expect("snapshot sweep unblocks")
            .expect("snapshot task")
            .expect("snapshot succeeds");
        assert_eq!(counts, SyncCounts::default());
        fixture.database.destroy().await;
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
            .enqueue_test_outbound(
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
        observed_owned.reviewed_provider_projection = Some(json!({"id": "snapshot-owned"}));
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
            "SELECT conflict_metadata->>'reason' FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND collection_id = $2 AND local_entity_id = $3",
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
    async fn postgres_inbound_task_close_respects_active_and_paused_execution_leases() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; inbound execution guard test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let discovered = fixture
            .repository
            .replace_discovered(
                fixture.account_id,
                None,
                GoogleCollectionKind::TaskList,
                vec![DiscoveredCollection {
                    kind: GoogleCollectionKind::TaskList,
                    remote_id: "execution-guard-tasks".to_owned(),
                    display_name: "Execution guard tasks".to_owned(),
                    provider_access_role: None,
                    provider_primary: false,
                    provider_selected: true,
                    provider_hidden: false,
                    provider_deleted: false,
                }],
                fixture.now,
            )
            .await
            .expect("task list discovery");
        let task_list = fixture
            .repository
            .configure_collection(
                fixture.account_id,
                discovered
                    .iter()
                    .find(|collection| collection.kind == GoogleCollectionKind::TaskList)
                    .expect("task list")
                    .id,
                discovered
                    .iter()
                    .find(|collection| collection.kind == GoogleCollectionKind::TaskList)
                    .expect("task list")
                    .revision,
                true,
                true,
                GoogleSyncRole::Writable,
                GoogleCalendarPolicy::default(),
                fixture.now,
            )
            .await
            .expect("task list configured");

        let completed_remote_id = "execution-active-completed-task";
        assert_eq!(
            fixture
                .repository
                .apply_remote_item(
                    &fixture.claim,
                    remote_task(
                        fixture.account_id,
                        task_list.id,
                        task_list.revision,
                        completed_remote_id,
                        "Run before completion",
                        ItemStatus::Planned,
                        [121; 32],
                    ),
                    fixture.now,
                )
                .await
                .expect("initial task import"),
            ImportOutcome::Created
        );
        let completed_item_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(task_list.id)
        .bind(completed_remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("completed task identity");
        let active_session = seed_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            completed_item_id,
            "active",
            fixture.now + Duration::seconds(1),
        )
        .await;
        let counts_before =
            canonical_mutation_counts(&fixture.database.pool, fixture.scope.workspace_id).await;
        let item_before: (String, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT status, revision, completed_at, trashed_at FROM items \
                 WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(completed_item_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("task before blocked completion");
        let mapping_before: (Option<String>, Option<Vec<u8>>, Option<i64>, String) =
            sqlx::query_as(
                "SELECT remote_etag, remote_payload_hash, local_revision, sync_state \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
                   AND remote_resource_id = $3",
            )
            .bind(fixture.scope.workspace_id)
            .bind(task_list.id)
            .bind(completed_remote_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("mapping before blocked completion");
        assert_eq!(
            fixture
                .repository
                .apply_remote_item(
                    &fixture.claim,
                    remote_task(
                        fixture.account_id,
                        task_list.id,
                        task_list.revision,
                        completed_remote_id,
                        "Completed at Google",
                        ItemStatus::Completed,
                        [122; 32],
                    ),
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ItemExecutionActive)
        );
        let item_after: (String, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT status, revision, completed_at, trashed_at FROM items \
                 WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(completed_item_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("task after blocked completion");
        let mapping_after: (Option<String>, Option<Vec<u8>>, Option<i64>, String) = sqlx::query_as(
            "SELECT remote_etag, remote_payload_hash, local_revision, sync_state \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
                   AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(task_list.id)
        .bind(completed_remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mapping after blocked completion");
        assert_eq!(item_after, item_before);
        assert_eq!(mapping_after, mapping_before);
        assert_eq!(
            canonical_mutation_counts(&fixture.database.pool, fixture.scope.workspace_id).await,
            counts_before,
            "a blocked inbound completion must not advance canonical, mapping, cursor, outbox, or audit state",
        );
        close_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            active_session,
            fixture.now + Duration::seconds(3),
        )
        .await;
        assert_eq!(
            fixture
                .repository
                .apply_remote_item(
                    &fixture.claim,
                    remote_task(
                        fixture.account_id,
                        task_list.id,
                        task_list.revision,
                        completed_remote_id,
                        "Completed at Google",
                        ItemStatus::Completed,
                        [122; 32],
                    ),
                    fixture.now + Duration::seconds(4),
                )
                .await
                .expect("completion after lease close"),
            ImportOutcome::Updated
        );

        let deleted_remote_id = "execution-paused-deleted-task";
        fixture
            .repository
            .apply_remote_item(
                &fixture.claim,
                remote_task(
                    fixture.account_id,
                    task_list.id,
                    task_list.revision,
                    deleted_remote_id,
                    "Pause before deletion",
                    ItemStatus::Planned,
                    [123; 32],
                ),
                fixture.now + Duration::seconds(5),
            )
            .await
            .expect("second task import");
        let deleted_item_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(task_list.id)
        .bind(deleted_remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("deleted task identity");
        let paused_session = seed_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            deleted_item_id,
            "paused",
            fixture.now + Duration::seconds(6),
        )
        .await;
        let counts_before =
            canonical_mutation_counts(&fixture.database.pool, fixture.scope.workspace_id).await;
        assert_eq!(
            fixture
                .repository
                .apply_remote_item(
                    &fixture.claim,
                    remote_tombstone(
                        fixture.account_id,
                        task_list.id,
                        task_list.revision,
                        deleted_remote_id,
                        [124; 32],
                    ),
                    fixture.now + Duration::seconds(7),
                )
                .await,
            Err(GoogleSyncRepositoryError::ItemExecutionActive)
        );
        let retained: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT status, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(deleted_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("task retained while paused");
        assert_eq!(retained, ("planned".to_owned(), 1, None));
        assert_eq!(
            canonical_mutation_counts(&fixture.database.pool, fixture.scope.workspace_id).await,
            counts_before,
        );
        close_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            paused_session,
            fixture.now + Duration::seconds(8),
        )
        .await;
        assert_eq!(
            fixture
                .repository
                .apply_remote_item(
                    &fixture.claim,
                    remote_tombstone(
                        fixture.account_id,
                        task_list.id,
                        task_list.revision,
                        deleted_remote_id,
                        [124; 32],
                    ),
                    fixture.now + Duration::seconds(9),
                )
                .await
                .expect("delete after lease close"),
            ImportOutcome::Deleted
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_projection_rolls_back_the_whole_batch_on_active_execution() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar execution guard test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let first_remote = "execution-guard-first-occurrence";
        let active_remote = "execution-guard-later-occurrence";
        fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![
                        projected_occurrence(&fixture, first_remote, "Original first", false, 125),
                        projected_occurrence(&fixture, active_remote, "Active later", false, 126),
                    ],
                ),
                fixture.now,
            )
            .await
            .expect("initial two-member projection");
        let active_item_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(active_remote)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence identity");
        let first_before: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT item.title, item.revision, item.trashed_at FROM items item \
             JOIN provider_sync_mappings mapping ON mapping.workspace_id = item.workspace_id \
               AND mapping.local_entity_id = item.id WHERE mapping.workspace_id = $1 \
               AND mapping.collection_id = $2 AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(first_remote)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("first occurrence before replacement");
        let generation_before: i64 = sqlx::query_scalar(
            "SELECT planning_generation FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("projection generation before replacement");
        let active_session = seed_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            active_item_id,
            "active",
            fixture.now + Duration::seconds(1),
        )
        .await;
        assert_eq!(
            fixture
                .repository
                .replace_calendar_projection(
                    &fixture.claim,
                    projection_batch(
                        &fixture,
                        vec![projected_occurrence(
                            &fixture,
                            first_remote,
                            "Must roll back",
                            false,
                            127,
                        )],
                    ),
                    fixture.now + Duration::seconds(2),
                )
                .await,
            Err(GoogleSyncRepositoryError::ItemExecutionActive)
        );
        let first_after: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT item.title, item.revision, item.trashed_at FROM items item \
             JOIN provider_sync_mappings mapping ON mapping.workspace_id = item.workspace_id \
               AND mapping.local_entity_id = item.id WHERE mapping.workspace_id = $1 \
               AND mapping.collection_id = $2 AND mapping.remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(first_remote)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("first occurrence after rejected replacement");
        let active_trashed_at: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT trashed_at FROM items WHERE workspace_id = $1 AND id = $2")
                .bind(fixture.scope.workspace_id)
                .bind(active_item_id)
                .fetch_one(&fixture.database.pool)
                .await
                .expect("active occurrence retained");
        let generation_after: i64 = sqlx::query_scalar(
            "SELECT planning_generation FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("projection generation after rejection");
        assert_eq!(first_after, first_before);
        assert_eq!(active_trashed_at, None);
        assert_eq!(generation_after, generation_before);
        close_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            active_session,
            fixture.now + Duration::seconds(3),
        )
        .await;
        let accepted = fixture
            .repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![projected_occurrence(
                        &fixture,
                        first_remote,
                        "Must roll back",
                        false,
                        127,
                    )],
                ),
                fixture.now + Duration::seconds(4),
            )
            .await
            .expect("same batch accepted after lease close");
        assert!(accepted.complete);
        let accepted_state: (String, bool) = sqlx::query_as(
            "SELECT \
             (SELECT item.title FROM items item JOIN provider_sync_mappings mapping \
               ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
               WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
                 AND mapping.remote_resource_id = $3), \
             (SELECT trashed_at IS NOT NULL FROM items WHERE workspace_id = $1 AND id = $4)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(first_remote)
        .bind(active_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("accepted projection state");
        assert_eq!(accepted_state, ("Must roll back".to_owned(), true));
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Keeps the deterministic lock choreography and rollback assertions together.
    async fn postgres_inbound_close_and_execution_start_serialize_without_deadlock() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; inbound/Start race test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let remote_id = "execution-inbound-race";
        fixture
            .repository
            .apply_remote_item(
                &fixture.claim,
                remote_event(
                    fixture.account_id,
                    fixture.collection.id,
                    fixture.collection.revision,
                    remote_id,
                    "Inbound race",
                    [128; 32],
                ),
                fixture.now,
            )
            .await
            .expect("race item imported");
        let item_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND remote_resource_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(remote_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("race item identity");
        let clock = Arc::new(SystemClock);
        let items = Arc::new(ItemService::new(
            Arc::new(PostgresItemRepository::new(
                fixture.database.pool.clone(),
                fixture.scope,
            )),
            clock.clone(),
        ));
        let execution = Arc::new(ExecutionService::new(
            Arc::new(PostgresExecutionRepository::new(
                fixture.database.pool.clone(),
                fixture.scope,
            )),
            items,
            clock,
        ));

        // Stop the inbound transaction after it owns execution_state but before it can enter
        // canonical item space. Start must queue behind state, then reject the committed close.
        let mut canonical_blocker = fixture
            .database
            .pool
            .begin()
            .await
            .expect("begin canonical blocker");
        super::super::database::lock_canonical_item_space(
            &mut canonical_blocker,
            fixture.scope.workspace_id,
        )
        .await
        .expect("hold canonical item space");
        let mut completed = remote_event(
            fixture.account_id,
            fixture.collection.id,
            fixture.collection.revision,
            remote_id,
            "Completed remotely",
            [129; 32],
        );
        completed.item.as_mut().expect("completed item").status = ItemStatus::Completed;
        let inbound_task = {
            let repository = fixture.repository.clone();
            let claim = fixture.claim.clone();
            tokio::spawn(async move {
                repository
                    .apply_remote_item(&claim, completed, fixture.now + Duration::seconds(1))
                    .await
            })
        };
        wait_until_execution_state_is_locked(&fixture.database.pool, fixture.scope).await;
        let session_id = Uuid::new_v4();
        let start_task = {
            let execution = execution.clone();
            tokio::spawn(async move {
                execution
                    .command(
                        0,
                        ExecutionCommand::Start(StartExecution {
                            session_id,
                            item_id,
                            item_revision: 1,
                            occurrence_id: None,
                            session_index: 0,
                            planned_block_id: None,
                            device_id: Uuid::new_v4(),
                        }),
                        ExecutionIdempotencyKey {
                            key: "google-inbound-start-race-001".to_owned(),
                            fingerprint: [130; 32],
                        },
                    )
                    .await
            })
        };
        canonical_blocker
            .commit()
            .await
            .expect("release canonical item space");
        let (inbound, start) = tokio::time::timeout(StdDuration::from_secs(10), async {
            tokio::join!(inbound_task, start_task)
        })
        .await
        .expect("inbound/Start race completes without deadlock");
        assert_eq!(
            inbound
                .expect("inbound task joins")
                .expect("inbound close wins"),
            ImportOutcome::Updated
        );
        assert!(matches!(
            start.expect("Start task joins"),
            Err(ExecutionServiceError::Repository(
                ExecutionRepositoryError::ItemRevisionConflict
            ))
        ));
        let state: (String, i64, Option<Uuid>) = sqlx::query_as(
            "SELECT item.status, item.revision, state.active_session_id FROM items item \
             JOIN execution_state state ON state.workspace_id = item.workspace_id \
             WHERE item.workspace_id = $1 AND item.id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("race terminal state");
        assert_eq!(state, ("completed".to_owned(), 2, None));
        let start_fence_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1 \
             AND namespace = 'execution.command'",
        )
        .bind(fixture.scope.workspace_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("failed Start fence rollback");
        assert_eq!(start_fence_count, 0);
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
            .enqueue_test_outbound(
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
        let stale_permit = repository
            .authorize_outbound_dispatch(&stale_work, true, fixture.now)
            .await
            .expect("stale response dispatch authorized before the race");
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
                        dispatch_nonce: stale_permit.nonce,
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
            .enqueue_test_outbound(
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
        let pause_permit = repository
            .authorize_outbound_dispatch(&pause_work, true, fixture.now)
            .await
            .expect("pause dispatch authorized before the race");
        repository
            .replace_calendar_projection(
                &fixture.claim,
                projection_batch(
                    &fixture,
                    vec![
                        projected_occurrence(
                            &fixture,
                            "pause-projected-occurrence",
                            "Projected meeting retired on pause",
                            false,
                            96,
                        ),
                        projected_occurrence(
                            &fixture,
                            "pause-active-occurrence",
                            "Active meeting blocks pause",
                            false,
                            97,
                        ),
                    ],
                ),
                fixture.now,
            )
            .await
            .expect("complete projection before account pause");
        let pause_projection_identity: (Uuid, Uuid, Option<i64>) = sqlx::query_as(
            "SELECT id, local_entity_id, local_revision FROM provider_sync_mappings \
             WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = 'pause-projected-occurrence'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("projected occurrence identity before pause");
        let pause_projected_item_id = pause_projection_identity.1;
        let pause_active_item_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = 'pause-active-occurrence'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active projected occurrence identity before pause");
        assert_eq!(pause_projection_identity.2, Some(1));
        sqlx::query(
            "UPDATE items SET title = 'Locally edited before account pause', \
             revision = revision + 1, updated_at = $3 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(pause_projected_item_id)
        .bind(fixture.now + Duration::seconds(30))
        .execute(&fixture.database.pool)
        .await
        .expect("local edit before account pause");
        let oauth =
            PostgresGoogleOAuthRepository::new(fixture.database.pool.clone(), fixture.scope);
        let pause_session = seed_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            pause_active_item_id,
            "active",
            fixture.now + Duration::seconds(31),
        )
        .await;
        let pause_active_before: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(pause_active_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence before account pause");
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
            .expect("security pause commits while an occurrence is executing");
        let pause_active_after: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(pause_active_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence after account pause");
        let pause_active_mapping: (String, Option<DateTime<Utc>>, Option<String>) = sqlx::query_as(
            "SELECT sync_state, tombstoned_at, conflict_metadata->>'reason' \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
                   AND remote_resource_id = 'pause-active-occurrence'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence mapping detached by account pause");
        let active_lease: (String, Option<Uuid>) = sqlx::query_as(
            "SELECT session.state, state.active_session_id FROM execution_sessions session \
             JOIN execution_state state ON state.workspace_id = session.workspace_id \
             WHERE session.workspace_id = $1 AND session.id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(pause_session)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("execution lease survives account pause");
        assert_eq!(pause_active_after, pause_active_before);
        assert_eq!(pause_active_mapping.0, "conflict");
        assert_eq!(
            pause_active_mapping.1,
            Some(fixture.now + Duration::minutes(1))
        );
        assert_eq!(
            pause_active_mapping.2.as_deref(),
            Some("calendar_occurrence_configuration_retired_execution_active")
        );
        assert_eq!(active_lease, ("active".to_owned(), Some(pause_session)));
        close_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            pause_session,
            fixture.now + Duration::minutes(1) + Duration::seconds(1),
        )
        .await;
        let pause_projection_state: (String, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT planning_projection_state, planning_window_start \
             FROM google_sync_collections WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("pause tears down projected Calendar truth");
        assert_eq!(pause_projection_state, ("uninitialized".to_owned(), None));
        let pause_preserved_item: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(pause_projected_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("account pause preserves the locally edited occurrence");
        assert_eq!(
            pause_preserved_item,
            ("Locally edited before account pause".to_owned(), 2, None)
        );
        let pause_retired_mapping: (String, Option<DateTime<Utc>>, Option<i64>, Option<Value>) =
            sqlx::query_as(
                "SELECT sync_state, tombstoned_at, local_revision, conflict_metadata \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
            )
            .bind(fixture.scope.workspace_id)
            .bind(pause_projection_identity.0)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("account pause preserves historical provider mapping");
        assert_eq!(pause_retired_mapping.0, "conflict");
        assert_eq!(
            pause_retired_mapping.1,
            Some(fixture.now + Duration::minutes(1))
        );
        assert_eq!(pause_retired_mapping.2, Some(1));
        let pause_conflict = pause_retired_mapping
            .3
            .expect("account pause local-only conflict metadata");
        assert_eq!(
            pause_conflict,
            json!({
                "reason": "calendar_occurrence_configuration_retired_local_changed",
                "local_item_id": pause_projected_item_id,
                "mapping_local_revision": 1,
                "item_revision": 2
            })
        );
        assert!(
            !pause_conflict
                .to_string()
                .contains("pause-projected-occurrence")
        );
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
                        dispatch_nonce: pause_permit.nonce,
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
        repository
            .replace_calendar_projection(
                &resumed_claim,
                projection_batch(
                    &fixture,
                    vec![projected_occurrence(
                        &fixture,
                        "disconnect-active-occurrence",
                        "Active meeting blocks disconnect",
                        false,
                        98,
                    )],
                ),
                fixture.now + Duration::minutes(2),
            )
            .await
            .expect("complete projection before blocked disconnect");
        let disconnect_active_item_id: Uuid = sqlx::query_scalar(
            "SELECT local_entity_id FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND entity_kind = 'calendar_occurrence' \
             AND remote_resource_id = 'disconnect-active-occurrence'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active projected occurrence identity before disconnect");
        let disconnect_session = seed_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            disconnect_active_item_id,
            "paused",
            fixture.now + Duration::minutes(2) + Duration::seconds(1),
        )
        .await;
        let disconnect_item_before: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(disconnect_active_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence before disconnect");
        let disconnect_claim_id = Uuid::new_v4();
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
                    GoogleCalendarPolicy::default(),
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
                disconnect_claim_id,
                fixture.now + Duration::minutes(3),
                fixture.now,
                fixture.now,
                oauth_idempotency("google_oauth_disconnect", 74, fixture.now),
            )
            .await
            .expect("disconnect guardian commits while an occurrence is executing");
        let disconnect_item_after: (String, i64, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT title, revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(disconnect_active_item_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence after disconnect claim");
        let disconnect_mapping: (String, Option<DateTime<Utc>>, Option<String>) = sqlx::query_as(
            "SELECT sync_state, tombstoned_at, conflict_metadata->>'reason' \
                 FROM provider_sync_mappings WHERE workspace_id = $1 AND collection_id = $2 \
                   AND remote_resource_id = 'disconnect-active-occurrence'",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("active occurrence mapping detached by disconnect");
        let disconnect_lease: (String, Option<Uuid>) = sqlx::query_as(
            "SELECT session.state, state.active_session_id FROM execution_sessions session \
             JOIN execution_state state ON state.workspace_id = session.workspace_id \
             WHERE session.workspace_id = $1 AND session.id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(disconnect_session)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("execution lease survives disconnect claim");
        assert_eq!(disconnect_item_after, disconnect_item_before);
        assert_eq!(disconnect_mapping.0, "conflict");
        assert_eq!(
            disconnect_mapping.1,
            Some(fixture.now + Duration::minutes(3))
        );
        assert_eq!(
            disconnect_mapping.2.as_deref(),
            Some("calendar_occurrence_configuration_retired_execution_active")
        );
        assert_eq!(
            disconnect_lease,
            ("paused".to_owned(), Some(disconnect_session))
        );
        close_execution_lease(
            &fixture.database.pool,
            fixture.scope,
            disconnect_session,
            fixture.now + Duration::minutes(3) + Duration::seconds(1),
        )
        .await;
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
                GoogleCalendarPolicy::default(),
                now,
            )
            .await
            .expect("writable owner calendar");

        repository
            .request_refresh(account_id, Uuid::new_v4(), now)
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
        reprojection
            .item
            .as_mut()
            .expect("projected item")
            .is_sensitive = true;
        reprojection.remote_projection_hash = [7; 32];
        assert_eq!(
            repository
                .apply_remote_item(&claim, reprojection.clone(), now + Duration::minutes(5))
                .await
                .expect("unchanged provider payload re-projected"),
            ImportOutcome::Updated
        );
        let promoted: bool = sqlx::query_scalar(
            "SELECT item.is_sensitive FROM items item \
             JOIN provider_sync_mappings mapping ON mapping.local_entity_id = item.id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id = 'remote-3'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("promoted imported sensitivity");
        assert!(
            promoted,
            "visible-to-private replay must promote sensitivity"
        );

        reprojection.item.as_mut().expect("projected item").title =
            "SYNTHETIC-VISIBLE-REPLAY-CANARY".to_owned();
        reprojection
            .item
            .as_mut()
            .expect("projected item")
            .is_sensitive = false;
        reprojection.remote_projection_hash = [8; 32];
        assert_eq!(
            repository
                .apply_remote_item(&claim, reprojection, now + Duration::minutes(6))
                .await
                .expect("visible replay after private import"),
            ImportOutcome::Updated
        );
        let retained: bool = sqlx::query_scalar(
            "SELECT item.is_sensitive FROM items item \
             JOIN provider_sync_mappings mapping ON mapping.local_entity_id = item.id \
             WHERE mapping.workspace_id = $1 AND mapping.collection_id = $2 \
               AND mapping.remote_resource_id = 'remote-3'",
        )
        .bind(scope.workspace_id)
        .bind(collection.id)
        .fetch_one(&database.pool)
        .await
        .expect("retained imported sensitivity");
        assert!(
            retained,
            "provider replay must not declassify a sensitive item"
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
                GoogleCalendarPolicy::default(),
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
                is_sensitive: false,
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
            .enqueue_test_outbound(account_id, prepared.clone(), collection.id, now)
            .await
            .expect("outbound queued");
        let replay = repository
            .enqueue_test_outbound(account_id, prepared, collection.id, now)
            .await
            .expect("outbound replay");
        assert_eq!(queued.outbox_id, replay.outbox_id);
        assert!(replay.replayed);
        let mut observed_before_ack = remote_event(
            account_id,
            collection.id,
            reconfigured.revision,
            "stable-provider-id",
            "DayWeave firm block",
            [9; 32],
        );
        observed_before_ack.dayweave_item_id = Some(local.id);
        observed_before_ack.reviewed_provider_projection =
            Some(json!({"id": "stable-provider-id"}));
        assert_eq!(
            repository
                .apply_remote_item(&claim, observed_before_ack, now + Duration::seconds(15))
                .await
                .expect("exact provider create recovered through durable proof"),
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
            .enqueue_test_outbound(
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
        assert_eq!(superseded_state, "published");
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
            .request_refresh(account_id, Uuid::new_v4(), now + Duration::minutes(2))
            .await
            .expect("manual refresh advances retained delivery");
        let work = repository
            .claim_outbound(&claim, now + Duration::minutes(2))
            .await
            .expect("reclaim outbound")
            .expect("backoff advanced by manual refresh");
        let permit = repository
            .authorize_outbound_dispatch(&work, true, now + Duration::minutes(2))
            .await
            .expect("final dispatch authorization");
        repository
            .complete_outbound(
                &work,
                OutboundResult {
                    remote_resource_id: "stable-provider-id".to_owned(),
                    remote_etag: Some("etag-1".to_owned()),
                    remote_updated_at: Some(now),
                    payload_hash: [9; 32],
                    dispatch_nonce: permit.nonce,
                },
                now + Duration::minutes(2),
            )
            .await
            .expect("publish acknowledgement");
        let immutable_reviewed_identity: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT remote_resource_id, expected_etag FROM google_sync_outbox WHERE id = $1",
        )
        .bind(work.id)
        .fetch_one(&database.pool)
        .await
        .expect("immutable reviewed provider identity");
        assert_eq!(
            immutable_reviewed_identity,
            (
                Some("stable-provider-id".to_owned()),
                Some("etag-stable-provider-id".to_owned())
            ),
            "publication must not rewrite the provider ID/ETag bound to approval"
        );
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
            .enqueue_test_outbound(
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
                is_sensitive: false,
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
            .enqueue_test_outbound(
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
            .enqueue_test_outbound(
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

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_parent_run_takeover_fences_authorize_complete_and_claim_aba() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; parent-run fence test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        let item = local_firm_block(Uuid::new_v4(), "Parent run fence", fixture.now);
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &item)
            .await
            .expect("item fixture");
        transaction.commit().await.expect("item commit");
        let accepted = fixture
            .repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": "parent-run-fence", "summary": "Parent run fence"}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("queued");
        let stale_work = fixture
            .repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("claimed")
            .expect("work");

        let takeover_at = fixture.now + Duration::minutes(11);
        let second_claim = fixture
            .repository
            .claim_due(takeover_at, takeover_at + Duration::minutes(10))
            .await
            .expect("takeover query")
            .expect("second parent claim");
        assert!(second_claim.claim_generation > fixture.claim.claim_generation);
        assert!(matches!(
            fixture
                .repository
                .authorize_outbound_dispatch(&stale_work, true, takeover_at)
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        ));
        assert_eq!(
            fixture
                .repository
                .renew_claim(
                    &fixture.claim,
                    takeover_at,
                    takeover_at + Duration::minutes(10),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost),
            "the old claim ID and generation cannot survive takeover"
        );
        let after_first_takeover: (String, String) =
            sqlx::query_as("SELECT state, last_error_code FROM google_sync_outbox WHERE id = $1")
                .bind(accepted.outbox_id)
                .fetch_one(&fixture.database.pool)
                .await
                .expect("reconciled outbox");
        assert_eq!(
            after_first_takeover,
            (
                "backoff".to_owned(),
                "parent_run_lease_expired_before_send".to_owned()
            )
        );

        let second_work = fixture
            .repository
            .claim_outbound(&second_claim, takeover_at)
            .await
            .expect("second claim outbound")
            .expect("reclaimed work");
        let permit = fixture
            .repository
            .authorize_outbound_dispatch(&second_work, true, takeover_at)
            .await
            .expect("second run authorization");
        let second_takeover_at = takeover_at + Duration::minutes(11);
        let third_claim = fixture
            .repository
            .claim_due(
                second_takeover_at,
                second_takeover_at + Duration::minutes(10),
            )
            .await
            .expect("second takeover query")
            .expect("third parent claim");
        assert!(third_claim.claim_generation > second_claim.claim_generation);
        assert_eq!(
            fixture
                .repository
                .complete_outbound(
                    &second_work,
                    OutboundResult {
                        remote_resource_id: "parent-run-fence".to_owned(),
                        remote_etag: Some("etag-parent-run-fence".to_owned()),
                        remote_updated_at: Some(second_takeover_at),
                        payload_hash: [81; 32],
                        dispatch_nonce: permit.nonce,
                    },
                    second_takeover_at,
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost),
            "a slow response cannot publish after parent takeover"
        );
        let mapping_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_sync_mappings WHERE workspace_id = $1 \
             AND collection_id = $2 AND local_entity_id = $3)",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.collection.id)
        .bind(item.id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("mapping absence");
        assert!(!mapping_exists);
        fixture.database.destroy().await;
    }

    #[derive(Clone, Copy, Debug)]
    enum OutboundParentLockStage {
        Renew,
        Authorize,
        Cancel,
        Fail,
    }

    #[tokio::test]
    async fn postgres_every_outbound_stage_serializes_parent_takeover() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; parent lock race test skipped");
            return;
        };
        for stage in [
            OutboundParentLockStage::Renew,
            OutboundParentLockStage::Authorize,
            OutboundParentLockStage::Cancel,
            OutboundParentLockStage::Fail,
        ] {
            assert_outbound_stage_serializes_takeover(&database_url, stage).await;
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn assert_outbound_stage_serializes_takeover(
        database_url: &str,
        stage: OutboundParentLockStage,
    ) {
        let fixture = sync_fixture(database_url).await;
        let item = local_firm_block(
            Uuid::new_v4(),
            &format!("Parent lock {stage:?}"),
            fixture.now,
        );
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("item transaction");
        insert_imported_item(&mut transaction, fixture.scope, &item)
            .await
            .expect("item fixture");
        transaction.commit().await.expect("item commit");
        fixture
            .repository
            .enqueue_test_outbound(
                fixture.account_id,
                PreparedOutbound {
                    entity_kind: "calendar_event",
                    item: item.clone(),
                    operation: OutboundOperation::Upsert,
                    payload: json!({"id": format!("parent-lock-{}", item.id.simple())}),
                },
                fixture.collection.id,
                fixture.now,
            )
            .await
            .expect("queued");
        let work = fixture
            .repository
            .claim_outbound(&fixture.claim, fixture.now)
            .await
            .expect("claim query")
            .expect("work");
        if matches!(stage, OutboundParentLockStage::Cancel) {
            fixture
                .repository
                .authorize_outbound_dispatch(&work, true, fixture.now)
                .await
                .expect("pre-cancel authorization");
        }

        // Hold the child row so the stage must pause after acquiring its
        // shared parent-run lock and before its own child mutation.
        let mut outbox_gate = fixture
            .database
            .pool
            .begin()
            .await
            .expect("outbox gate transaction");
        sqlx::query("SELECT 1 FROM google_sync_outbox WHERE id = $1 FOR UPDATE")
            .bind(work.id)
            .fetch_one(&mut *outbox_gate)
            .await
            .expect("outbox row locked");
        let stage_repository = fixture.repository.clone();
        let stage_work = work.clone();
        let stage_now = fixture.now;
        let stage_task = tokio::spawn(async move {
            match stage {
                OutboundParentLockStage::Renew => {
                    stage_repository
                        .renew_outbound(&stage_work, stage_now)
                        .await
                }
                OutboundParentLockStage::Authorize => stage_repository
                    .authorize_outbound_dispatch(&stage_work, true, stage_now)
                    .await
                    .map(|_| ()),
                OutboundParentLockStage::Cancel => {
                    stage_repository
                        .cancel_outbound_before_send(
                            &stage_work,
                            "synthetic_pre_send_cancel",
                            stage_now,
                            stage_now,
                        )
                        .await
                }
                OutboundParentLockStage::Fail => {
                    stage_repository
                        .fail_outbound(
                            &stage_work,
                            "backoff",
                            "synthetic_pre_send_failure",
                            stage_now,
                            stage_now,
                        )
                        .await
                }
            }
        });

        let mut parent_lock_observed = false;
        for _ in 0..100 {
            let lock_attempt = sqlx::query(
                "SELECT 1 FROM google_sync_runs WHERE workspace_id = $1 \
                 AND provider_account_id = $2 FOR UPDATE NOWAIT",
            )
            .bind(fixture.scope.workspace_id)
            .bind(fixture.account_id)
            .fetch_optional(&fixture.database.pool)
            .await;
            if matches!(
                lock_attempt,
                Err(sqlx::Error::Database(ref error))
                    if error.code().as_deref() == Some("55P03")
            ) {
                parent_lock_observed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            parent_lock_observed,
            "{stage:?} must lock the exact parent before waiting on the child"
        );

        let takeover_pool = fixture.database.pool.clone();
        let takeover_scope = fixture.scope;
        let takeover_account = fixture.account_id;
        let old_parent_claim = fixture.claim.clone();
        let replacement_claim_id = Uuid::new_v4();
        let takeover = tokio::spawn(async move {
            sqlx::query(
                "UPDATE google_sync_runs SET claim_id = $6, \
                 claim_generation = claim_generation + 1, revision = revision + 1 \
                 WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
                   AND claim_id = $4 AND claim_generation = $5",
            )
            .bind(takeover_scope.workspace_id)
            .bind(takeover_scope.user_id)
            .bind(takeover_account)
            .bind(old_parent_claim.claim_id)
            .bind(u64_to_i64(old_parent_claim.claim_generation).expect("generation"))
            .bind(replacement_claim_id)
            .execute(&takeover_pool)
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !takeover.is_finished(),
            "{stage:?} must exclude parent takeover until its child transition commits"
        );
        outbox_gate.commit().await.expect("release outbox gate");
        stage_task
            .await
            .expect("stage task joined")
            .expect("stage completed before takeover");
        assert_eq!(
            takeover
                .await
                .expect("takeover task joined")
                .expect("takeover query")
                .rows_affected(),
            1
        );
        assert_eq!(
            fixture
                .repository
                .renew_claim(
                    &fixture.claim,
                    fixture.now,
                    fixture.now + Duration::minutes(10),
                )
                .await,
            Err(GoogleSyncRepositoryError::ClaimLost)
        );
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_claim_and_authorize_both_fence_mapping_identity_changes() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; mapping-stage fence test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        for (suffix, mutate_after_claim) in [("claim", false), ("authorize", true)] {
            let item = local_firm_block(
                Uuid::new_v4(),
                &format!("Mapping fence {suffix}"),
                fixture.now,
            );
            let mut transaction = fixture
                .database
                .pool
                .begin()
                .await
                .expect("item transaction");
            insert_imported_item(&mut transaction, fixture.scope, &item)
                .await
                .expect("item fixture");
            transaction.commit().await.expect("item commit");
            let remote_id = format!("mapping-fence-{suffix}");
            let accepted = fixture
                .repository
                .enqueue_test_outbound(
                    fixture.account_id,
                    PreparedOutbound {
                        entity_kind: "calendar_event",
                        item: item.clone(),
                        operation: OutboundOperation::Upsert,
                        payload: json!({"id": remote_id}),
                    },
                    fixture.collection.id,
                    fixture.now,
                )
                .await
                .expect("queued");
            let work = if mutate_after_claim {
                Some(
                    fixture
                        .repository
                        .claim_outbound(&fixture.claim, fixture.now)
                        .await
                        .expect("claim query")
                        .expect("claimed work"),
                )
            } else {
                None
            };
            let mut change = remote_event(
                fixture.account_id,
                fixture.collection.id,
                fixture.collection.revision,
                &remote_id,
                "Concurrent provider identity",
                [82; 32],
            );
            change.dayweave_item_id = Some(item.id);
            let mut transaction = fixture
                .database
                .pool
                .begin()
                .await
                .expect("mapping transaction");
            insert_mapping(
                &mut transaction,
                fixture.scope,
                &change,
                Some(item.id),
                Some(1),
                "synced",
                "dayweave",
                None,
                fixture.now,
            )
            .await
            .expect("concurrent mapping");
            transaction.commit().await.expect("mapping commit");
            if let Some(work) = work {
                assert!(matches!(
                    fixture
                        .repository
                        .authorize_outbound_dispatch(&work, true, fixture.now)
                        .await,
                    Err(GoogleSyncRepositoryError::ClaimLost)
                ));
            } else {
                assert!(
                    fixture
                        .repository
                        .claim_outbound(&fixture.claim, fixture.now)
                        .await
                        .expect("claim-stage mapping scan")
                        .is_none()
                );
            }
            let state: (String, String) = sqlx::query_as(
                "SELECT state, last_error_code FROM google_sync_outbox WHERE id = $1",
            )
            .bind(accepted.outbox_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("mapping-fenced state");
            assert_eq!(state.0, "conflict");
            assert!(matches!(
                state.1.as_str(),
                "provider_mapping_changed_before_claim" | "dispatch_authorization_denied"
            ));
        }
        fixture.database.destroy().await;
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn postgres_calendar_recovery_requires_complete_reviewed_semantics() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar recovery test skipped");
            return;
        };
        let fixture = sync_fixture(&database_url).await;
        for (suffix, provider_summary, expected_ownership) in [
            ("exact", "Reviewed summary", "dayweave"),
            ("edited", "Externally edited", "external"),
        ] {
            let item = local_firm_block(Uuid::new_v4(), &format!("Recovery {suffix}"), fixture.now);
            let mut transaction = fixture
                .database
                .pool
                .begin()
                .await
                .expect("item transaction");
            insert_imported_item(&mut transaction, fixture.scope, &item)
                .await
                .expect("item fixture");
            transaction.commit().await.expect("item commit");
            let remote_id = format!("recovery-{suffix}");
            let reviewed = json!({
                "id": remote_id,
                "summary": "Reviewed summary",
                "description": "Reviewed description",
                "extendedProperties": {"private": {"proof": "synthetic"}}
            });
            let accepted = fixture
                .repository
                .enqueue_test_outbound(
                    fixture.account_id,
                    PreparedOutbound {
                        entity_kind: "calendar_event",
                        item: item.clone(),
                        operation: OutboundOperation::Upsert,
                        payload: reviewed.clone(),
                    },
                    fixture.collection.id,
                    fixture.now,
                )
                .await
                .expect("queued create");
            if suffix == "exact" {
                sqlx::query(
                    "UPDATE items SET title = 'Newer local revision', revision = 2, updated_at = $3 \
                     WHERE workspace_id = $1 AND id = $2",
                )
                .bind(fixture.scope.workspace_id)
                .bind(item.id)
                .bind(fixture.now + Duration::seconds(1))
                .execute(&fixture.database.pool)
                .await
                .expect("newer local revision");
                let mut newer_item = item.clone();
                newer_item.title = "Newer local revision".to_owned();
                newer_item.revision = 2;
                newer_item.updated_at = fixture.now + Duration::seconds(1);
                fixture
                    .repository
                    .enqueue_test_outbound(
                        fixture.account_id,
                        PreparedOutbound {
                            entity_kind: "calendar_event",
                            item: newer_item,
                            operation: OutboundOperation::Upsert,
                            payload: json!({
                                "id": remote_id,
                                "summary": "Newer local revision",
                                "description": "Reviewed description",
                                "extendedProperties": {"private": {"proof": "synthetic"}}
                            }),
                        },
                        fixture.collection.id,
                        fixture.now + Duration::seconds(1),
                    )
                    .await
                    .expect("newer reviewed create queued before recovery");
                let original_state: String =
                    sqlx::query_scalar("SELECT state FROM google_sync_outbox WHERE id = $1")
                        .bind(accepted.outbox_id)
                        .fetch_one(&fixture.database.pool)
                        .await
                        .expect("superseded original");
                assert_eq!(original_state, "superseded");
            }
            let mut observed = remote_event(
                fixture.account_id,
                fixture.collection.id,
                fixture.collection.revision,
                &remote_id,
                provider_summary,
                [83; 32],
            );
            observed.dayweave_item_id = Some(item.id);
            observed.reviewed_provider_projection = Some(json!({
                "id": remote_id,
                "summary": provider_summary,
                "description": "Reviewed description",
                "extendedProperties": {"private": {"proof": "synthetic"}}
            }));
            let outcome = fixture
                .repository
                .apply_remote_item(&fixture.claim, observed, fixture.now)
                .await
                .expect("inbound recovery");
            assert_eq!(
                outcome,
                if suffix == "exact" {
                    ImportOutcome::Unchanged
                } else {
                    ImportOutcome::Conflict
                }
            );
            let mapping: (Option<Uuid>, String) = sqlx::query_as(
                "SELECT local_entity_id, ownership FROM provider_sync_mappings \
                 WHERE workspace_id = $1 AND collection_id = $2 AND remote_resource_id = $3",
            )
            .bind(fixture.scope.workspace_id)
            .bind(fixture.collection.id)
            .bind(&remote_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("recovery mapping");
            assert_eq!(mapping.1, expected_ownership);
            assert_eq!(mapping.0, (suffix == "exact").then_some(item.id));
            let outbox: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
                "SELECT state, remote_resource_id, expected_etag, last_error_code \
                     FROM google_sync_outbox WHERE id = $1",
            )
            .bind(accepted.outbox_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("recovery outbox");
            if suffix == "exact" {
                assert_eq!(outbox, ("published".to_owned(), None, None, None));
            } else {
                assert_eq!(outbox.0, "conflict");
                assert_eq!(
                    outbox.3.as_deref(),
                    Some("provider_semantics_changed_before_recovery")
                );
            }
        }
        fixture.database.destroy().await;
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
            reviewed_provider_projection: None,
            item: Some(NewItem {
                id: Uuid::new_v4(),
                is_sensitive: false,
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

    fn remote_task(
        account_id: Uuid,
        collection_id: Uuid,
        collection_revision: u64,
        remote_id: &str,
        title: &str,
        status: ItemStatus,
        hash: [u8; 32],
    ) -> RemoteItemChange {
        RemoteItemChange {
            account_id,
            collection_id,
            collection_revision,
            dayweave_item_id: None,
            remote_id: remote_id.to_owned(),
            remote_parent_id: None,
            remote_etag: Some(format!("etag-{remote_id}-{}", hash[0])),
            remote_updated_at: None,
            remote_payload_hash: hash,
            remote_projection_hash: hash,
            reviewed_provider_projection: None,
            item: Some(NewItem {
                id: Uuid::new_v4(),
                is_sensitive: false,
                kind: ItemKind::Task,
                status,
                title: title.to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_seconds: Some(1_800),
                deadline_at: None,
                earliest_start_at: None,
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
            reviewed_provider_projection: None,
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
