use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::google_oauth::MAX_CLEANUP_ATTEMPTS;
use crate::google_oauth::{
    AccountSecretSnapshot, AuthorizationCompletion, AuthorizationResolution, CallbackClaim,
    ClaimedOAuthSession, CleanupClaim, DisconnectClaim, DisconnectMutation, EncryptedCredentials,
    GoogleAccount, GoogleAccountMutation, GoogleAccountStatus, GoogleOAuthCleanupStatus,
    GoogleOAuthRepository, GoogleOAuthRepositoryError, NewOAuthSession, OAuthIdempotency,
    OAuthSessionStart, OperatorRecoveryResult, RevocationFenceClaim, SealedSecret, SecretHash,
};

use super::{
    DatabaseScope, google_sync_repository::retire_active_calendar_occurrences_for_account,
    lock_execution_and_canonical_item_space,
};

const ACCOUNT_COLUMNS: &str = "id, external_account_id, display_label, status, sync_enabled, \
    is_default, granted_scopes, token_expires_at, revision, created_at, updated_at, \
    encrypted_credentials, credential_key_version";
const SESSION_RESOURCE: &str = "google_oauth_session";
const ACCOUNT_RESOURCE: &str = "google_account";
const DISCONNECT_RESOURCE: &str = "google_disconnect";

#[derive(Clone, Copy)]
enum SchedulePublicationAccountTransition {
    Retry,
    Conflict,
}

#[derive(Clone, Debug)]
pub struct PostgresGoogleOAuthRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresGoogleOAuthRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }
}

#[async_trait]
impl GoogleOAuthRepository for PostgresGoogleOAuthRepository {
    async fn preflight_cleanup_storage(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM google_oauth_sessions WHERE workspace_id = $1 \
             AND user_id = $2 AND id = $3 AND status = 'exchanging')",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if !valid {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        // A harmless scoped write exercises the durable store before Google
        // can issue a refresh token. It does not claim that future writes
        // cannot fail; post-exchange ownership still uses exact-idempotent hold.
        sqlx::query(
            "UPDATE google_oauth_scope_state SET credential_generation = credential_generation \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let _ = now;
        transaction.commit().await.map_err(internal)
    }

    #[allow(clippy::too_many_lines)]
    async fn create_session(
        &self,
        session: NewOAuthSession,
        idempotency: OAuthIdempotency,
        exchange_stale_before: DateTime<Utc>,
    ) -> Result<OAuthSessionStart, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        match lookup_idempotency(
            &mut transaction,
            self.scope,
            &idempotency,
            session.created_at,
        )
        .await?
        {
            IdempotencyState::Completed {
                resource_type,
                resource_id: Some(session_id),
                ..
            } if resource_type.as_deref() == Some(SESSION_RESOURCE) => {
                let row = sqlx::query(
                    "SELECT encrypted_authorization_url, authorization_url_key_version, expires_at \
                     FROM google_oauth_sessions WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(session_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(internal)?
                .ok_or(GoogleOAuthRepositoryError::Internal)?;
                let started = OAuthSessionStart {
                    id: session_id,
                    encrypted_authorization_url: SealedSecret {
                        key_version: version_from_i32(
                            row.try_get("authorization_url_key_version")
                                .map_err(internal)?,
                        )?,
                        ciphertext: row
                            .try_get("encrypted_authorization_url")
                            .map_err(internal)?,
                    },
                    expires_at: row.try_get("expires_at").map_err(internal)?,
                    replayed: true,
                };
                transaction.commit().await.map_err(internal)?;
                return Ok(started);
            }
            IdempotencyState::Completed { .. } => {
                return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
            }
            IdempotencyState::InProgress { .. } => {
                return Err(GoogleOAuthRepositoryError::IdempotencyInProgress);
            }
            IdempotencyState::Absent => {}
        }

        cleanup_stale_sessions(
            &mut transaction,
            self.scope,
            session.created_at,
            exchange_stale_before,
        )
        .await?;
        ensure_no_open_exchange(&mut transaction, self.scope).await?;
        let current = match session.expected_account_id {
            Some(account_id) => {
                fetch_optional_account_by_id(&mut transaction, self.scope, account_id, true).await?
            }
            None => None,
        };
        ensure_expected_account(
            current.as_ref(),
            session.expected_account_id,
            session.expected_account_revision,
        )?;
        if current
            .as_ref()
            .is_some_and(|snapshot| snapshot.account.status == GoogleAccountStatus::Disconnecting)
        {
            return Err(GoogleOAuthRepositoryError::AccountStateConflict);
        }
        sqlx::query(
            "UPDATE google_oauth_sessions SET status = 'failed', failed_at = $3, \
             encrypted_pkce_verifier = NULL, verifier_key_version = NULL \
             WHERE workspace_id = $1 AND user_id = $2 AND status = 'pending'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session.created_at)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;

        let started = OAuthSessionStart {
            id: session.id,
            encrypted_authorization_url: session.encrypted_authorization_url.clone(),
            expires_at: session.expires_at,
            replayed: false,
        };
        let insert = sqlx::query(
            "INSERT INTO google_oauth_sessions (id, workspace_id, user_id, owner_subject_hash, \
             state_hash, encrypted_pkce_verifier, verifier_key_version, \
             encrypted_authorization_url, authorization_url_key_version, requested_scopes, \
             expected_account_id, expected_account_revision, make_default, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(session.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session.owner_subject_hash.as_slice())
        .bind(session.state_hash.as_slice())
        .bind(session.encrypted_verifier.ciphertext.as_slice())
        .bind(version_to_i32(session.encrypted_verifier.key_version)?)
        .bind(session.encrypted_authorization_url.ciphertext.as_slice())
        .bind(version_to_i32(
            session.encrypted_authorization_url.key_version,
        )?)
        .bind(session.requested_scopes.into_iter().collect::<Vec<_>>())
        .bind(session.expected_account_id)
        .bind(
            session
                .expected_account_revision
                .map(revision_to_i64)
                .transpose()?,
        )
        .bind(session.make_default)
        .bind(session.created_at)
        .bind(session.expires_at)
        .execute(&mut *transaction)
        .await;
        match insert {
            Ok(_) => {}
            Err(error) if is_unique_violation(&error) => {
                return Err(GoogleOAuthRepositoryError::DuplicateState);
            }
            Err(_) => return Err(GoogleOAuthRepositoryError::Internal),
        }
        insert_idempotency(
            &mut transaction,
            self.scope,
            &idempotency,
            true,
            Some(SESSION_RESOURCE),
            Some(session.id),
            None,
            session.created_at,
        )
        .await?;
        transaction.commit().await.map_err(internal)?;
        Ok(started)
    }

    async fn claim_callback(
        &self,
        state_hash: SecretHash,
        now: DateTime<Utc>,
        _exchange_stale_before: DateTime<Utc>,
    ) -> Result<CallbackClaim, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let row = sqlx::query(
            "SELECT id, owner_subject_hash, encrypted_pkce_verifier, verifier_key_version, \
             requested_scopes, expected_account_id, expected_account_revision, make_default, \
             status, expires_at, exchange_started_at FROM google_oauth_sessions \
             WHERE workspace_id = $1 AND user_id = $2 AND state_hash = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(state_hash.as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        let id: Uuid = row.try_get("id").map_err(internal)?;
        let status: String = row.try_get("status").map_err(internal)?;
        if status == "staged" {
            transaction.commit().await.map_err(internal)?;
            return Ok(CallbackClaim::Staged { session_id: id });
        }
        let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(internal)?;
        if status == "pending" && expires_at <= now {
            fail_session(&mut transaction, self.scope, id, now, true).await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        // Never reap an exchanging session automatically. A transport loss or
        // process crash can happen after Google minted a credential, so its
        // durable marker must survive for explicit recovery.
        if status != "pending" {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        let expected_id: Option<Uuid> = row.try_get("expected_account_id").map_err(internal)?;
        let existing_account = match expected_id {
            Some(account_id) => {
                fetch_optional_account_by_id(&mut transaction, self.scope, account_id, true).await?
            }
            None => None,
        };
        let expected_revision = row
            .try_get::<Option<i64>, _>("expected_account_revision")
            .map_err(internal)?
            .map(revision_from_i64)
            .transpose()?;
        if ensure_expected_account(existing_account.as_ref(), expected_id, expected_revision)
            .is_err()
        {
            fail_session(&mut transaction, self.scope, id, now, false).await?;
            transaction.commit().await.map_err(internal)?;
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        let changed = sqlx::query(
            "UPDATE google_oauth_sessions SET status = 'exchanging', exchange_started_at = $4 \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND status = 'pending'",
        )
        .bind(id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed != 1 {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        let claimed = ClaimedOAuthSession {
            id,
            owner_subject_hash: fixed_hash(row.try_get("owner_subject_hash").map_err(internal)?)?,
            encrypted_verifier: SealedSecret {
                key_version: version_from_i32(
                    row.try_get("verifier_key_version").map_err(internal)?,
                )?,
                ciphertext: row.try_get("encrypted_pkce_verifier").map_err(internal)?,
            },
            requested_scopes: row
                .try_get::<Vec<String>, _>("requested_scopes")
                .map_err(internal)?
                .into_iter()
                .collect(),
            make_default: row.try_get("make_default").map_err(internal)?,
            existing_account,
        };
        transaction.commit().await.map_err(internal)?;
        Ok(CallbackClaim::Exchange(Box::new(claimed)))
    }

    async fn stage_authorization(
        &self,
        completion: AuthorizationCompletion,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let session = sqlx::query(
            "SELECT status, owner_subject_hash, expected_account_id, expected_account_revision, \
             make_default \
             FROM google_oauth_sessions WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
             FOR UPDATE",
        )
        .bind(completion.session_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        let stored_expected_id: Option<Uuid> =
            session.try_get("expected_account_id").map_err(internal)?;
        let stored_expected_revision = session
            .try_get::<Option<i64>, _>("expected_account_revision")
            .map_err(internal)?
            .map(revision_from_i64)
            .transpose()?;
        if session.try_get::<String, _>("status").map_err(internal)? != "exchanging"
            || fixed_hash(session.try_get("owner_subject_hash").map_err(internal)?)?
                != completion.owner_subject_hash
            || stored_expected_revision != completion.expected_account_revision
            || stored_expected_id.is_some_and(|id| id != completion.account_id)
            || session
                .try_get::<bool, _>("make_default")
                .map_err(internal)?
                != completion.make_default
        {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        if let Some(expected_account_id) = stored_expected_id {
            let selected_identity: Option<String> = sqlx::query_scalar(
                "SELECT external_account_id FROM provider_accounts WHERE workspace_id = $1 \
                 AND user_id = $2 AND id = $3 AND provider = 'google' AND status <> 'revoked' \
                 AND tombstoned_at IS NULL",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(expected_account_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(internal)?
            .flatten();
            if selected_identity.as_deref() != Some(completion.external_account_id.as_str()) {
                return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
            }
        }
        let identity_in_use: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_accounts WHERE workspace_id = $1 \
             AND user_id = $2 AND provider = 'google' AND external_account_id = $3 \
             AND id <> $4 AND status <> 'revoked' AND tombstoned_at IS NULL)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(&completion.external_account_id)
        .bind(completion.account_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if identity_in_use {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        let changed = sqlx::query(
            "UPDATE google_oauth_sessions SET status = 'staged', \
             encrypted_pkce_verifier = NULL, verifier_key_version = NULL, staged_account_id = $4, \
             staged_external_account_id = $5, staged_display_label = $6, \
             staged_encrypted_credentials = $7, staged_credential_key_version = $8, \
             staged_granted_scopes = $9, staged_token_expires_at = $10, staged_at = $11 \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND status = 'exchanging'",
        )
        .bind(completion.session_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(completion.account_id)
        .bind(completion.external_account_id)
        .bind(completion.display_label)
        .bind(completion.credentials.sealed.ciphertext.as_slice())
        .bind(version_to_i32(completion.credentials.sealed.key_version)?)
        .bind(completion.granted_scopes.into_iter().collect::<Vec<_>>())
        .bind(completion.token_expires_at)
        .bind(completion.now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed != 1 {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        transaction.commit().await.map_err(internal)
    }

    async fn hold_cleanup_token(
        &self,
        session_id: Uuid,
        encrypted_refresh_token: SealedSecret,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_cleanup_hold_allowed(&mut transaction, self.scope, session_id).await?;
        let status: String = sqlx::query_scalar(
            "SELECT status FROM google_oauth_sessions WHERE workspace_id = $1 AND user_id = $2 \
             AND id = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        if status != "exchanging" {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        let existing = sqlx::query(
            "SELECT encrypted_refresh_token, key_version FROM google_oauth_cleanup_tokens \
             WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        if let Some(existing) = existing {
            let same = existing
                .try_get::<Vec<u8>, _>("encrypted_refresh_token")
                .map_err(internal)?
                == encrypted_refresh_token.ciphertext
                && existing
                    .try_get::<i32, _>("key_version")
                    .map_err(internal)?
                    == version_to_i32(encrypted_refresh_token.key_version)?;
            if !same {
                return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
            }
            transaction.commit().await.map_err(internal)?;
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO google_oauth_cleanup_tokens (session_id, workspace_id, user_id, \
             encrypted_refresh_token, key_version, created_at, updated_at, next_attempt_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $6, $6)",
        )
        .bind(session_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(encrypted_refresh_token.ciphertext.as_slice())
        .bind(version_to_i32(encrypted_refresh_token.key_version)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)
    }

    async fn identify_cleanup_token(
        &self,
        session_id: Uuid,
        external_account_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        if external_account_id.is_empty() || external_account_id.len() > 500 {
            return Err(GoogleOAuthRepositoryError::Internal);
        }
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_cleanup_hold_allowed(&mut transaction, self.scope, session_id).await?;
        let changed = sqlx::query(
            "UPDATE google_oauth_cleanup_tokens SET external_account_id = $4, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3 \
             AND status = 'held' AND (external_account_id IS NULL OR external_account_id = $4)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(external_account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed != 1 {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        transaction.commit().await.map_err(internal)
    }

    async fn resolve_authorization(
        &self,
        session_id: Uuid,
    ) -> Result<AuthorizationResolution, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let row = sqlx::query(
            "SELECT status, account_id FROM google_oauth_sessions WHERE workspace_id = $1 \
             AND user_id = $2 AND id = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        let status: String = row.try_get("status").map_err(internal)?;
        let resolution = match status.as_str() {
            "staged" => AuthorizationResolution::Staged,
            "consumed" => {
                let account_id: Uuid = row.try_get("account_id").map_err(internal)?;
                let account =
                    fetch_public_account_by_id(&mut transaction, self.scope, account_id, true)
                        .await?;
                AuthorizationResolution::Consumed(account)
            }
            "pending" | "exchanging" | "failed" => AuthorizationResolution::NeverStaged,
            _ => return Err(GoogleOAuthRepositoryError::Internal),
        };
        transaction.commit().await.map_err(internal)?;
        Ok(resolution)
    }

    async fn complete_staged_authorization(
        &self,
        session_id: Uuid,
    ) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let account = complete_staged(&mut transaction, self.scope, session_id).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(account)
    }

    async fn reconcile_staged(&self) -> Result<Option<GoogleAccount>, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let session_id = sqlx::query_scalar(
            "SELECT id FROM google_oauth_sessions WHERE workspace_id = $1 AND user_id = $2 \
             AND status = 'staged' ORDER BY created_at, id LIMIT 1 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let account = if let Some(session_id) = session_id {
            Some(complete_staged(&mut transaction, self.scope, session_id).await?)
        } else {
            None
        };
        transaction.commit().await.map_err(internal)?;
        Ok(account)
    }

    async fn abandon_authorization(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let status: String = sqlx::query_scalar(
            "SELECT status FROM google_oauth_sessions WHERE workspace_id = $1 AND user_id = $2 \
             AND id = $3 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        if matches!(status.as_str(), "exchanging" | "staged") {
            sqlx::query(
                "UPDATE google_oauth_sessions SET status = 'failed', failed_at = $4, \
                 encrypted_pkce_verifier = NULL, verifier_key_version = NULL, \
                 staged_account_id = NULL, staged_external_account_id = NULL, \
                 staged_display_label = NULL, staged_encrypted_credentials = NULL, \
                 staged_credential_key_version = NULL, staged_granted_scopes = NULL, \
                 staged_token_expires_at = NULL, staged_at = NULL \
                 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
                 AND status IN ('exchanging', 'staged')",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(session_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        if matches!(status.as_str(), "exchanging" | "staged" | "failed") {
            sqlx::query(
                "UPDATE google_oauth_cleanup_tokens SET status = 'pending', claim_id = NULL, \
                 claimed_at = NULL, updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 \
                 AND session_id = $3 AND status = 'held'",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(session_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        transaction.commit().await.map_err(internal)
    }

    #[allow(clippy::too_many_lines)]
    async fn claim_cleanup(
        &self,
        claim_id: Uuid,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        only_session_id: Option<Uuid>,
    ) -> Result<Option<CleanupClaim>, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        cleanup_stale_sessions(&mut transaction, self.scope, now, exchange_stale_before).await?;
        let legacy_recovery_required: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM google_oauth_legacy_credential_quarantine \
             WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NULL)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if legacy_recovery_required {
            transaction.commit().await.map_err(internal)?;
            return Ok(None);
        }
        let scope_state = sqlx::query(
            "SELECT credential_generation, revocation_kind, revocation_owner_id \
             FROM google_oauth_scope_state \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let credential_generation_i64: i64 = scope_state
            .try_get("credential_generation")
            .map_err(internal)?;
        let credential_generation = generation_from_i64(credential_generation_i64)?;
        let revocation_kind: Option<String> =
            scope_state.try_get("revocation_kind").map_err(internal)?;
        if revocation_kind
            .as_deref()
            .is_some_and(|kind| kind != "cleanup")
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(None);
        }
        let fenced_session: Option<Uuid> = scope_state
            .try_get("revocation_owner_id")
            .map_err(internal)?;
        let row = sqlx::query(
            "SELECT cleanup.session_id, cleanup.encrypted_refresh_token, cleanup.key_version, \
             cleanup.external_account_id, cleanup.attempt_count \
             FROM google_oauth_cleanup_tokens AS cleanup \
             WHERE cleanup.workspace_id = $1 AND cleanup.user_id = $2 \
             AND ($4::uuid IS NULL OR cleanup.session_id = $4) \
             AND ($5::uuid IS NULL OR cleanup.session_id = $5) \
             AND cleanup.attempt_count < $6 AND \
             ((cleanup.status = 'pending' AND cleanup.next_attempt_at <= $7) OR \
              (cleanup.status = 'revoking' AND cleanup.claimed_at <= $3)) \
             ORDER BY cleanup.created_at, cleanup.session_id LIMIT 1 FOR UPDATE OF cleanup",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(stale_before)
        .bind(fenced_session)
        .bind(only_session_id)
        .bind(
            i32::try_from(MAX_CLEANUP_ATTEMPTS)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
        )
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let Some(row) = row else {
            transaction.commit().await.map_err(internal)?;
            return Ok(None);
        };
        let session_id: Uuid = row.try_get("session_id").map_err(internal)?;
        let attempt_i32 = row
            .try_get::<i32, _>("attempt_count")
            .map_err(internal)?
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        let attempt =
            u32::try_from(attempt_i32).map_err(|_| GoogleOAuthRepositoryError::Internal)?;
        sqlx::query(
            "UPDATE google_oauth_cleanup_tokens SET status = 'revoking', claim_id = $4, \
             claimed_at = $5, attempt_count = $6, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(claim_id)
        .bind(now)
        .bind(attempt_i32)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let fence_changed = sqlx::query(
            "UPDATE google_oauth_scope_state SET revocation_kind = 'cleanup', \
             revocation_owner_id = $3, revocation_claim_id = $4, revocation_claimed_at = $5, \
             revocation_generation = credential_generation WHERE workspace_id = $1 \
             AND user_id = $2 AND (revocation_owner_id IS NULL \
             OR (revocation_kind = 'cleanup' AND revocation_owner_id = $3))",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(claim_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if fence_changed != 1 {
            return Err(GoogleOAuthRepositoryError::RevocationInProgress);
        }
        let protected_accounts = fetch_all_accounts(&mut transaction, self.scope).await?;
        let claim = CleanupClaim {
            session_id,
            claim_id,
            encrypted_refresh_token: SealedSecret {
                key_version: version_from_i32(row.try_get("key_version").map_err(internal)?)?,
                ciphertext: row.try_get("encrypted_refresh_token").map_err(internal)?,
            },
            external_account_id: row.try_get("external_account_id").map_err(internal)?,
            protected_accounts,
            credential_generation,
            attempt,
        };
        transaction.commit().await.map_err(internal)?;
        Ok(Some(claim))
    }

    async fn complete_cleanup(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        _now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "cleanup",
            session_id,
            claim_id,
            credential_generation,
        )
        .await?;
        let changed = sqlx::query(
            "DELETE FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 AND user_id = $2 \
             AND session_id = $3 AND status = 'revoking' AND claim_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(claim_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed == 1 {
            let cleared = sqlx::query(
                "UPDATE google_oauth_scope_state SET revocation_kind = NULL, \
                 revocation_owner_id = NULL, revocation_claim_id = NULL, revocation_claimed_at = NULL, \
                 revocation_generation = NULL WHERE workspace_id = $1 AND user_id = $2 \
                 AND revocation_kind = 'cleanup' AND revocation_owner_id = $3 \
                 AND revocation_claim_id = $4 \
                 AND revocation_generation = $5 AND credential_generation = $5",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(session_id)
            .bind(claim_id)
            .bind(generation_to_i64(credential_generation)?)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?
            .rows_affected();
            if cleared != 1 {
                return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
            }
            transaction.commit().await.map_err(internal)
        } else {
            Err(GoogleOAuthRepositoryError::CleanupClaimLost)
        }
    }

    async fn fail_cleanup(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "cleanup",
            session_id,
            claim_id,
            credential_generation,
        )
        .await?;
        let changed = sqlx::query(
            "UPDATE google_oauth_cleanup_tokens SET status = 'pending', claim_id = NULL, \
             claimed_at = NULL, last_failure_at = $5, updated_at = $5, next_attempt_at = $6 \
             WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3 \
             AND status = 'revoking' AND claim_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(claim_id)
        .bind(now)
        .bind(next_attempt_at)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed == 1 {
            transaction.commit().await.map_err(internal)
        } else {
            Err(GoogleOAuthRepositoryError::CleanupClaimLost)
        }
    }

    async fn defer_cleanup_for_operator(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "cleanup",
            session_id,
            claim_id,
            credential_generation,
        )
        .await?;
        let changed = sqlx::query(
            "UPDATE google_oauth_cleanup_tokens SET status = 'operator_required', \
             claim_id = NULL, claimed_at = NULL, last_failure_at = $5, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3 \
             AND status = 'revoking' AND claim_id = $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(claim_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed != 1 {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        let fenced = sqlx::query(
            "UPDATE google_oauth_scope_state SET revocation_kind = 'recovery', \
             revocation_claimed_at = $6 WHERE workspace_id = $1 AND user_id = $2 \
             AND revocation_kind = 'cleanup' AND revocation_owner_id = $3 \
             AND revocation_claim_id = $4 AND revocation_generation = $5 \
             AND credential_generation = $5",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(claim_id)
        .bind(generation_to_i64(credential_generation)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if fenced != 1 {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        transaction.commit().await.map_err(internal)
    }

    async fn cleanup_status(&self) -> Result<GoogleOAuthCleanupStatus, GoogleOAuthRepositoryError> {
        let row = sqlx::query(
            "SELECT count(*) FILTER (WHERE status = 'held') AS held, \
             count(*) FILTER (WHERE status = 'pending') AS pending, \
             count(*) FILTER (WHERE status = 'revoking') AS retrying, \
             count(*) FILTER (WHERE status = 'operator_required') AS operator_required, \
             count(*) FILTER (WHERE attempt_count >= $3) AS exhausted, \
             min(next_attempt_at) FILTER (WHERE status = 'pending' AND attempt_count < $3) \
                 AS next_attempt_at, max(last_failure_at) AS last_failure_at \
             , (SELECT revocation_owner_id IS NOT NULL FROM google_oauth_scope_state \
                WHERE workspace_id = $1 AND user_id = $2) AS revocation_fenced, \
             (SELECT COALESCE(revocation_kind = 'recovery', false) FROM google_oauth_scope_state \
                WHERE workspace_id = $1 AND user_id = $2) AS recovery_fenced, \
             (SELECT count(*) FROM google_oauth_sessions WHERE workspace_id = $1 \
                AND user_id = $2 AND status = 'exchanging') AS uncertain_authorizations, \
             (SELECT count(*) FROM google_oauth_legacy_credential_quarantine \
                WHERE workspace_id = $1 AND user_id = $2 \
                AND recovery_confirmed_at IS NULL) AS legacy_recovery_required \
             FROM google_oauth_cleanup_tokens \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(
            i32::try_from(MAX_CLEANUP_ATTEMPTS)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        let legacy_recovery_required = u64::try_from(
            row.try_get::<i64, _>("legacy_recovery_required")
                .map_err(internal)?,
        )
        .map_err(|_| GoogleOAuthRepositoryError::Internal)?;
        Ok(GoogleOAuthCleanupStatus {
            held: u64::try_from(row.try_get::<i64, _>("held").map_err(internal)?)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
            pending: u64::try_from(row.try_get::<i64, _>("pending").map_err(internal)?)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
            retrying: u64::try_from(row.try_get::<i64, _>("retrying").map_err(internal)?)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
            exhausted: u64::try_from(row.try_get::<i64, _>("exhausted").map_err(internal)?)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
            volatile_guardians: 0,
            durability_degraded: false,
            revocation_fenced: row
                .try_get::<bool, _>("revocation_fenced")
                .map_err(internal)?
                || legacy_recovery_required > 0,
            operator_recovery_required: row
                .try_get::<bool, _>("recovery_fenced")
                .map_err(internal)?
                || row
                    .try_get::<i64, _>("operator_required")
                    .map_err(internal)?
                    > 0
                || legacy_recovery_required > 0,
            uncertain_authorizations: u64::try_from(
                row.try_get::<i64, _>("uncertain_authorizations")
                    .map_err(internal)?,
            )
            .map_err(|_| GoogleOAuthRepositoryError::Internal)?,
            legacy_recovery_required,
            next_attempt_at: row.try_get("next_attempt_at").map_err(internal)?,
            last_failure_at: row.try_get("last_failure_at").map_err(internal)?,
        })
    }

    async fn claim_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<RevocationFenceClaim, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let row = sqlx::query(
            "SELECT credential_generation, revocation_kind, revocation_owner_id, \
             revocation_claim_id, revocation_claimed_at \
             FROM google_oauth_scope_state WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let existing_kind: Option<String> = row.try_get("revocation_kind").map_err(internal)?;
        let existing_owner: Option<Uuid> = row.try_get("revocation_owner_id").map_err(internal)?;
        let existing_claim: Option<Uuid> = row.try_get("revocation_claim_id").map_err(internal)?;
        let existing_claimed_at: Option<DateTime<Utc>> =
            row.try_get("revocation_claimed_at").map_err(internal)?;
        if existing_kind.is_some() {
            let exact_owner = existing_kind.as_deref() == Some("guardian")
                && existing_owner == Some(owner_id)
                && existing_claim == Some(claim_id);
            let stale_guardian = existing_kind.as_deref() == Some("guardian")
                && existing_claimed_at.is_some_and(|claimed_at| claimed_at <= stale_before);
            if !exact_owner && !stale_guardian {
                return Err(GoogleOAuthRepositoryError::RevocationInProgress);
            }
        }
        let session_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM google_oauth_sessions WHERE workspace_id = $1 \
             AND user_id = $2 AND id = $3)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(owner_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if !session_exists {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        let credential_generation =
            generation_from_i64(row.try_get("credential_generation").map_err(internal)?)?;
        sqlx::query(
            "UPDATE google_oauth_scope_state SET revocation_kind = 'guardian', \
             revocation_owner_id = $3, revocation_claim_id = $4, revocation_claimed_at = $5, \
             revocation_generation = credential_generation WHERE workspace_id = $1 \
             AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(owner_id)
        .bind(claim_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let protected_accounts = fetch_all_accounts(&mut transaction, self.scope).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(RevocationFenceClaim {
            owner_id,
            claim_id,
            protected_accounts,
            credential_generation,
        })
    }

    async fn complete_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        if guardian_resolution_exists(
            &mut transaction,
            self.scope,
            owner_id,
            claim_id,
            credential_generation,
            "revoked",
        )
        .await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(());
        }
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "guardian",
            owner_id,
            claim_id,
            credential_generation,
        )
        .await?;
        sqlx::query(
            "DELETE FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 AND user_id = $2 \
             AND session_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        fail_session(&mut transaction, self.scope, owner_id, Utc::now(), true).await?;
        record_guardian_resolution(
            &mut transaction,
            self.scope,
            owner_id,
            claim_id,
            credential_generation,
            "revoked",
        )
        .await?;
        clear_revocation_fence(
            &mut transaction,
            self.scope,
            "guardian",
            owner_id,
            claim_id,
            credential_generation,
        )
        .await?;
        transaction.commit().await.map_err(internal)
    }

    async fn release_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        if guardian_resolution_exists(
            &mut transaction,
            self.scope,
            owner_id,
            claim_id,
            credential_generation,
            "released",
        )
        .await?
        {
            transaction.commit().await.map_err(internal)?;
            return Ok(());
        }
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "guardian",
            owner_id,
            claim_id,
            credential_generation,
        )
        .await?;
        record_guardian_resolution(
            &mut transaction,
            self.scope,
            owner_id,
            claim_id,
            credential_generation,
            "released",
        )
        .await?;
        clear_revocation_fence(
            &mut transaction,
            self.scope,
            "guardian",
            owner_id,
            claim_id,
            credential_generation,
        )
        .await?;
        transaction.commit().await.map_err(internal)
    }

    #[allow(clippy::too_many_lines)]
    async fn recover_startup(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let fence = sqlx::query(
            "SELECT credential_generation, revocation_kind, revocation_owner_id, \
             revocation_claim_id, revocation_claimed_at FROM google_oauth_scope_state \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let kind: Option<String> = fence.try_get("revocation_kind").map_err(internal)?;
        let owner: Option<Uuid> = fence.try_get("revocation_owner_id").map_err(internal)?;
        let claim: Option<Uuid> = fence.try_get("revocation_claim_id").map_err(internal)?;
        let claimed_at: Option<DateTime<Utc>> =
            fence.try_get("revocation_claimed_at").map_err(internal)?;
        let generation = generation_from_i64(
            fence
                .try_get::<i64, _>("credential_generation")
                .map_err(internal)?,
        )?;
        if kind.as_deref() == Some("guardian")
            && claimed_at.is_some_and(|value| value <= stale_before)
        {
            let owner = owner.ok_or(GoogleOAuthRepositoryError::Internal)?;
            let claim = claim.ok_or(GoogleOAuthRepositoryError::Internal)?;
            let durable: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM google_oauth_cleanup_tokens \
                 WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3)",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(owner)
            .fetch_one(&mut *transaction)
            .await
            .map_err(internal)?;
            if durable {
                fail_session(&mut transaction, self.scope, owner, now, false).await?;
                record_guardian_resolution(
                    &mut transaction,
                    self.scope,
                    owner,
                    claim,
                    generation,
                    "released",
                )
                .await?;
                clear_revocation_fence(
                    &mut transaction,
                    self.scope,
                    "guardian",
                    owner,
                    claim,
                    generation,
                )
                .await?;
            } else {
                sqlx::query(
                    "UPDATE google_oauth_scope_state SET revocation_kind = 'recovery', \
                     revocation_claimed_at = $3 WHERE workspace_id = $1 AND user_id = $2 \
                     AND revocation_kind = 'guardian'",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(now)
                .execute(&mut *transaction)
                .await
                .map_err(internal)?;
            }
        }

        let current_kind: Option<String> = sqlx::query_scalar(
            "SELECT revocation_kind FROM google_oauth_scope_state \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if current_kind.is_none() {
            let stale_session: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM google_oauth_sessions WHERE workspace_id = $1 AND user_id = $2 \
                 AND status = 'exchanging' AND exchange_started_at <= $3 \
                 ORDER BY exchange_started_at, id LIMIT 1 FOR UPDATE",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(stale_before)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(internal)?;
            if let Some(session_id) = stale_session {
                let durable: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM google_oauth_cleanup_tokens \
                     WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3)",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(session_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(internal)?;
                if durable {
                    fail_session(&mut transaction, self.scope, session_id, now, false).await?;
                } else {
                    sqlx::query(
                        "UPDATE google_oauth_scope_state SET revocation_kind = 'recovery', \
                         revocation_owner_id = $3, revocation_claim_id = $4, \
                         revocation_claimed_at = $5, revocation_generation = credential_generation \
                         WHERE workspace_id = $1 AND user_id = $2 AND revocation_kind IS NULL",
                    )
                    .bind(self.scope.workspace_id)
                    .bind(self.scope.user_id)
                    .bind(session_id)
                    .bind(Uuid::new_v4())
                    .bind(now)
                    .execute(&mut *transaction)
                    .await
                    .map_err(internal)?;
                }
            }
        }
        transaction.commit().await.map_err(internal)
    }

    #[allow(clippy::too_many_lines)]
    async fn acknowledge_operator_recovery(
        &self,
        now: DateTime<Utc>,
    ) -> Result<OperatorRecoveryResult, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_execution_and_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let state = sqlx::query(
            "SELECT revocation_kind, revocation_owner_id FROM google_oauth_scope_state \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let recovery = state
            .try_get::<Option<String>, _>("revocation_kind")
            .map_err(internal)?
            .as_deref()
            == Some("recovery");
        let recovery_owner: Option<Uuid> =
            state.try_get("revocation_owner_id").map_err(internal)?;
        let legacy_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM google_oauth_legacy_credential_quarantine \
             WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if !recovery && legacy_count == 0 {
            return Err(GoogleOAuthRepositoryError::OperatorRecoveryNotRequired);
        }

        let teardown_account_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT account.id FROM provider_accounts account \
             WHERE account.workspace_id = $1 AND account.user_id = $2 \
               AND account.provider = 'google' AND ( \
                 ($3 AND account.status <> 'revoked' AND account.tombstoned_at IS NULL) \
                 OR EXISTS(SELECT 1 FROM google_oauth_legacy_credential_quarantine quarantine \
                   WHERE quarantine.workspace_id = account.workspace_id \
                     AND quarantine.user_id = account.user_id \
                     AND quarantine.source_account_id = account.id \
                     AND quarantine.recovery_confirmed_at IS NULL)) \
             ORDER BY account.id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(recovery)
        .fetch_all(&mut *transaction)
        .await
        .map_err(internal)?;
        for &account_id in &teardown_account_ids {
            retire_active_calendar_occurrences_for_account(
                &mut transaction,
                self.scope,
                account_id,
                now,
            )
            .await
            .map_err(internal)?;
            reconcile_schedule_publications_for_account_transition(
                &mut transaction,
                self.scope,
                account_id,
                SchedulePublicationAccountTransition::Retry,
                "operator_recovery_revoked_parent_claim",
                now,
            )
            .await?;
        }

        let accounts_affected = if recovery {
            sqlx::query(
                "UPDATE provider_accounts SET status = 'reauthorization_required', \
                 sync_enabled = false, revision = revision + 1, updated_at = $3 \
                 WHERE workspace_id = $1 AND user_id = $2 AND provider = 'google' \
                 AND status <> 'revoked' AND tombstoned_at IS NULL",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?
            .rows_affected()
        } else {
            0
        };
        if let Some(owner) = recovery_owner.filter(|_| recovery) {
            sqlx::query(
                "DELETE FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 \
                 AND user_id = $2 AND session_id = $3",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(owner)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
            fail_session(&mut transaction, self.scope, owner, now, true).await?;
        }
        let legacy_affected = sqlx::query(
            "UPDATE provider_accounts AS account SET provider = 'google', status = 'revoked', \
             sync_enabled = false, encrypted_credentials = NULL, credential_key_version = NULL, \
             granted_scopes = '{}', token_expires_at = NULL, is_default = false, \
             disconnected_at = COALESCE(disconnected_at, $3), updated_at = $3 \
             FROM google_oauth_legacy_credential_quarantine AS quarantine \
             WHERE quarantine.workspace_id = $1 AND quarantine.user_id = $2 \
             AND quarantine.recovery_confirmed_at IS NULL \
             AND account.id = quarantine.source_account_id \
             AND account.workspace_id = quarantine.workspace_id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        sqlx::query(
            "UPDATE google_oauth_legacy_credential_quarantine SET recovery_confirmed_at = $3, \
             encrypted_credentials = NULL, credential_key_version = NULL \
             WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        if !teardown_account_ids.is_empty() {
            // Account recovery is also a parent-run fence. Revoke the exact
            // identity after every generated-schedule child has been made
            // claimable again under a future, reauthorized run.
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
                                        ELSE 'operator_recovery_revoked_parent_claim' END, \
                 updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 \
                   AND provider_account_id = ANY($3) AND state = 'delivering'",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(&teardown_account_ids)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
            sqlx::query(
                "UPDATE google_sync_runs SET state = 'reauthorization_required', claim_id = NULL, \
                 lease_until = NULL, requested_at = NULL, next_attempt_at = $4, \
                 last_error_code = 'operator_recovery_required', last_error_at = $4, \
                 revision = revision + 1, updated_at = $4 \
                 WHERE workspace_id = $1 AND user_id = $2 \
                   AND provider_account_id = ANY($3)",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(&teardown_account_ids)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        sqlx::query(
            "UPDATE google_oauth_scope_state SET credential_generation = credential_generation + 1, \
             revocation_kind = NULL, revocation_owner_id = NULL, revocation_claim_id = NULL, \
             revocation_claimed_at = NULL, revocation_generation = NULL \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        Ok(OperatorRecoveryResult {
            accounts_marked_reauthorization_required: accounts_affected,
            legacy_accounts_finalized: legacy_affected,
        })
    }

    async fn fail_authorization(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        sqlx::query(
            "UPDATE google_oauth_sessions SET status = 'failed', failed_at = $4, \
             encrypted_pkce_verifier = NULL, verifier_key_version = NULL \
             WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND status = 'exchanging'",
        )
        .bind(session_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        transaction.commit().await.map_err(internal)
    }

    async fn account(&self) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let account = fetch_current_account(&mut transaction, self.scope, false).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(account)
    }

    async fn account_by_id(
        &self,
        account_id: Uuid,
    ) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let account =
            fetch_optional_account_by_id(&mut transaction, self.scope, account_id, false).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(account)
    }

    async fn accounts(&self) -> Result<Vec<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE workspace_id = $1 \
             AND user_id = $2 AND provider = 'google' AND status <> 'revoked' \
             AND tombstoned_at IS NULL ORDER BY is_default DESC, created_at, id"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.iter().map(account_from_row).collect()
    }

    async fn update_access_credentials(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        credentials: EncryptedCredentials,
        granted_scopes: BTreeSet<String>,
        token_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<AccountSecretSnapshot, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_no_open_authorization(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let current = fetch_account_by_id(&mut transaction, self.scope, account_id, true).await?;
        check_revision(&current.account, expected_revision)?;
        if current.account.status != GoogleAccountStatus::Active || !current.account.sync_enabled {
            return Err(GoogleOAuthRepositoryError::AccountStateConflict);
        }
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE provider_accounts SET encrypted_credentials = $4, credential_key_version = $5, \
             granted_scopes = $6, token_expires_at = $7, revision = $8, updated_at = $9 \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND provider = 'google' \
             AND status = 'active' AND sync_enabled AND revision = $10 \
             RETURNING {ACCOUNT_COLUMNS}"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(&credentials.sealed.ciphertext)
        .bind(version_to_i32(credentials.sealed.key_version)?)
        .bind(granted_scopes.into_iter().collect::<Vec<_>>())
        .bind(token_expires_at)
        .bind(revision_to_i64(next_revision)?)
        .bind(now)
        .bind(revision_to_i64(expected_revision)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::RevisionConflict {
            expected: expected_revision,
            actual: current.account.revision,
        })?;
        sqlx::query(
            "UPDATE google_oauth_scope_state SET credential_generation = credential_generation + 1 \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let snapshot = account_from_row(&row)?;
        transaction.commit().await.map_err(internal)?;
        Ok(snapshot)
    }

    #[allow(clippy::too_many_lines)] // Account state and every sync guardian change atomically.
    async fn set_paused(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        paused: bool,
        now: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_execution_and_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        if let Some(account) = account_idempotency_replay(
            lookup_idempotency(&mut transaction, self.scope, &idempotency, now).await?,
            ACCOUNT_RESOURCE,
        )? {
            transaction.commit().await.map_err(internal)?;
            return Ok(GoogleAccountMutation {
                account,
                replayed: true,
            });
        }
        cleanup_stale_sessions(&mut transaction, self.scope, now, exchange_stale_before).await?;
        ensure_no_open_authorization(&mut transaction, self.scope).await?;
        ensure_no_revocation_fence(&mut transaction, self.scope).await?;
        let current = fetch_account_by_id(&mut transaction, self.scope, account_id, true).await?;
        check_revision(&current.account, expected_revision)?;
        let expected_status = if paused { "active" } else { "paused" };
        if current.account.status.as_db() != expected_status {
            return Err(GoogleOAuthRepositoryError::AccountStateConflict);
        }
        if paused {
            retire_active_calendar_occurrences_for_account(
                &mut transaction,
                self.scope,
                account_id,
                now,
            )
            .await
            .map_err(internal)?;
        }
        let next_status = if paused { "paused" } else { "active" };
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE provider_accounts SET status = $4, sync_enabled = $5, revision = $6, \
             updated_at = $7 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
             RETURNING {ACCOUNT_COLUMNS}"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(next_status)
        .bind(!paused)
        .bind(revision_to_i64(next_revision)?)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if paused {
            // Account pause is a guardian fence for both inbound reconciliation
            // and provider publication. Revoke the run and every live delivery
            // claim in the same transaction as the credential-state change.
            reconcile_schedule_publications_for_account_transition(
                &mut transaction,
                self.scope,
                account_id,
                SchedulePublicationAccountTransition::Retry,
                "account_paused_with_active_schedule_delivery",
                now,
            )
            .await?;
            sqlx::query(
                "UPDATE google_sync_runs SET state = 'idle', claim_id = NULL, lease_until = NULL, \
                 requested_at = NULL, next_attempt_at = $4, last_error_code = 'account_paused', \
                 last_error_at = $4, revision = revision + 1, updated_at = $4 \
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
                "UPDATE google_sync_outbox SET state = 'backoff', claim_id = NULL, \
                 claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
                 dispatch_nonce = NULL, dispatch_authorized_at = NULL, \
                 dispatch_expires_at = NULL, attempts = attempts + 1, available_at = $4, \
                 last_error_code = 'account_paused', updated_at = $4 \
                 WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
                   AND state = 'delivering'",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(account_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        } else {
            sqlx::query(
                "UPDATE google_sync_runs SET requested_at = $4, next_attempt_at = $4, \
                 last_error_code = CASE WHEN last_error_code = 'account_paused' \
                                        THEN NULL ELSE last_error_code END, \
                 last_error_at = CASE WHEN last_error_code = 'account_paused' \
                                      THEN NULL ELSE last_error_at END, \
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
                "UPDATE google_sync_outbox SET available_at = $4, last_error_code = NULL, \
                 updated_at = $4 WHERE workspace_id = $1 AND user_id = $2 \
                 AND provider_account_id = $3 AND state = 'backoff' \
                 AND last_error_code = 'account_paused'",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(account_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        }
        let account = account_from_row(&row)?.account;
        let response =
            serde_json::to_value(&account).map_err(|_| GoogleOAuthRepositoryError::Internal)?;
        insert_idempotency(
            &mut transaction,
            self.scope,
            &idempotency,
            true,
            Some(ACCOUNT_RESOURCE),
            Some(account.id),
            Some(&response),
            now,
        )
        .await?;
        transaction.commit().await.map_err(internal)?;
        Ok(GoogleAccountMutation {
            account,
            replayed: false,
        })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn claim_disconnect(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        claim_id: Uuid,
        now: DateTime<Utc>,
        disconnect_stale_before: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<DisconnectMutation, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_execution_and_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let idempotency_state =
            lookup_idempotency(&mut transaction, self.scope, &idempotency, now).await?;
        if matches!(idempotency_state, IdempotencyState::Completed { .. }) {
            let account =
                account_idempotency_replay(idempotency_state.clone(), DISCONNECT_RESOURCE)?
                    .ok_or(GoogleOAuthRepositoryError::Internal)?;
            transaction.commit().await.map_err(internal)?;
            return Ok(DisconnectMutation::Replay(account));
        }
        let idempotency_retry = match &idempotency_state {
            IdempotencyState::Absent => false,
            IdempotencyState::InProgress {
                resource_type,
                resource_id: Some(id),
            } if resource_type.as_deref() == Some(DISCONNECT_RESOURCE) && *id == account_id => true,
            IdempotencyState::InProgress { .. } | IdempotencyState::Completed { .. } => {
                return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
            }
        };
        cleanup_stale_sessions(&mut transaction, self.scope, now, exchange_stale_before).await?;
        ensure_no_open_authorization(&mut transaction, self.scope).await?;
        let scope_state = sqlx::query(
            "SELECT credential_generation, revocation_kind, revocation_owner_id \
             FROM google_oauth_scope_state WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        let existing_kind: Option<String> =
            scope_state.try_get("revocation_kind").map_err(internal)?;
        let existing_owner: Option<Uuid> = scope_state
            .try_get("revocation_owner_id")
            .map_err(internal)?;
        let credential_generation = generation_from_i64(
            scope_state
                .try_get("credential_generation")
                .map_err(internal)?,
        )?;
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {ACCOUNT_COLUMNS}, disconnect_claim_id, disconnect_claimed_at, \
             disconnect_operation_hash FROM provider_accounts WHERE workspace_id = $1 \
             AND user_id = $2 AND id = $3 AND provider = 'google' AND status <> 'revoked' \
             FOR UPDATE"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::AccountNotFound)?;
        let current = account_from_row(&row)?;
        let current_operation = row
            .try_get::<Option<Vec<u8>>, _>("disconnect_operation_hash")
            .map_err(internal)?
            .map(fixed_hash)
            .transpose()?;
        let claimed_at: Option<DateTime<Utc>> =
            row.try_get("disconnect_claimed_at").map_err(internal)?;
        let matching_disconnect_fence =
            existing_kind.as_deref() == Some("disconnect") && existing_owner == Some(account_id);
        // The durable disconnect operation hash and scope fence intentionally
        // outlive the ordinary idempotency row after provider revocation fails.
        // Recreate only that exact key/account operation; a different absent key
        // remains blocked by the fence below.
        let recovered_expired_retry = matches!(idempotency_state, IdempotencyState::Absent)
            && matching_disconnect_fence
            && current_operation == Some(idempotency.key_hash);
        let retry = idempotency_retry || recovered_expired_retry;
        if existing_kind.is_some() && (!retry || !matching_disconnect_fence) {
            return Err(GoogleOAuthRepositoryError::RevocationInProgress);
        }
        if retry {
            if current_operation != Some(idempotency.key_hash) {
                return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
            }
            if current.account.status == GoogleAccountStatus::Disconnecting
                && claimed_at.is_some_and(|value| value > disconnect_stale_before)
            {
                return Err(GoogleOAuthRepositoryError::IdempotencyInProgress);
            }
            if !matches!(
                current.account.status,
                GoogleAccountStatus::Disconnecting | GoogleAccountStatus::RevocationFailed
            ) {
                return Err(GoogleOAuthRepositoryError::AccountStateConflict);
            }
        } else {
            check_revision(&current.account, expected_revision)?;
            if current.account.status == GoogleAccountStatus::Disconnecting
                && claimed_at.is_some_and(|value| value > disconnect_stale_before)
            {
                return Err(GoogleOAuthRepositoryError::DisconnectInProgress);
            }
        }
        retire_active_calendar_occurrences_for_account(
            &mut transaction,
            self.scope,
            account_id,
            now,
        )
        .await
        .map_err(internal)?;
        let next_revision = current
            .account
            .revision
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        let changed = sqlx::query(
            "UPDATE provider_accounts SET status = 'disconnecting', sync_enabled = false, \
             disconnect_claim_id = $4, disconnect_claimed_at = $5, disconnect_operation_hash = $6, \
             revocation_error_at = NULL, revision = $7, updated_at = $5 \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(claim_id)
        .bind(now)
        .bind(idempotency.key_hash.as_slice())
        .bind(revision_to_i64(next_revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed != 1 {
            return Err(GoogleOAuthRepositoryError::Internal);
        }
        // Disconnect is the account-level guardian fence for Calendar/Tasks.
        // Revoke every local delivery claim in the same transaction that makes
        // the credentials unavailable, so a stale worker cannot acknowledge or
        // later replay provider mutations after the owner disconnects.
        reconcile_schedule_publications_for_account_transition(
            &mut transaction,
            self.scope,
            account_id,
            SchedulePublicationAccountTransition::Conflict,
            "account_disconnecting",
            now,
        )
        .await?;
        sqlx::query(
            "UPDATE google_sync_outbox SET state = 'conflict', claim_id = NULL, \
             claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
             dispatch_nonce = NULL, dispatch_authorized_at = NULL, \
             dispatch_expires_at = NULL, last_error_code = 'account_disconnecting', updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3 \
             AND state IN ('pending', 'delivering', 'backoff')",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "UPDATE google_sync_runs SET state = 'idle', claim_id = NULL, lease_until = NULL, \
             next_attempt_at = $4, requested_at = NULL, last_error_code = 'account_disconnecting', \
             last_error_at = $4, revision = revision + 1, updated_at = $4 \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        if !idempotency_retry {
            insert_idempotency(
                &mut transaction,
                self.scope,
                &idempotency,
                false,
                Some(DISCONNECT_RESOURCE),
                Some(account_id),
                None,
                now,
            )
            .await?;
        }
        sqlx::query(
            "UPDATE google_oauth_scope_state SET revocation_kind = 'disconnect', \
             revocation_owner_id = $3, revocation_claim_id = $4, revocation_claimed_at = $5, \
             revocation_generation = credential_generation WHERE workspace_id = $1 \
             AND user_id = $2",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(claim_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let protected_accounts = fetch_all_accounts(&mut transaction, self.scope).await?;
        transaction.commit().await.map_err(internal)?;
        let mut account = current.account;
        account.status = GoogleAccountStatus::Disconnecting;
        account.sync_enabled = false;
        account.revision = next_revision;
        account.updated_at = now;
        Ok(DisconnectMutation::Execute(DisconnectClaim {
            claim_id,
            account,
            credentials: current.credentials,
            protected_accounts,
            credential_generation,
        }))
    }

    async fn complete_disconnect(
        &self,
        account_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        let state = lookup_idempotency(&mut transaction, self.scope, &idempotency, now).await?;
        if matches!(state, IdempotencyState::Completed { .. }) {
            let account = account_idempotency_replay(state.clone(), DISCONNECT_RESOURCE)?
                .ok_or(GoogleOAuthRepositoryError::Internal)?;
            transaction.commit().await.map_err(internal)?;
            return Ok(GoogleAccountMutation {
                account,
                replayed: true,
            });
        }
        match state {
            IdempotencyState::InProgress {
                resource_type,
                resource_id: Some(id),
            } if resource_type.as_deref() == Some(DISCONNECT_RESOURCE) && id == account_id => {}
            _ => return Err(GoogleOAuthRepositoryError::IdempotencyConflict),
        }
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "disconnect",
            account_id,
            claim_id,
            credential_generation,
        )
        .await?;
        let was_default: bool = sqlx::query_scalar(
            "SELECT is_default FROM provider_accounts WHERE workspace_id = $1 AND user_id = $2 \
             AND id = $3 AND provider = 'google' FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::AccountNotFound)?;
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE provider_accounts SET status = 'revoked', sync_enabled = false, \
             is_default = false, \
             encrypted_credentials = NULL, credential_key_version = NULL, granted_scopes = '{{}}', \
             token_expires_at = NULL, disconnect_claim_id = NULL, disconnect_claimed_at = NULL, \
             disconnect_operation_hash = NULL, disconnected_at = $6, revision = revision + 1, \
             updated_at = $6 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
             AND disconnect_claim_id = $4 AND disconnect_operation_hash = $5 \
             AND status = 'disconnecting' RETURNING {ACCOUNT_COLUMNS}"
        )))
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(claim_id)
        .bind(idempotency.key_hash.as_slice())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(GoogleOAuthRepositoryError::DisconnectInProgress)?;
        let account = account_from_row_allow_revoked(&row)?;
        if was_default {
            promote_postgres_default(&mut transaction, self.scope).await?;
        }
        let response =
            serde_json::to_value(&account).map_err(|_| GoogleOAuthRepositoryError::Internal)?;
        complete_idempotency(
            &mut transaction,
            self.scope,
            &idempotency,
            DISCONNECT_RESOURCE,
            account_id,
            &response,
        )
        .await?;
        clear_revocation_fence(
            &mut transaction,
            self.scope,
            "disconnect",
            account_id,
            claim_id,
            credential_generation,
        )
        .await?;
        transaction.commit().await.map_err(internal)?;
        Ok(GoogleAccountMutation {
            account,
            replayed: false,
        })
    }

    async fn fail_disconnect(
        &self,
        account_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_scope(&mut transaction, self.scope).await?;
        ensure_revocation_fence(
            &mut transaction,
            self.scope,
            "disconnect",
            account_id,
            claim_id,
            credential_generation,
        )
        .await?;
        let changed = sqlx::query(
            "UPDATE provider_accounts SET status = 'revocation_failed', sync_enabled = false, \
             disconnect_claim_id = NULL, disconnect_claimed_at = NULL, revocation_error_at = $5, \
             revision = revision + 1, updated_at = $5 WHERE workspace_id = $1 AND user_id = $2 \
             AND id = $3 AND disconnect_claim_id = $4 AND status = 'disconnecting'",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(account_id)
        .bind(claim_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if changed != 1 {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        transaction.commit().await.map_err(internal)
    }
}

async fn reconcile_schedule_publications_for_account_transition(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    account_id: Uuid,
    transition: SchedulePublicationAccountTransition,
    code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), GoogleOAuthRepositoryError> {
    // Every schedule-publication stage first takes a shared lock on this
    // parent row. Taking it exclusively before touching children serializes
    // an OAuth/account fence with authorize/complete, including a response
    // already in flight at the provider.
    sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM google_sync_runs WHERE workspace_id = $1 AND user_id = $2 \
         AND provider_account_id = $3 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;

    let terminal = matches!(transition, SchedulePublicationAccountTransition::Conflict);
    let publication_ids = sqlx::query_scalar::<_, Uuid>(
        "WITH reconciled AS (UPDATE google_schedule_publication_outbox outbox SET \
         state = CASE WHEN $4 THEN 'conflict' ELSE 'backoff' END, \
         claim_id = NULL, claimed_at = NULL, run_claim_id = NULL, \
         run_claim_generation = NULL, dispatch_nonce = NULL, dispatch_provider_write = NULL, \
         dispatch_authorized_at = NULL, dispatch_expires_at = NULL, \
         available_at = CASE WHEN $4 THEN outbox.available_at ELSE $6 END, \
         terminal_at = CASE WHEN $4 THEN $6 ELSE NULL END, \
         last_error_code = CASE \
           WHEN NOT $4 AND outbox.provider_post_may_have_started \
             AND outbox.last_error_code IN (\
               'provider_identity_unresolved', 'provider_response_too_large'\
             ) THEN outbox.last_error_code \
           ELSE $5 END, updated_at = $6 \
         FROM google_schedule_publication_batches batch \
         WHERE outbox.workspace_id = $1 AND outbox.user_id = $2 \
           AND batch.workspace_id = outbox.workspace_id AND batch.user_id = outbox.user_id \
           AND batch.id = outbox.publication_id AND batch.provider_account_id = $3 \
           AND (outbox.state = 'delivering' \
                OR ($4 AND outbox.state IN ('pending', 'backoff'))) \
         RETURNING outbox.publication_id) SELECT DISTINCT publication_id FROM reconciled",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(terminal)
    .bind(code)
    .bind(now)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    for publication_id in publication_ids {
        refresh_schedule_publication_batch(transaction, scope, publication_id, now).await?;
    }
    Ok(())
}

async fn refresh_schedule_publication_batch(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    publication_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), GoogleOAuthRepositoryError> {
    let counts = sqlx::query(
        "SELECT count(*)::integer AS total_count, \
           count(*) FILTER (WHERE state = 'pending')::integer AS pending_count, \
           count(*) FILTER (WHERE state = 'delivering')::integer AS delivering_count, \
           count(*) FILTER (WHERE state = 'backoff')::integer AS backoff_count, \
           count(*) FILTER (WHERE state = 'published')::integer AS published_count, \
           count(*) FILTER (WHERE state = 'conflict')::integer AS conflict_count, \
           count(*) FILTER (WHERE state = 'failed')::integer AS failed_count, \
           count(*) FILTER (WHERE state = 'superseded')::integer AS superseded_count, \
           (array_agg(last_error_code ORDER BY updated_at DESC, id DESC) \
             FILTER (WHERE last_error_code IS NOT NULL))[1] AS last_error_code \
         FROM google_schedule_publication_outbox \
         WHERE workspace_id = $1 AND user_id = $2 AND publication_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(publication_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    let total: i32 = counts.try_get("total_count").map_err(internal)?;
    let pending: i32 = counts.try_get("pending_count").map_err(internal)?;
    let delivering: i32 = counts.try_get("delivering_count").map_err(internal)?;
    let backoff: i32 = counts.try_get("backoff_count").map_err(internal)?;
    let published: i32 = counts.try_get("published_count").map_err(internal)?;
    let conflict: i32 = counts.try_get("conflict_count").map_err(internal)?;
    let failed: i32 = counts.try_get("failed_count").map_err(internal)?;
    let superseded: i32 = counts.try_get("superseded_count").map_err(internal)?;
    let state = if delivering > 0 {
        "delivering"
    } else if pending > 0 {
        "pending"
    } else if backoff > 0 {
        "backoff"
    } else if published == total {
        "published"
    } else if published > 0 {
        "partially_published"
    } else if conflict > 0 {
        "conflict"
    } else if failed > 0 {
        "failed"
    } else {
        "superseded"
    };
    let active = pending + delivering + backoff > 0;
    let last_error_code: Option<String> = counts.try_get("last_error_code").map_err(internal)?;
    sqlx::query(
        "UPDATE google_schedule_publication_batches SET state = $4, pending_count = $5, \
         delivering_count = $6, backoff_count = $7, published_count = $8, \
         conflict_count = $9, failed_count = $10, superseded_count = $11, \
         last_error_code = $12, \
         last_error_at = CASE WHEN $12::varchar IS NULL THEN NULL ELSE $13 END, \
         started_at = CASE WHEN $6 > 0 THEN COALESCE(started_at, $13) ELSE started_at END, \
         completed_at = CASE WHEN $14 THEN NULL ELSE COALESCE(completed_at, $13) END, \
         updated_at = $13 WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(publication_id)
    .bind(state)
    .bind(pending)
    .bind(delivering)
    .bind(backoff)
    .bind(published)
    .bind(conflict)
    .bind(failed)
    .bind(superseded)
    .bind(last_error_code)
    .bind(now)
    .bind(active)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn complete_staged(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    session_id: Uuid,
) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
    ensure_no_revocation_fence(transaction, scope).await?;
    let row = sqlx::query(
        "SELECT status, account_id, expected_account_revision, make_default, staged_account_id, \
         staged_external_account_id, staged_display_label, staged_encrypted_credentials, \
         staged_credential_key_version, staged_granted_scopes, staged_token_expires_at, staged_at \
         FROM google_oauth_sessions WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
         FOR UPDATE",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
    let status: String = row.try_get("status").map_err(internal)?;
    if status == "consumed" {
        let account_id: Uuid = row.try_get("account_id").map_err(internal)?;
        return fetch_public_account_by_id(transaction, scope, account_id, true).await;
    }
    if status != "staged" {
        return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
    }
    let expected_revision = row
        .try_get::<Option<i64>, _>("expected_account_revision")
        .map_err(internal)?
        .map(revision_from_i64)
        .transpose()?;
    let account_id: Uuid = row.try_get("staged_account_id").map_err(internal)?;
    let now: DateTime<Utc> = row.try_get("staged_at").map_err(internal)?;
    let make_default: bool = row.try_get("make_default").map_err(internal)?;
    let account = if let Some(expected) = expected_revision {
        let current = fetch_optional_account_by_id(transaction, scope, account_id, true)
            .await?
            .ok_or(GoogleOAuthRepositoryError::AuthorizationConflict)?;
        if current.account.revision != expected {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        if make_default {
            clear_google_default(transaction, scope, Some(account_id)).await?;
        }
        let next_revision = expected
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        let account_row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE provider_accounts SET status = 'active', sync_enabled = true, \
                 encrypted_credentials = $4, credential_key_version = $5, granted_scopes = $6, \
                 token_expires_at = $7, revision = $8, updated_at = $9, external_account_id = $10, \
                 display_label = $11, disconnect_claim_id = NULL, disconnect_claimed_at = NULL, \
                 disconnect_operation_hash = NULL, revocation_error_at = NULL, \
                 is_default = CASE WHEN $12 THEN true ELSE is_default END \
                 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 RETURNING {ACCOUNT_COLUMNS}"
        )))
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(account_id)
        .bind(
            row.try_get::<Vec<u8>, _>("staged_encrypted_credentials")
                .map_err(internal)?,
        )
        .bind(
            row.try_get::<i32, _>("staged_credential_key_version")
                .map_err(internal)?,
        )
        .bind(
            row.try_get::<Vec<String>, _>("staged_granted_scopes")
                .map_err(internal)?,
        )
        .bind(
            row.try_get::<DateTime<Utc>, _>("staged_token_expires_at")
                .map_err(internal)?,
        )
        .bind(revision_to_i64(next_revision)?)
        .bind(now)
        .bind(
            row.try_get::<String, _>("staged_external_account_id")
                .map_err(internal)?,
        )
        .bind(
            row.try_get::<String, _>("staged_display_label")
                .map_err(internal)?,
        )
        .bind(make_default)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| map_authorization_write_error(&error))?;
        account_from_row(&account_row)?.account
    } else {
        let has_default: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM provider_accounts WHERE workspace_id = $1 \
                 AND user_id = $2 AND provider = 'google' AND is_default \
                 AND status <> 'revoked' AND tombstoned_at IS NULL)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)?;
        let is_default = make_default || !has_default;
        if make_default {
            clear_google_default(transaction, scope, None).await?;
        }
        let account_row = sqlx::query(AssertSqlSafe(format!(
                "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
                 external_account_id, display_label, encrypted_credentials, credential_key_version, \
                 granted_scopes, status, sync_enabled, is_default, token_expires_at, created_at, updated_at) \
                 VALUES ($1, $2, $3, 'google', $9, $10, $4, $5, $6, 'active', true, $11, $7, $8, $8) \
                 RETURNING {ACCOUNT_COLUMNS}"
            )))
            .bind(account_id)
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .bind(
                row.try_get::<Vec<u8>, _>("staged_encrypted_credentials")
                    .map_err(internal)?,
            )
            .bind(
                row.try_get::<i32, _>("staged_credential_key_version")
                    .map_err(internal)?,
            )
            .bind(
                row.try_get::<Vec<String>, _>("staged_granted_scopes")
                    .map_err(internal)?,
            )
            .bind(
                row.try_get::<DateTime<Utc>, _>("staged_token_expires_at")
                    .map_err(internal)?,
            )
            .bind(now)
            .bind(
                row.try_get::<String, _>("staged_external_account_id")
                    .map_err(internal)?,
            )
            .bind(
                row.try_get::<String, _>("staged_display_label")
                    .map_err(internal)?,
            )
            .bind(is_default)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| map_authorization_write_error(&error))?;
        account_from_row(&account_row)?.account
    };
    let generation_changed = sqlx::query(
        "UPDATE google_oauth_scope_state SET credential_generation = credential_generation + 1 \
         WHERE workspace_id = $1 AND user_id = $2 AND revocation_owner_id IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if generation_changed != 1 {
        return Err(GoogleOAuthRepositoryError::RevocationInProgress);
    }
    sqlx::query(
        "DELETE FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 AND user_id = $2 \
         AND session_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(session_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    let changed = sqlx::query(
        "UPDATE google_oauth_sessions SET status = 'consumed', account_id = $4, consumed_at = $5, \
         staged_account_id = NULL, staged_external_account_id = NULL, staged_display_label = NULL, \
         staged_encrypted_credentials = NULL, staged_credential_key_version = NULL, \
         staged_granted_scopes = NULL, staged_token_expires_at = NULL, staged_at = NULL \
         WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND status = 'staged'",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account.id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if changed != 1 {
        return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
    }
    Ok(account)
}

async fn lock_scope(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), GoogleOAuthRepositoryError> {
    sqlx::query(
        "SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleOAuthRepositoryError::Internal)?;
    sqlx::query(
        "INSERT INTO google_oauth_scope_state (workspace_id, user_id) VALUES ($1, $2) \
         ON CONFLICT (workspace_id, user_id) DO NOTHING",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "SELECT 1 FROM google_oauth_scope_state WHERE workspace_id = $1 AND user_id = $2 \
         FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn ensure_no_revocation_fence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), GoogleOAuthRepositoryError> {
    let fenced: bool = sqlx::query_scalar(
        "SELECT revocation_owner_id IS NOT NULL OR EXISTS( \
             SELECT 1 FROM google_oauth_legacy_credential_quarantine \
             WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NULL \
         ) FROM google_oauth_scope_state WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if fenced {
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    } else {
        Ok(())
    }
}

async fn ensure_cleanup_hold_allowed(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    session_id: Uuid,
) -> Result<(), GoogleOAuthRepositoryError> {
    let allowed: bool = sqlx::query_scalar(
        "SELECT revocation_owner_id IS NULL OR (revocation_kind = 'guardian' \
         AND revocation_owner_id = $3) FROM google_oauth_scope_state \
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if allowed {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    }
}

#[allow(clippy::too_many_arguments)]
async fn guardian_resolution_exists(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    owner_id: Uuid,
    claim_id: Uuid,
    credential_generation: u64,
    outcome: &str,
) -> Result<bool, GoogleOAuthRepositoryError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM google_oauth_guardian_resolutions \
         WHERE workspace_id = $1 AND user_id = $2 AND session_id = $3 AND claim_id = $4 \
         AND credential_generation = $5 AND outcome = $6)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(owner_id)
    .bind(claim_id)
    .bind(generation_to_i64(credential_generation)?)
    .bind(outcome)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)
}

#[allow(clippy::too_many_arguments)]
async fn record_guardian_resolution(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    owner_id: Uuid,
    claim_id: Uuid,
    credential_generation: u64,
    outcome: &str,
) -> Result<(), GoogleOAuthRepositoryError> {
    sqlx::query(
        "INSERT INTO google_oauth_guardian_resolutions (workspace_id, user_id, session_id, \
         claim_id, credential_generation, outcome, resolved_at) \
         VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp())",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(owner_id)
    .bind(claim_id)
    .bind(generation_to_i64(credential_generation)?)
    .bind(outcome)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn ensure_revocation_fence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    kind: &str,
    session_id: Uuid,
    claim_id: Uuid,
    credential_generation: u64,
) -> Result<(), GoogleOAuthRepositoryError> {
    let matches: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM google_oauth_scope_state WHERE workspace_id = $1 \
         AND user_id = $2 AND revocation_kind = $6 AND revocation_owner_id = $3 \
         AND revocation_claim_id = $4 \
         AND revocation_generation = $5 AND credential_generation = $5)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(session_id)
    .bind(claim_id)
    .bind(generation_to_i64(credential_generation)?)
    .bind(kind)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if matches {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    }
}

#[allow(clippy::too_many_arguments)]
async fn clear_revocation_fence(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    kind: &str,
    owner_id: Uuid,
    claim_id: Uuid,
    credential_generation: u64,
) -> Result<(), GoogleOAuthRepositoryError> {
    let changed = sqlx::query(
        "UPDATE google_oauth_scope_state SET revocation_kind = NULL, \
         revocation_owner_id = NULL, revocation_claim_id = NULL, revocation_claimed_at = NULL, \
         revocation_generation = NULL WHERE workspace_id = $1 AND user_id = $2 \
         AND revocation_kind = $6 AND revocation_owner_id = $3 AND revocation_claim_id = $4 \
         AND revocation_generation = $5 AND credential_generation = $5",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(owner_id)
    .bind(claim_id)
    .bind(generation_to_i64(credential_generation)?)
    .bind(kind)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if changed == 1 {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    }
}

async fn clear_google_default(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    except_account_id: Option<Uuid>,
) -> Result<(), GoogleOAuthRepositoryError> {
    sqlx::query(
        "UPDATE provider_accounts SET is_default = false, revision = revision + 1, \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND user_id = $2 \
         AND provider = 'google' AND is_default AND status <> 'revoked' \
         AND tombstoned_at IS NULL AND ($3::uuid IS NULL OR id <> $3)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(except_account_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn promote_postgres_default(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), GoogleOAuthRepositoryError> {
    sqlx::query(
        "WITH candidate AS (SELECT id FROM provider_accounts WHERE workspace_id = $1 \
             AND user_id = $2 AND provider = 'google' AND status <> 'revoked' \
             AND tombstoned_at IS NULL ORDER BY created_at, id LIMIT 1 FOR UPDATE) \
         UPDATE provider_accounts AS account SET is_default = true, revision = revision + 1, \
             updated_at = clock_timestamp() FROM candidate WHERE account.id = candidate.id \
             AND NOT account.is_default",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn cleanup_stale_sessions(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
    _exchange_stale_before: DateTime<Utc>,
) -> Result<(), GoogleOAuthRepositoryError> {
    sqlx::query(
        "UPDATE google_oauth_sessions SET encrypted_authorization_url = NULL, \
         authorization_url_key_version = NULL WHERE workspace_id = $1 AND user_id = $2 \
         AND expires_at <= $3 AND status NOT IN ('pending', 'exchanging', 'staged')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "WITH failed AS ( \
             UPDATE google_oauth_sessions SET status = 'failed', failed_at = $3, \
             encrypted_pkce_verifier = NULL, verifier_key_version = NULL, \
             encrypted_authorization_url = CASE WHEN expires_at <= $3 THEN NULL \
                 ELSE encrypted_authorization_url END, \
             authorization_url_key_version = CASE WHEN expires_at <= $3 THEN NULL \
                 ELSE authorization_url_key_version END \
             WHERE workspace_id = $1 AND user_id = $2 \
             AND status = 'pending' AND expires_at <= $3 \
             RETURNING id \
         ) UPDATE google_oauth_cleanup_tokens SET status = 'pending', claim_id = NULL, \
             claimed_at = NULL, updated_at = $3 WHERE workspace_id = $1 AND user_id = $2 \
             AND status = 'held' AND session_id IN (SELECT id FROM failed)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn fail_session(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    session_id: Uuid,
    now: DateTime<Utc>,
    scrub_authorization_url: bool,
) -> Result<(), GoogleOAuthRepositoryError> {
    sqlx::query(
        "UPDATE google_oauth_sessions SET status = 'failed', failed_at = $4, \
         encrypted_pkce_verifier = NULL, verifier_key_version = NULL, \
         encrypted_authorization_url = CASE WHEN $5 THEN NULL ELSE encrypted_authorization_url END, \
         authorization_url_key_version = CASE WHEN $5 THEN NULL ELSE authorization_url_key_version END \
         WHERE id = $1 AND workspace_id = $2 AND user_id = $3 \
         AND status IN ('pending', 'exchanging')",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .bind(scrub_authorization_url)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    sqlx::query(
        "UPDATE google_oauth_cleanup_tokens SET status = 'pending', claim_id = NULL, \
         claimed_at = NULL, updated_at = $4 WHERE session_id = $1 AND workspace_id = $2 \
         AND user_id = $3 AND status = 'held' AND EXISTS (SELECT 1 FROM google_oauth_sessions \
         WHERE id = $1 AND workspace_id = $2 AND user_id = $3 AND status = 'failed')",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn ensure_no_open_exchange(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), GoogleOAuthRepositoryError> {
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM google_oauth_sessions WHERE workspace_id = $1 \
         AND user_id = $2 AND status IN ('exchanging', 'staged'))",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if open {
        Err(GoogleOAuthRepositoryError::AuthorizationInProgress)
    } else {
        Ok(())
    }
}

async fn ensure_no_open_authorization(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), GoogleOAuthRepositoryError> {
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM google_oauth_sessions WHERE workspace_id = $1 \
         AND user_id = $2 AND status IN ('pending', 'exchanging', 'staged'))",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if open {
        Err(GoogleOAuthRepositoryError::AuthorizationInProgress)
    } else {
        Ok(())
    }
}

fn ensure_expected_account(
    current: Option<&AccountSecretSnapshot>,
    expected_id: Option<Uuid>,
    expected_revision: Option<u64>,
) -> Result<(), GoogleOAuthRepositoryError> {
    let matches = match (expected_id, expected_revision, current) {
        (None, None, _) => true,
        (Some(id), Some(revision), Some(current)) => {
            current.account.id == id && current.account.revision == revision
        }
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::AuthorizationConflict)
    }
}

#[derive(Clone, Debug)]
enum IdempotencyState {
    Absent,
    InProgress {
        resource_type: Option<String>,
        resource_id: Option<Uuid>,
    },
    Completed {
        resource_type: Option<String>,
        resource_id: Option<Uuid>,
        response: Option<Value>,
    },
}

async fn lookup_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &OAuthIdempotency,
    now: DateTime<Utc>,
) -> Result<IdempotencyState, GoogleOAuthRepositoryError> {
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 \
         AND key_hash = $3 AND expires_at <= $4",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    let row = sqlx::query(
        "SELECT request_fingerprint, state, resource_type, resource_id, response_json \
         FROM idempotency_keys WHERE workspace_id = $1 AND namespace = $2 AND key_hash = $3 \
         FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(IdempotencyState::Absent);
    };
    if fixed_hash(row.try_get("request_fingerprint").map_err(internal)?)?
        != idempotency.request_fingerprint
    {
        return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
    }
    let resource_type = row.try_get("resource_type").map_err(internal)?;
    let resource_id = row.try_get("resource_id").map_err(internal)?;
    if row.try_get::<String, _>("state").map_err(internal)? == "completed" {
        Ok(IdempotencyState::Completed {
            resource_type,
            resource_id,
            response: row.try_get("response_json").map_err(internal)?,
        })
    } else {
        Ok(IdempotencyState::InProgress {
            resource_type,
            resource_id,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &OAuthIdempotency,
    completed: bool,
    resource_type: Option<&str>,
    resource_id: Option<Uuid>,
    response: Option<&Value>,
    now: DateTime<Utc>,
) -> Result<(), GoogleOAuthRepositoryError> {
    let state = if completed {
        "completed"
    } else {
        "in_progress"
    };
    sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         state, resource_type, resource_id, response_json, created_at, updated_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10)",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .bind(idempotency.request_fingerprint.as_slice())
    .bind(state)
    .bind(resource_type)
    .bind(resource_id)
    .bind(response)
    .bind(now)
    .bind(idempotency.expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            GoogleOAuthRepositoryError::IdempotencyConflict
        } else {
            GoogleOAuthRepositoryError::Internal
        }
    })?;
    Ok(())
}

async fn complete_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    idempotency: &OAuthIdempotency,
    resource_type: &str,
    resource_id: Uuid,
    response: &Value,
) -> Result<(), GoogleOAuthRepositoryError> {
    let changed = sqlx::query(
        "UPDATE idempotency_keys SET state = 'completed', response_json = $6, \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND namespace = $2 \
         AND key_hash = $3 AND request_fingerprint = $4 AND resource_type = $5 \
         AND resource_id = $7 AND state = 'in_progress'",
    )
    .bind(scope.workspace_id)
    .bind(idempotency.namespace)
    .bind(idempotency.key_hash.as_slice())
    .bind(idempotency.request_fingerprint.as_slice())
    .bind(resource_type)
    .bind(response)
    .bind(resource_id)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if changed == 1 {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::IdempotencyConflict)
    }
}

fn account_idempotency_replay(
    state: IdempotencyState,
    expected_resource: &str,
) -> Result<Option<GoogleAccount>, GoogleOAuthRepositoryError> {
    match state {
        IdempotencyState::Absent => Ok(None),
        IdempotencyState::InProgress { .. } => {
            Err(GoogleOAuthRepositoryError::IdempotencyInProgress)
        }
        IdempotencyState::Completed {
            resource_type,
            response: Some(response),
            ..
        } if resource_type.as_deref() == Some(expected_resource) => {
            serde_json::from_value(response)
                .map(Some)
                .map_err(|_| GoogleOAuthRepositoryError::Internal)
        }
        IdempotencyState::Completed { .. } => Err(GoogleOAuthRepositoryError::IdempotencyConflict),
    }
}

async fn fetch_current_account(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    lock: bool,
) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    sqlx::query(AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE workspace_id = $1 AND user_id = $2 \
         AND provider = 'google' AND is_default AND status <> 'revoked' \
         AND tombstoned_at IS NULL{suffix}"
    )))
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .as_ref()
    .map(account_from_row)
    .transpose()
}

async fn fetch_account_by_id(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    id: Uuid,
    lock: bool,
) -> Result<AccountSecretSnapshot, GoogleOAuthRepositoryError> {
    fetch_optional_account_by_id(transaction, scope, id, lock)
        .await?
        .ok_or(GoogleOAuthRepositoryError::AccountNotFound)
}

async fn fetch_optional_account_by_id(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    id: Uuid,
    lock: bool,
) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    sqlx::query(AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE workspace_id = $1 AND user_id = $2 \
         AND id = $3 AND provider = 'google' AND status <> 'revoked' \
         AND tombstoned_at IS NULL{suffix}"
    )))
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .as_ref()
    .map(account_from_row)
    .transpose()
}

async fn fetch_all_accounts(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<Vec<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE workspace_id = $1 \
         AND user_id = $2 AND provider = 'google' AND status <> 'revoked' \
         AND tombstoned_at IS NULL ORDER BY is_default DESC, created_at, id FOR UPDATE"
    )))
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    rows.iter().map(account_from_row).collect()
}

async fn fetch_public_account_by_id(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    id: Uuid,
    lock: bool,
) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE workspace_id = $1 AND user_id = $2 \
         AND id = $3 AND provider = 'google'{suffix}"
    )))
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(GoogleOAuthRepositoryError::Internal)?;
    public_account_from_row(&row)
}

fn account_from_row(row: &PgRow) -> Result<AccountSecretSnapshot, GoogleOAuthRepositoryError> {
    let account = public_account_from_row(row)?;
    let ciphertext: Option<Vec<u8>> = row.try_get("encrypted_credentials").map_err(internal)?;
    let version: Option<i32> = row.try_get("credential_key_version").map_err(internal)?;
    Ok(AccountSecretSnapshot {
        account,
        credentials: EncryptedCredentials {
            sealed: SealedSecret {
                key_version: version_from_i32(
                    version.ok_or(GoogleOAuthRepositoryError::Internal)?,
                )?,
                ciphertext: ciphertext.ok_or(GoogleOAuthRepositoryError::Internal)?,
            },
        },
    })
}

fn account_from_row_allow_revoked(
    row: &PgRow,
) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
    public_account_from_row(row)
}

fn public_account_from_row(row: &PgRow) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
    let status = parse_status(&row.try_get::<String, _>("status").map_err(internal)?)?;
    let revision: i64 = row.try_get("revision").map_err(internal)?;
    Ok(GoogleAccount {
        id: row.try_get("id").map_err(internal)?,
        external_account_id: row
            .try_get::<Option<String>, _>("external_account_id")
            .map_err(internal)?
            .ok_or(GoogleOAuthRepositoryError::Internal)?,
        display_label: row.try_get("display_label").map_err(internal)?,
        status,
        sync_enabled: row.try_get("sync_enabled").map_err(internal)?,
        is_default: row.try_get("is_default").map_err(internal)?,
        granted_scopes: row
            .try_get::<Vec<String>, _>("granted_scopes")
            .map_err(internal)?
            .into_iter()
            .collect::<BTreeSet<_>>(),
        token_expires_at: row.try_get("token_expires_at").map_err(internal)?,
        revision: revision_from_i64(revision)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
    })
}

fn parse_status(value: &str) -> Result<GoogleAccountStatus, GoogleOAuthRepositoryError> {
    match value {
        "active" => Ok(GoogleAccountStatus::Active),
        "paused" => Ok(GoogleAccountStatus::Paused),
        "reauthorization_required" => Ok(GoogleAccountStatus::ReauthorizationRequired),
        "disconnecting" => Ok(GoogleAccountStatus::Disconnecting),
        "revocation_failed" => Ok(GoogleAccountStatus::RevocationFailed),
        "revoked" => Ok(GoogleAccountStatus::Revoked),
        _ => Err(GoogleOAuthRepositoryError::Internal),
    }
}

fn fixed_hash(bytes: Vec<u8>) -> Result<SecretHash, GoogleOAuthRepositoryError> {
    bytes
        .try_into()
        .map_err(|_| GoogleOAuthRepositoryError::Internal)
}

fn version_to_i32(version: u32) -> Result<i32, GoogleOAuthRepositoryError> {
    i32::try_from(version).map_err(|_| GoogleOAuthRepositoryError::Internal)
}

fn version_from_i32(version: i32) -> Result<u32, GoogleOAuthRepositoryError> {
    u32::try_from(version)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(GoogleOAuthRepositoryError::Internal)
}

fn revision_to_i64(revision: u64) -> Result<i64, GoogleOAuthRepositoryError> {
    i64::try_from(revision).map_err(|_| GoogleOAuthRepositoryError::Internal)
}

fn revision_from_i64(revision: i64) -> Result<u64, GoogleOAuthRepositoryError> {
    u64::try_from(revision).map_err(|_| GoogleOAuthRepositoryError::Internal)
}

fn generation_to_i64(generation: u64) -> Result<i64, GoogleOAuthRepositoryError> {
    i64::try_from(generation).map_err(|_| GoogleOAuthRepositoryError::Internal)
}

fn generation_from_i64(generation: i64) -> Result<u64, GoogleOAuthRepositoryError> {
    u64::try_from(generation).map_err(|_| GoogleOAuthRepositoryError::Internal)
}

fn check_revision(
    account: &GoogleAccount,
    expected: u64,
) -> Result<(), GoogleOAuthRepositoryError> {
    if account.revision == expected {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::RevisionConflict {
            expected,
            actual: account.revision,
        })
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

fn map_authorization_write_error(error: &sqlx::Error) -> GoogleOAuthRepositoryError {
    if is_unique_violation(error) {
        GoogleOAuthRepositoryError::AuthorizationConflict
    } else {
        GoogleOAuthRepositoryError::Internal
    }
}

fn internal<T>(_: T) -> GoogleOAuthRepositoryError {
    GoogleOAuthRepositoryError::Internal
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
        persistence::MIGRATOR,
    };

    use super::*;

    struct ScheduleOAuthFixture {
        database: TestDatabase,
        repository: PostgresGoogleOAuthRepository,
        scope: DatabaseScope,
        account_id: Uuid,
        publication_id: Uuid,
        outbox_id: Uuid,
        parent_claim_id: Uuid,
        parent_claim_generation: i64,
        send_started_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    }

    #[tokio::test]
    async fn postgres_disconnect_reconciles_schedule_delivery_before_send() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; disconnect-before-send test skipped");
            return;
        };
        assert_disconnect_reconciles_schedule_delivery(&database_url, false).await;
    }

    #[tokio::test]
    async fn postgres_disconnect_preserves_possible_schedule_send_evidence() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; disconnect-after-send test skipped");
            return;
        };
        assert_disconnect_reconciles_schedule_delivery(&database_url, true).await;
    }

    #[tokio::test]
    async fn postgres_operator_recovery_revokes_parent_and_requeues_uncertain_schedule_delivery() {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; operator-recovery test skipped");
            return;
        };
        let fixture = schedule_oauth_fixture(&database_url, true).await;
        let mut transaction = fixture
            .database
            .pool
            .begin()
            .await
            .expect("sticky response fixture transaction");
        sqlx::query(
            "UPDATE google_schedule_publication_outbox SET \
             last_error_code = 'provider_identity_unresolved' \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.outbox_id)
        .execute(&mut *transaction)
        .await
        .expect("sticky ambiguous response fixture");
        refresh_schedule_publication_batch(
            &mut transaction,
            fixture.scope,
            fixture.publication_id,
            fixture.now,
        )
        .await
        .expect("refresh sticky response batch fixture");
        transaction
            .commit()
            .await
            .expect("commit sticky response fixture");
        let recovery_owner = Uuid::new_v4();
        let recovery_claim = Uuid::new_v4();
        sqlx::query(
            "UPDATE google_oauth_scope_state SET revocation_kind = 'recovery', \
             revocation_owner_id = $3, revocation_claim_id = $4, revocation_claimed_at = $5, \
             revocation_generation = credential_generation \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(recovery_owner)
        .bind(recovery_claim)
        .bind(fixture.now - Duration::minutes(1))
        .execute(&fixture.database.pool)
        .await
        .expect("operator recovery fence fixture");

        let result = fixture
            .repository
            .acknowledge_operator_recovery(fixture.now)
            .await
            .expect("operator recovery acknowledgement");
        assert_eq!(result.accounts_marked_reauthorization_required, 1);
        assert_eq!(result.legacy_accounts_finalized, 0);

        let account: (String, bool) = sqlx::query_as(
            "SELECT status, sync_enabled FROM provider_accounts \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.account_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("recovered account state");
        assert_eq!(account, ("reauthorization_required".to_owned(), false));
        assert_parent_run_revoked(&fixture, "reauthorization_required").await;
        assert_schedule_reconciled(&fixture, "backoff", "provider_identity_unresolved", true).await;
        assert_stale_schedule_completion_cannot_mutate(&fixture).await;
        fixture.database.destroy().await;
    }

    async fn assert_disconnect_reconciles_schedule_delivery(
        database_url: &str,
        possible_send: bool,
    ) {
        let fixture = schedule_oauth_fixture(database_url, possible_send).await;
        let mutation = fixture
            .repository
            .claim_disconnect(
                fixture.account_id,
                1,
                Uuid::new_v4(),
                fixture.now,
                fixture.now - Duration::minutes(5),
                fixture.now - Duration::minutes(5),
                OAuthIdempotency {
                    namespace: "google_oauth_schedule_disconnect_test",
                    key_hash: [91; 32],
                    request_fingerprint: [92; 32],
                    expires_at: fixture.now + Duration::hours(1),
                },
            )
            .await
            .expect("disconnect claim");
        assert!(matches!(mutation, DisconnectMutation::Execute(_)));

        assert_parent_run_revoked(&fixture, "idle").await;
        assert_schedule_reconciled(&fixture, "conflict", "account_disconnecting", possible_send)
            .await;
        assert_stale_schedule_completion_cannot_mutate(&fixture).await;
        fixture.database.destroy().await;
    }

    async fn assert_parent_run_revoked(fixture: &ScheduleOAuthFixture, expected_state: &str) {
        let run: (String, Option<Uuid>, Option<DateTime<Utc>>, i64) = sqlx::query_as(
            "SELECT state, claim_id, lease_until, claim_generation FROM google_sync_runs \
             WHERE workspace_id = $1 AND user_id = $2 AND provider_account_id = $3",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(fixture.account_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("parent run after account transition");
        assert_eq!(run.0, expected_state);
        assert_eq!(run.1, None);
        assert_eq!(run.2, None);
        assert_eq!(run.3, fixture.parent_claim_generation);
    }

    async fn assert_schedule_reconciled(
        fixture: &ScheduleOAuthFixture,
        expected_state: &str,
        expected_code: &str,
        possible_send: bool,
    ) {
        let outbox = sqlx::query(
            "SELECT state, claim_id, claimed_at, run_claim_id, run_claim_generation, \
             dispatch_nonce, dispatch_provider_write, dispatch_authorized_at, dispatch_expires_at, \
             provider_post_may_have_started, send_started_at, no_effect_observation_count, \
             last_no_effect_observed_at, last_error_code, terminal_at \
             FROM google_schedule_publication_outbox WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.outbox_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("schedule outbox after account transition");
        assert_eq!(outbox.get::<String, _>("state"), expected_state);
        assert_eq!(outbox.get::<Option<Uuid>, _>("claim_id"), None);
        assert_eq!(outbox.get::<Option<DateTime<Utc>>, _>("claimed_at"), None);
        assert_eq!(outbox.get::<Option<Uuid>, _>("run_claim_id"), None);
        assert_eq!(outbox.get::<Option<i64>, _>("run_claim_generation"), None);
        assert_eq!(outbox.get::<Option<Uuid>, _>("dispatch_nonce"), None);
        assert_eq!(
            outbox.get::<Option<bool>, _>("dispatch_provider_write"),
            None
        );
        assert_eq!(
            outbox.get::<Option<DateTime<Utc>>, _>("dispatch_authorized_at"),
            None
        );
        assert_eq!(
            outbox.get::<Option<DateTime<Utc>>, _>("dispatch_expires_at"),
            None
        );
        assert_eq!(
            outbox.get::<bool, _>("provider_post_may_have_started"),
            possible_send
        );
        assert_eq!(
            outbox.get::<Option<DateTime<Utc>>, _>("send_started_at"),
            fixture.send_started_at
        );
        assert_eq!(outbox.get::<i32, _>("no_effect_observation_count"), 0);
        assert_eq!(
            outbox.get::<Option<DateTime<Utc>>, _>("last_no_effect_observed_at"),
            None
        );
        assert_eq!(outbox.get::<String, _>("last_error_code"), expected_code);
        let terminal_at = outbox.get::<Option<DateTime<Utc>>, _>("terminal_at");
        assert_eq!(
            terminal_at,
            (expected_state == "conflict").then_some(fixture.now)
        );

        let batch: (String, i32, i32, i32, i32, Option<DateTime<Utc>>, String) = sqlx::query_as(
            "SELECT state, pending_count, delivering_count, backoff_count, conflict_count, \
                 completed_at, last_error_code FROM google_schedule_publication_batches \
                 WHERE workspace_id = $1 AND id = $2",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.publication_id)
        .fetch_one(&fixture.database.pool)
        .await
        .expect("schedule batch after account transition");
        assert_eq!(batch.0, expected_state);
        assert_eq!((batch.1, batch.2), (0, 0));
        assert_eq!(batch.3, i32::from(expected_state == "backoff"));
        assert_eq!(batch.4, i32::from(expected_state == "conflict"));
        assert_eq!(
            batch.5,
            (expected_state == "conflict").then_some(fixture.now)
        );
        assert_eq!(batch.6, expected_code);
    }

    async fn assert_stale_schedule_completion_cannot_mutate(fixture: &ScheduleOAuthFixture) {
        let changed = sqlx::query(
            "UPDATE google_schedule_publication_outbox SET state = 'published', claim_id = NULL, \
             claimed_at = NULL, run_claim_id = NULL, run_claim_generation = NULL, \
             dispatch_nonce = NULL, dispatch_provider_write = NULL, dispatch_authorized_at = NULL, \
             dispatch_expires_at = NULL, terminal_at = $7, updated_at = $7 \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND state = 'delivering' \
               AND run_claim_id = $4 AND run_claim_generation = $5 AND claim_id = $6",
        )
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(fixture.outbox_id)
        .bind(fixture.parent_claim_id)
        .bind(fixture.parent_claim_generation)
        .bind(Uuid::from_u128(61))
        .bind(fixture.now + Duration::seconds(1))
        .execute(&fixture.database.pool)
        .await
        .expect("stale schedule completion fence")
        .rows_affected();
        assert_eq!(changed, 0);
    }

    #[allow(clippy::too_many_lines)]
    async fn schedule_oauth_fixture(
        database_url: &str,
        possible_send: bool,
    ) -> ScheduleOAuthFixture {
        let database = TestDatabase::create(database_url).await;
        MIGRATOR
            .run(&database.pool)
            .await
            .expect("migrations apply");
        let scope = seed_scope(&database.pool).await;
        let now: DateTime<Utc> = "2026-09-03T12:00:00Z".parse().expect("fixture time");
        let created_at = now - Duration::minutes(2);
        let claimed_at = now - Duration::seconds(30);
        let authorized_at = now - Duration::seconds(10);
        let account_id = Uuid::new_v4();
        let collection_id = Uuid::new_v4();
        let schedule_revision_id = Uuid::new_v4();
        let source_block_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let publication_id = Uuid::new_v4();
        let change_id = Uuid::new_v4();
        let outbox_id = Uuid::new_v4();
        let parent_claim_id = Uuid::new_v4();
        let parent_claim_generation = 7_i64;
        let child_claim_id = Uuid::from_u128(61);
        let starts_at = now + Duration::hours(1);
        let ends_at = starts_at + Duration::hours(1);

        let mut transaction = database.pool.begin().await.expect("begin fixture");
        sqlx::query("INSERT INTO google_oauth_scope_state (workspace_id, user_id) VALUES ($1, $2)")
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .execute(&mut *transaction)
            .await
            .expect("OAuth scope fixture");
        sqlx::query(
            "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
             external_account_id, display_label, encrypted_credentials, credential_key_version, \
             granted_scopes, status, sync_enabled, is_default, created_at, updated_at) \
             VALUES ($1, $2, $3, 'google', $4, 'Schedule OAuth test account', $5, 1, \
             ARRAY['https://www.googleapis.com/auth/calendar'], 'active', true, true, $6, $6)",
        )
        .bind(account_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(format!("schedule-oauth-{account_id}"))
        .bind(vec![7_u8; 64])
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("Google account fixture");
        sqlx::query(
            "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
             collection_kind, remote_collection_id, display_name, provider_access_role, \
             provider_primary, provider_selected, selected, sync_role, revision, discovered_at, \
             configured_at, created_at, updated_at) VALUES ($1, $2, $3, $4, 'calendar', \
             'primary@example.test', 'Primary', 'owner', true, true, true, 'writable', 1, \
             $5, $5, $5, $5)",
        )
        .bind(collection_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(account_id)
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("Google collection fixture");
        sqlx::query(
            "INSERT INTO google_sync_runs (workspace_id, user_id, provider_account_id, state, \
             claim_id, lease_until, started_at, next_attempt_at, claim_generation, created_at, \
             updated_at) VALUES ($1, $2, $3, 'running', $4, $5, $6, $6, $7, $6, $6)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(account_id)
        .bind(parent_claim_id)
        .bind(now + Duration::minutes(5))
        .bind(created_at)
        .bind(parent_claim_generation)
        .execute(&mut *transaction)
        .await
        .expect("running parent fixture");
        sqlx::query(
            "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
             timezone_name, duration_seconds, earliest_start_at, deadline_at, created_at, updated_at) \
             VALUES ($1, $2, $3, 'task', 'scheduled', 'Schedule publication fixture', 'UTC', \
             3600, $4, $5, $6, $6)",
        )
        .bind(item_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(starts_at)
        .bind(ends_at)
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("item fixture");
        sqlx::query(
            "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, \
             horizon_start, horizon_end, timezone_name, input_digest, publication_hash, \
             created_by_user_id, created_at) VALUES ($1, $2, 1, 'draft', \
             $3, $4, 'UTC', $5, $6, $7, $8)",
        )
        .bind(schedule_revision_id)
        .bind(scope.workspace_id)
        .bind(now)
        .bind(now + Duration::days(1))
        .bind(vec![10_u8; 32])
        .bind(vec![11_u8; 32])
        .bind(scope.user_id)
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("schedule revision fixture");
        sqlx::query(
            "INSERT INTO schedule_blocks (id, workspace_id, schedule_revision_id, source_block_id, \
             item_id, block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal, \
             constraint_snapshot, created_at) VALUES ($1, $2, $3, $1, $4, 'planned', \
             'Schedule publication fixture', $5, $6, 'UTC', 0, $7, $8)",
        )
        .bind(source_block_id)
        .bind(scope.workspace_id)
        .bind(schedule_revision_id)
        .bind(item_id)
        .bind(starts_at)
        .bind(ends_at)
        .bind(json!({
            "source_block_id": source_block_id,
            "occurrence_id": null,
            "session_index": 0,
            "core_kind": "planned"
        }))
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("schedule block fixture");
        sqlx::query(
            "INSERT INTO google_schedule_publication_previews (id, workspace_id, user_id, \
             provider_account_id, collection_id, collection_revision, collection_remote_id, \
             collection_display_name, required_scope, schedule_revision_id, schedule_revision_number, \
             schedule_publication_hash, desired_set_hash, timezone_name, horizon_start, horizon_end, \
             preview_hash, change_count, create_count, update_count, delete_count, noop_count, \
             expires_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 1, \
             'primary@example.test', 'Primary', 'https://www.googleapis.com/auth/calendar', \
             $6, 1, $7, $8, 'UTC', $9, $10, $11, 1, 1, 0, 0, 0, $12, $13, $13)",
        )
        .bind(publication_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(schedule_revision_id)
        .bind(vec![11_u8; 32])
        .bind(vec![12_u8; 32])
        .bind(now)
        .bind(now + Duration::days(1))
        .bind(vec![13_u8; 32])
        .bind(created_at + Duration::minutes(29))
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("schedule preview fixture");
        sqlx::query(
            "INSERT INTO google_schedule_publication_preview_changes (id, workspace_id, user_id, \
             preview_id, provider_account_id, collection_id, schedule_revision_id, ordinal, slot_id, \
             source_block_id, item_id, session_index, incarnation, operation, desired_payload_hash, \
             intent_hash, provider_payload, review_summary, starts_at, ends_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $9, $10, 0, 1, 'create', \
             $11, $12, $13, $14, $15, $16, $17)",
        )
        .bind(change_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(publication_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(schedule_revision_id)
        .bind(Uuid::new_v4())
        .bind(source_block_id)
        .bind(item_id)
        .bind(vec![14_u8; 32])
        .bind(vec![15_u8; 32])
        .bind(json!({"id": "schedule-event-id"}))
        .bind(json!({"summary": "Schedule publication fixture"}))
        .bind(starts_at)
        .bind(ends_at)
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("schedule preview change fixture");
        sqlx::query(
            "INSERT INTO google_schedule_publication_batches (id, workspace_id, user_id, \
             provider_account_id, collection_id, collection_revision, collection_remote_id, \
             required_scope, schedule_revision_id, schedule_revision_number, schedule_publication_hash, \
             state, total_count, create_count, update_count, delete_count, noop_count, pending_count, \
             delivering_count, backoff_count, published_count, conflict_count, failed_count, \
             superseded_count, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 1, \
             'primary@example.test', 'https://www.googleapis.com/auth/calendar', $6, 1, $7, \
             'pending', 1, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, $8, $8)",
        )
        .bind(publication_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(account_id)
        .bind(collection_id)
        .bind(schedule_revision_id)
        .bind(vec![11_u8; 32])
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("schedule publication batch fixture");
        sqlx::query(
            "INSERT INTO google_schedule_publication_outbox (id, workspace_id, user_id, \
             publication_id, change_id, ordinal, operation, state, available_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, 0, 'create', 'pending', $6, $6, $6)",
        )
        .bind(outbox_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(publication_id)
        .bind(change_id)
        .bind(created_at)
        .execute(&mut *transaction)
        .await
        .expect("schedule publication outbox fixture");
        sqlx::query(
            "UPDATE google_schedule_publication_outbox SET state = 'delivering', claim_id = $3, \
             claimed_at = $4, run_claim_id = $5, run_claim_generation = $6, updated_at = $4 \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(outbox_id)
        .bind(child_claim_id)
        .bind(claimed_at)
        .bind(parent_claim_id)
        .bind(parent_claim_generation)
        .execute(&mut *transaction)
        .await
        .expect("claimed schedule publication fixture");
        let send_started_at = possible_send.then_some(authorized_at);
        if possible_send {
            sqlx::query(
                "UPDATE google_schedule_publication_outbox SET dispatch_nonce = $3, \
                 dispatch_provider_write = true, dispatch_authorized_at = $4, \
                 dispatch_expires_at = $5, provider_post_may_have_started = true, \
                 send_started_at = $4, updated_at = $4 WHERE workspace_id = $1 AND id = $2",
            )
            .bind(scope.workspace_id)
            .bind(outbox_id)
            .bind(Uuid::new_v4())
            .bind(authorized_at)
            .bind(authorized_at + Duration::seconds(30))
            .execute(&mut *transaction)
            .await
            .expect("authorized schedule publication fixture");
        }
        refresh_schedule_publication_batch(&mut transaction, scope, publication_id, authorized_at)
            .await
            .expect("refresh claimed schedule batch fixture");
        transaction.commit().await.expect("commit schedule fixture");

        let repository = PostgresGoogleOAuthRepository::new(database.pool.clone(), scope);
        ScheduleOAuthFixture {
            database,
            repository,
            scope,
            account_id,
            publication_id,
            outbox_id,
            parent_claim_id,
            parent_claim_generation,
            send_started_at,
            now,
        }
    }

    async fn seed_scope(pool: &PgPool) -> DatabaseScope {
        let scope = DatabaseScope {
            user_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
        };
        sqlx::query(
            "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
             VALUES ($1, $2, 'Schedule OAuth owner', 'UTC')",
        )
        .bind(scope.user_id)
        .bind(format!("schedule-oauth-owner-{}", scope.user_id))
        .execute(pool)
        .await
        .expect("user fixture");
        sqlx::query(
            "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
             VALUES ($1, $2, $3, 'Schedule OAuth workspace', 'UTC')",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(format!("oauth-{}", scope.workspace_id.simple()))
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
            let schema = format!("dw_oauth_sched_{}", Uuid::new_v4().simple());
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
