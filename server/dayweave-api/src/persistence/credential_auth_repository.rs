use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use url::Url;
use uuid::Uuid;

use crate::{
    auth::Scope,
    credential_auth::{
        ACCESS_TOKEN_TTL, AccountRecoveryCode, AccountRecoveryCodeSpec, AccountRecoveryConsumption,
        AccountRecoverySessionSpec, CredentialKind, CredentialMutation, CredentialRepository,
        CredentialRepositoryError, DEVICE_CLIENT_CONTRACT_VERSION, DEVICE_SESSION_ABSOLUTE_TTL,
        DEVICE_SESSION_REFRESH_IDLE_TTL, DeviceClientKind, DeviceEnrollmentCreation,
        DeviceEnrollmentSpec, DeviceSession, ENROLLMENT_TOKEN_TTL, MAX_ACTIVE_DEVICE_SESSIONS,
        MAX_MCP_CREDENTIAL_TTL, MAX_PENDING_DEVICE_ENROLLMENTS, MCP_CLIENT_CONTRACT_VERSION,
        MCP_CREDENTIAL_DEFAULT_TTL, McpClient, McpClientSpec, OpaqueCredential,
        full_owner_device_scopes,
    },
};

use super::DatabaseScope;

#[derive(Clone)]
pub struct PostgresCredentialRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresCredentialRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }
}

// `async_trait` lifts each transactional method into the generated impl body;
// keep the recovery reset legible as one atomic sequence instead of obscuring
// its lock/update/insert ordering behind many one-line helpers.
#[allow(clippy::too_many_lines)]
#[async_trait]
impl CredentialRepository for PostgresCredentialRepository {
    async fn get_active_account_recovery_code(
        &self,
    ) -> Result<Option<AccountRecoveryCode>, CredentialRepositoryError> {
        let row = sqlx::query(
            "SELECT id, created_at, revision FROM account_recovery_codes \
             WHERE workspace_id = $1 AND user_id = $2 \
             AND consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.as_ref().map(account_recovery_code_from_row).transpose()
    }

    async fn create_or_rotate_account_recovery_code(
        &self,
        spec: AccountRecoveryCodeSpec,
        recovery_code: &OpaqueCredential<'_>,
        authorizing_session_id: Uuid,
    ) -> Result<CredentialMutation<AccountRecoveryCode>, CredentialRepositoryError> {
        require_kind(recovery_code, CredentialKind::AccountRecovery)?;
        validate_account_recovery_code(&spec)?;
        let token_hash = recovery_code.persistence_digest();
        let predecessor_revision = spec
            .replaces_recovery_code_revision
            .map(|revision| {
                i64::try_from(revision).map_err(|_| CredentialRepositoryError::InvalidInput)
            })
            .transpose()?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_device_authority_space(&mut transaction, self.scope).await?;
        validate_authorizing_device_session(
            &mut transaction,
            self.scope,
            Some(authorizing_session_id),
            Some(DEVICE_CLIENT_CONTRACT_VERSION),
            spec.created_at,
        )
        .await?;

        // This branch deliberately precedes the current-code CAS. A retried
        // request sees its exact active successor and recovers the committed
        // response; any changed identifier, secret, or predecessor conflicts.
        let replay = sqlx::query(
            "SELECT id, created_at, revision FROM account_recovery_codes \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND token_hash = $4 \
             AND predecessor_code_id IS NOT DISTINCT FROM $5 \
             AND predecessor_revision IS NOT DISTINCT FROM $6 \
             AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= $7 FOR SHARE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.id)
        .bind(token_hash.as_slice())
        .bind(spec.replaces_recovery_code_id)
        .bind(predecessor_revision)
        .bind(spec.created_at)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some(row) = replay {
            let recovery_code = account_recovery_code_from_row(&row)?;
            transaction.commit().await.map_err(storage_error)?;
            return Ok(CredentialMutation {
                value: recovery_code,
                replayed: true,
            });
        }

        let current = sqlx::query(
            "SELECT id, created_at, revision FROM account_recovery_codes \
             WHERE workspace_id = $1 AND user_id = $2 \
             AND consumed_at IS NULL AND revoked_at IS NULL FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        match current {
            None if spec.replaces_recovery_code_id.is_none() && predecessor_revision.is_none() => {}
            Some(row) => {
                let current_code = account_recovery_code_from_row(&row)?;
                if current_code.created_at > spec.created_at
                    || spec.replaces_recovery_code_id != Some(current_code.id)
                    || spec.replaces_recovery_code_revision != Some(current_code.revision)
                    || current_code.id == spec.id
                {
                    return Err(CredentialRepositoryError::Conflict);
                }
                let next_revision = sqlx::query_scalar::<_, i64>(
                    "UPDATE account_recovery_codes \
                     SET revoked_at = $5, replacement_code_id = $4, revision = revision + 1 \
                     WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
                     AND revision = $6 AND consumed_at IS NULL AND revoked_at IS NULL \
                     RETURNING revision",
                )
                .bind(self.scope.workspace_id)
                .bind(self.scope.user_id)
                .bind(current_code.id)
                .bind(spec.id)
                .bind(spec.created_at)
                .bind(
                    i64::try_from(current_code.revision)
                        .map_err(|_| CredentialRepositoryError::Internal)?,
                )
                .fetch_optional(&mut *transaction)
                .await
                .map_err(storage_error)?
                .ok_or(CredentialRepositoryError::Conflict)?;
                insert_auth_audit(
                    &mut transaction,
                    self.scope,
                    "auth.account_recovery_code.rotated",
                    "account_recovery_code",
                    current_code.id,
                    Some(next_revision - 1),
                    Some(next_revision),
                    spec.created_at,
                )
                .await?;
            }
            None => return Err(CredentialRepositoryError::Conflict),
        }

        let inserted = sqlx::query(
            "INSERT INTO account_recovery_codes (id, workspace_id, user_id, token_hash, \
             predecessor_code_id, predecessor_revision, created_at, revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1) \
             RETURNING id, created_at, revision",
        )
        .bind(spec.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(token_hash.as_slice())
        .bind(spec.replaces_recovery_code_id)
        .bind(predecessor_revision)
        .bind(spec.created_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(write_error)?;
        let recovery_code = account_recovery_code_from_row(&inserted)?;
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.account_recovery_code.created",
            "account_recovery_code",
            spec.id,
            None,
            Some(1),
            spec.created_at,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(CredentialMutation {
            value: recovery_code,
            replayed: false,
        })
    }

    #[allow(clippy::too_many_lines)] // Recovery and all local authority fencing must be atomic.
    async fn consume_account_recovery_code(
        &self,
        recovery_code: &OpaqueCredential<'_>,
        spec: AccountRecoverySessionSpec,
        access_token: &OpaqueCredential<'_>,
        refresh_token: &OpaqueCredential<'_>,
        successor_recovery_code: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<CredentialMutation<AccountRecoveryConsumption>, CredentialRepositoryError> {
        require_kind(recovery_code, CredentialKind::AccountRecovery)?;
        require_kind(access_token, CredentialKind::DeviceAccess)?;
        require_kind(refresh_token, CredentialKind::DeviceRefresh)?;
        require_kind(successor_recovery_code, CredentialKind::AccountRecovery)?;
        require_pairwise_distinct_material(&[
            recovery_code,
            access_token,
            refresh_token,
            successor_recovery_code,
        ])?;
        validate_account_recovery_session(&spec)?;

        let recovery_hash = recovery_code.persistence_digest();
        let access_hash = access_token.persistence_digest();
        let refresh_hash = refresh_token.persistence_digest();
        let successor_hash = successor_recovery_code.persistence_digest();
        let access_expires_at = checked_add(now, ACCESS_TOKEN_TTL)?;
        let refresh_idle_expires_at = checked_add(now, DEVICE_SESSION_REFRESH_IDLE_TTL)?;
        let absolute_expires_at = checked_add(now, DEVICE_SESSION_ABSOLUTE_TTL)?;
        let owner_scopes = full_owner_device_scopes();
        let stored_owner_scopes = scope_names(&owner_scopes);
        let contract_version = i16::try_from(spec.client_contract_version)
            .map_err(|_| CredentialRepositoryError::InvalidInput)?;

        // Reject random bearer probes without joining the serialized owner
        // authority queue. This is only an optimization: the row is queried
        // and fully validated again after the advisory lock is acquired.
        let recovery_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM account_recovery_codes \
             WHERE workspace_id = $1 AND user_id = $2 AND token_hash = $3)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(recovery_hash.as_slice())
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        if !recovery_exists {
            return Err(CredentialRepositoryError::InvalidCredential);
        }

        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_device_authority_space(&mut transaction, self.scope).await?;
        let code = sqlx::query(
            "SELECT id, created_at, consumed_at, revoked_at, recovered_session_id, \
             replacement_code_id, revision FROM account_recovery_codes \
             WHERE workspace_id = $1 AND user_id = $2 AND token_hash = $3 \
             AND created_at <= $4 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(recovery_hash.as_slice())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(CredentialRepositoryError::InvalidCredential)?;
        let recovery_code_id: Uuid = code.try_get("id").map_err(storage_error)?;
        let consumed_at: Option<DateTime<Utc>> =
            code.try_get("consumed_at").map_err(storage_error)?;
        let revoked_at: Option<DateTime<Utc>> =
            code.try_get("revoked_at").map_err(storage_error)?;
        let recovered_session_id: Option<Uuid> = code
            .try_get("recovered_session_id")
            .map_err(storage_error)?;
        let replacement_code_id: Option<Uuid> =
            code.try_get("replacement_code_id").map_err(storage_error)?;
        let recovery_revision: i64 = code.try_get("revision").map_err(storage_error)?;
        if recovery_revision <= 0 {
            return Err(CredentialRepositoryError::Internal);
        }
        if revoked_at.is_some() {
            return Err(CredentialRepositoryError::InvalidCredential);
        }

        // A consumed code remains as a hash-only receipt. It can recover only
        // the exact successor/session tuple committed by the first request.
        if consumed_at.is_some() {
            if recovered_session_id != Some(spec.session_id)
                || replacement_code_id != Some(spec.successor_recovery_code_id)
                || recovery_revision <= 1
            {
                return Err(CredentialRepositoryError::InvalidCredential);
            }
            let session_row = sqlx::query(
                "SELECT id, workspace_id, user_id, client_instance_id, client_kind, \
                 device_label, scopes, created_at, last_seen_at, expires_at, \
                 refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
                 client_contract_version, client_version, client_capabilities FROM sessions \
                 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND token_hash = $4 \
                 AND refresh_token_hash = $5 AND client_instance_id = $6 AND client_kind = $7 \
                 AND device_label = $8 AND scopes = $9 AND client_contract_version = $10 \
                 AND client_version = $11 AND client_capabilities = $12 \
                 AND auth_version = 1 AND revoked_at IS NULL AND created_at <= $13 \
                 AND refresh_idle_expires_at > $13 AND absolute_expires_at > $13",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(spec.session_id)
            .bind(access_hash.as_slice())
            .bind(refresh_hash.as_slice())
            .bind(spec.client_instance_id)
            .bind(spec.client_kind.as_storage_name())
            .bind(&spec.device_label)
            .bind(&stored_owner_scopes)
            .bind(contract_version)
            .bind(&spec.client_version)
            .bind(&spec.client_capabilities)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage_error)?
            .ok_or(CredentialRepositoryError::InvalidCredential)?;
            let session = device_session_from_row(&session_row)?;
            let successor_row = sqlx::query(
                "SELECT id, created_at, revision FROM account_recovery_codes \
                 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND token_hash = $4 \
                 AND predecessor_code_id = $5 AND predecessor_revision = $6 \
                 AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= $7",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(spec.successor_recovery_code_id)
            .bind(successor_hash.as_slice())
            .bind(recovery_code_id)
            .bind(recovery_revision - 1)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage_error)?
            .ok_or(CredentialRepositoryError::InvalidCredential)?;
            let successor = account_recovery_code_from_row(&successor_row)?;
            transaction.commit().await.map_err(storage_error)?;
            return Ok(CredentialMutation {
                value: AccountRecoveryConsumption {
                    session,
                    successor_recovery_code: successor,
                },
                replayed: true,
            });
        }
        if recovered_session_id.is_some()
            || replacement_code_id.is_some()
            || spec.successor_recovery_code_id == recovery_code_id
        {
            return Err(CredentialRepositoryError::InvalidCredential);
        }

        // Recovery is a local authority reset. Set-based updates keep the
        // transaction's memory use bounded even if historical rows exist.
        let revoked_device_sessions = sqlx::query(
            "UPDATE sessions SET revoked_at = GREATEST(created_at, $3), revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 AND auth_version = 1 \
             AND revoked_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?
        .rows_affected();
        let revoked_device_enrollments = sqlx::query(
            "UPDATE device_enrollments \
             SET revoked_at = GREATEST(created_at, $3), revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 \
             AND consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?
        .rows_affected();
        let revoked_mcp_clients = sqlx::query(
            "UPDATE mcp_clients SET status = 'revoked', \
             revoked_at = GREATEST(created_at, $3), \
             updated_at = GREATEST(updated_at, $3), revision = revision + 1 \
             WHERE workspace_id = $1 AND created_by_user_id = $2 AND auth_version = 1 \
             AND status <> 'revoked' AND revoked_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?
        .rows_affected();

        let recovered_session_row = sqlx::query(
            "INSERT INTO sessions (id, workspace_id, user_id, token_hash, client_kind, \
             device_label, metadata, created_at, last_seen_at, expires_at, auth_version, \
             client_instance_id, refresh_token_hash, scopes, refresh_idle_expires_at, \
             absolute_expires_at, credential_issued_at, revision, client_contract_version, \
             client_version, client_capabilities) \
             VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, $7, $7, $8, 1, $9, $10, $11, \
             $12, $13, $7, 1, $14, $15, $16) \
             RETURNING id, workspace_id, user_id, client_instance_id, client_kind, \
             device_label, scopes, created_at, last_seen_at, expires_at, \
             refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
             client_contract_version, client_version, client_capabilities",
        )
        .bind(spec.session_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(access_hash.as_slice())
        .bind(spec.client_kind.as_storage_name())
        .bind(&spec.device_label)
        .bind(now)
        .bind(access_expires_at)
        .bind(spec.client_instance_id)
        .bind(refresh_hash.as_slice())
        .bind(&stored_owner_scopes)
        .bind(refresh_idle_expires_at)
        .bind(absolute_expires_at)
        .bind(contract_version)
        .bind(&spec.client_version)
        .bind(&spec.client_capabilities)
        .fetch_one(&mut *transaction)
        .await
        .map_err(write_error)?;
        let recovered_session = device_session_from_row(&recovered_session_row)?;

        let consumed_revision = sqlx::query_scalar::<_, i64>(
            "UPDATE account_recovery_codes SET consumed_at = $6, recovered_session_id = $4, \
             replacement_code_id = $5, revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 AND revision = $7 \
             AND consumed_at IS NULL AND revoked_at IS NULL RETURNING revision",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(recovery_code_id)
        .bind(spec.session_id)
        .bind(spec.successor_recovery_code_id)
        .bind(now)
        .bind(recovery_revision)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(CredentialRepositoryError::InvalidCredential)?;

        let successor_row = sqlx::query(
            "INSERT INTO account_recovery_codes (id, workspace_id, user_id, token_hash, \
             predecessor_code_id, predecessor_revision, created_at, revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1) \
             RETURNING id, created_at, revision",
        )
        .bind(spec.successor_recovery_code_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(successor_hash.as_slice())
        .bind(recovery_code_id)
        .bind(recovery_revision)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(write_error)?;
        let successor = account_recovery_code_from_row(&successor_row)?;

        insert_account_recovery_consumption_audit(
            &mut transaction,
            self.scope,
            recovery_code_id,
            recovery_revision,
            consumed_revision,
            revoked_device_sessions,
            revoked_device_enrollments,
            revoked_mcp_clients,
            now,
        )
        .await?;
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.device_session.recovered",
            "device_session",
            spec.session_id,
            None,
            Some(1),
            now,
        )
        .await?;
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.account_recovery_code.created",
            "account_recovery_code",
            spec.successor_recovery_code_id,
            None,
            Some(1),
            now,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;

        Ok(CredentialMutation {
            value: AccountRecoveryConsumption {
                session: recovered_session,
                successor_recovery_code: successor,
            },
            replayed: false,
        })
    }

    async fn create_device_enrollment(
        &self,
        spec: DeviceEnrollmentSpec,
        enrollment_token: &OpaqueCredential<'_>,
    ) -> Result<(), CredentialRepositoryError> {
        require_kind(enrollment_token, CredentialKind::Enrollment)?;
        validate_device_enrollment(&spec)?;
        let expires_at = spec
            .created_at
            .checked_add_signed(ENROLLMENT_TOKEN_TTL)
            .ok_or(CredentialRepositoryError::InvalidInput)?;
        let token_hash = enrollment_token.persistence_digest();
        let scopes = scope_names(&spec.scopes);
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_device_authority_space(&mut transaction, self.scope).await?;
        ensure_pending_enrollment_capacity(&mut transaction, self.scope, spec.created_at).await?;
        sqlx::query(
            "INSERT INTO device_enrollments (id, workspace_id, user_id, client_instance_id, \
             client_kind, device_label, token_hash, scopes, created_at, expires_at, \
             client_contract_version, client_version, client_capabilities) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(spec.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.client_instance_id)
        .bind(spec.client_kind.as_storage_name())
        .bind(spec.device_label)
        .bind(token_hash.as_slice())
        .bind(scopes)
        .bind(spec.created_at)
        .bind(expires_at)
        .bind(
            i16::try_from(spec.client_contract_version)
                .map_err(|_| CredentialRepositoryError::InvalidInput)?,
        )
        .bind(spec.client_version)
        .bind(spec.client_capabilities)
        .execute(&mut *transaction)
        .await
        .map_err(write_error)?;
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.device_enrollment.created",
            "device_enrollment",
            spec.id,
            None,
            Some(1),
            spec.created_at,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }

    async fn create_or_replay_device_enrollment(
        &self,
        spec: DeviceEnrollmentSpec,
        enrollment_token: &OpaqueCredential<'_>,
        authorizing_session_id: Option<Uuid>,
    ) -> Result<CredentialMutation<DeviceEnrollmentCreation>, CredentialRepositoryError> {
        require_kind(enrollment_token, CredentialKind::Enrollment)?;
        validate_device_enrollment(&spec)?;
        let expires_at = checked_add(spec.created_at, ENROLLMENT_TOKEN_TTL)?;
        let token_hash = enrollment_token.persistence_digest();
        let scopes = scope_names(&spec.scopes);
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_device_authority_space(&mut transaction, self.scope).await?;
        validate_authorizing_device_session(
            &mut transaction,
            self.scope,
            authorizing_session_id,
            None,
            spec.created_at,
        )
        .await?;

        // Capacity must not break response-loss recovery: the exact same live
        // request remains replayable even when all pending slots are occupied.
        let replayed_expires_at = sqlx::query_scalar::<_, DateTime<Utc>>(
            "SELECT expires_at FROM device_enrollments WHERE id = $1 \
             AND workspace_id = $2 AND user_id = $3 AND client_instance_id = $4 \
             AND client_kind = $5 AND device_label = $6 AND token_hash = $7 AND scopes = $8 \
             AND client_contract_version = $9 AND client_version = $10 \
             AND client_capabilities = $11 AND consumed_at IS NULL AND revoked_at IS NULL \
             AND created_at <= $12 AND expires_at > $12 \
             AND expires_at <= created_at + interval '600 seconds' FOR SHARE",
        )
        .bind(spec.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.client_instance_id)
        .bind(spec.client_kind.as_storage_name())
        .bind(&spec.device_label)
        .bind(token_hash.as_slice())
        .bind(&scopes)
        .bind(
            i16::try_from(spec.client_contract_version)
                .map_err(|_| CredentialRepositoryError::InvalidInput)?,
        )
        .bind(&spec.client_version)
        .bind(&spec.client_capabilities)
        .bind(spec.created_at)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some(expires_at) = replayed_expires_at {
            transaction.commit().await.map_err(storage_error)?;
            return Ok(CredentialMutation {
                value: DeviceEnrollmentCreation { expires_at },
                replayed: true,
            });
        }

        ensure_pending_enrollment_capacity(&mut transaction, self.scope, spec.created_at).await?;
        sqlx::query(
            "INSERT INTO device_enrollments (id, workspace_id, user_id, client_instance_id, \
             client_kind, device_label, token_hash, scopes, created_at, expires_at, \
             client_contract_version, client_version, client_capabilities) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(spec.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.client_instance_id)
        .bind(spec.client_kind.as_storage_name())
        .bind(&spec.device_label)
        .bind(token_hash.as_slice())
        .bind(&scopes)
        .bind(spec.created_at)
        .bind(expires_at)
        .bind(
            i16::try_from(spec.client_contract_version)
                .map_err(|_| CredentialRepositoryError::InvalidInput)?,
        )
        .bind(&spec.client_version)
        .bind(&spec.client_capabilities)
        .execute(&mut *transaction)
        .await
        .map_err(write_error)?;
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.device_enrollment.created",
            "device_enrollment",
            spec.id,
            None,
            Some(1),
            spec.created_at,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(CredentialMutation {
            value: DeviceEnrollmentCreation { expires_at },
            replayed: false,
        })
    }

    #[allow(clippy::too_many_lines)] // Keeps the one-time claim and session issue transaction together.
    async fn consume_device_enrollment(
        &self,
        enrollment_token: &OpaqueCredential<'_>,
        session_id: Uuid,
        access_token: &OpaqueCredential<'_>,
        refresh_token: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError> {
        require_kind(enrollment_token, CredentialKind::Enrollment)?;
        require_kind(access_token, CredentialKind::DeviceAccess)?;
        require_kind(refresh_token, CredentialKind::DeviceRefresh)?;
        require_pairwise_distinct_material(&[enrollment_token, access_token, refresh_token])?;
        let enrollment_hash = enrollment_token.persistence_digest();
        let access_hash = access_token.persistence_digest();
        let refresh_hash = refresh_token.persistence_digest();
        let access_expires_at = checked_add(now, ACCESS_TOKEN_TTL)?;
        let refresh_idle_expires_at = checked_add(now, DEVICE_SESSION_REFRESH_IDLE_TTL)?;
        let absolute_expires_at = checked_add(now, DEVICE_SESSION_ABSOLUTE_TTL)?;

        // Unknown enrollment secrets must not contend on the per-owner lock.
        // The locked query below remains the authority for state and replay.
        let enrollment_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM device_enrollments \
             WHERE workspace_id = $1 AND user_id = $2 AND token_hash = $3)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(enrollment_hash.as_slice())
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        if !enrollment_exists {
            return Err(CredentialRepositoryError::InvalidCredential);
        }

        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        // Lock before any enrollment row so every writer uses one canonical
        // order and distinct installations cannot race past the owner cap.
        lock_device_authority_space(&mut transaction, self.scope).await?;
        let enrollment = sqlx::query(
            "SELECT id, client_instance_id, client_kind, device_label, scopes, expires_at, \
             consumed_session_id, revoked_at, client_contract_version, client_version, \
             client_capabilities \
             FROM device_enrollments \
             WHERE workspace_id = $1 AND user_id = $2 AND token_hash = $3 \
             AND created_at <= $4 FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(enrollment_hash.as_slice())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(CredentialRepositoryError::InvalidCredential)?;
        let enrollment_id: Uuid = enrollment.try_get("id").map_err(storage_error)?;
        let revoked_at: Option<DateTime<Utc>> =
            enrollment.try_get("revoked_at").map_err(storage_error)?;
        if revoked_at.is_some() {
            return Err(CredentialRepositoryError::InvalidCredential);
        }
        let consumed_session_id: Option<Uuid> = enrollment
            .try_get("consumed_session_id")
            .map_err(storage_error)?;
        if let Some(consumed_session_id) = consumed_session_id {
            if consumed_session_id != session_id {
                return Err(CredentialRepositoryError::InvalidCredential);
            }
            let row = sqlx::query(
                "SELECT id, workspace_id, user_id, client_instance_id, client_kind, \
                 device_label, scopes, created_at, last_seen_at, expires_at, \
                 refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
                 client_contract_version, client_version, client_capabilities \
                 FROM sessions WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
                 AND auth_version = 1 AND revoked_at IS NULL AND token_hash = $4 \
                 AND refresh_token_hash = $5 \
                 AND refresh_idle_expires_at > $6 AND absolute_expires_at > $6",
            )
            .bind(self.scope.workspace_id)
            .bind(self.scope.user_id)
            .bind(session_id)
            .bind(access_hash.as_slice())
            .bind(refresh_hash.as_slice())
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage_error)?
            .ok_or(CredentialRepositoryError::InvalidCredential)?;
            let session = device_session_from_row(&row)?;
            transaction.commit().await.map_err(storage_error)?;
            return Ok(CredentialMutation {
                value: session,
                replayed: true,
            });
        }
        let enrollment_expires_at: DateTime<Utc> =
            enrollment.try_get("expires_at").map_err(storage_error)?;
        if enrollment_expires_at <= now {
            return Err(CredentialRepositoryError::InvalidCredential);
        }
        let client_instance_id: Uuid = enrollment
            .try_get("client_instance_id")
            .map_err(storage_error)?;
        let client_kind = parse_client_kind(
            enrollment
                .try_get::<String, _>("client_kind")
                .map_err(storage_error)?
                .as_str(),
        )?;
        let device_label: String = enrollment.try_get("device_label").map_err(storage_error)?;
        let stored_scopes: Vec<String> = enrollment.try_get("scopes").map_err(storage_error)?;
        let scopes = parse_scopes(&stored_scopes)?;
        let client_contract_version = parse_contract_version(&enrollment)?;
        let client_version: String = enrollment
            .try_get("client_version")
            .map_err(storage_error)?;
        let client_capabilities: Vec<String> = enrollment
            .try_get("client_capabilities")
            .map_err(storage_error)?;
        if !valid_label(&device_label, 200)
            || !valid_scopes(&scopes)
            || !scopes.iter().all(|scope| scope.is_rest())
            || !valid_device_stored_metadata(
                client_contract_version,
                &scopes,
                &client_version,
                &client_capabilities,
            )
        {
            return Err(CredentialRepositoryError::Internal);
        }

        // A successful enrollment replaces any previous session for the same
        // app installation. This update and the partial unique index make the
        // single-active-session invariant durable under concurrent enrollment.
        sqlx::query(
            "UPDATE sessions SET revoked_at = GREATEST(created_at, $4), revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 AND client_instance_id = $3 \
             AND auth_version = 1 AND revoked_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(client_instance_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;

        ensure_active_session_capacity(&mut transaction, self.scope, now).await?;

        sqlx::query(
            "INSERT INTO sessions (id, workspace_id, user_id, token_hash, client_kind, \
             device_label, metadata, created_at, last_seen_at, expires_at, auth_version, \
             client_instance_id, refresh_token_hash, scopes, refresh_idle_expires_at, \
             absolute_expires_at, credential_issued_at, revision, client_contract_version, \
             client_version, client_capabilities) \
             VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, $7, $7, $8, 1, $9, $10, $11, \
             $12, $13, $7, 1, $14, $15, $16)",
        )
        .bind(session_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(access_hash.as_slice())
        .bind(client_kind.as_storage_name())
        .bind(&device_label)
        .bind(now)
        .bind(access_expires_at)
        .bind(client_instance_id)
        .bind(refresh_hash.as_slice())
        .bind(scope_names(&scopes))
        .bind(refresh_idle_expires_at)
        .bind(absolute_expires_at)
        .bind(
            i16::try_from(client_contract_version)
                .map_err(|_| CredentialRepositoryError::Internal)?,
        )
        .bind(&client_version)
        .bind(&client_capabilities)
        .execute(&mut *transaction)
        .await
        .map_err(write_error)?;

        let consumed = sqlx::query(
            "UPDATE device_enrollments SET consumed_at = $5, consumed_session_id = $4, \
             revision = revision + 1 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
             AND consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(enrollment_id)
        .bind(session_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?
        .rows_affected();
        if consumed != 1 {
            return Err(CredentialRepositoryError::InvalidCredential);
        }
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.device_enrollment.consumed",
            "device_session",
            session_id,
            None,
            Some(1),
            now,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;

        Ok(CredentialMutation {
            value: DeviceSession {
                id: session_id,
                workspace_id: self.scope.workspace_id,
                user_id: self.scope.user_id,
                client_instance_id,
                client_kind,
                device_label,
                scopes,
                client_contract_version,
                client_version,
                client_capabilities,
                created_at: now,
                last_seen_at: now,
                credential_issued_at: now,
                access_expires_at,
                refresh_idle_expires_at,
                absolute_expires_at,
                revision: 1,
            },
            replayed: false,
        })
    }

    async fn revoke_device_enrollment(
        &self,
        enrollment_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, CredentialRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let changed = sqlx::query(
            "UPDATE device_enrollments SET revoked_at = GREATEST(created_at, $4), \
             revision = revision + 1 WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
             AND consumed_at IS NULL AND revoked_at IS NULL RETURNING revision",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(enrollment_id)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some(row) = changed {
            let revision: i64 = row.try_get("revision").map_err(storage_error)?;
            insert_auth_audit(
                &mut transaction,
                self.scope,
                "auth.device_enrollment.revoked",
                "device_enrollment",
                enrollment_id,
                Some(revision - 1),
                Some(revision),
                now,
            )
            .await?;
            transaction.commit().await.map_err(storage_error)?;
            Ok(true)
        } else {
            transaction.commit().await.map_err(storage_error)?;
            Ok(false)
        }
    }

    async fn authenticate_device_access(
        &self,
        access_token: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<DeviceSession, CredentialRepositoryError> {
        require_kind(access_token, CredentialKind::DeviceAccess)?;
        let token_hash = access_token.persistence_digest();
        let row = sqlx::query(
            "UPDATE sessions SET last_seen_at = GREATEST(last_seen_at, $4) \
             WHERE workspace_id = $1 AND user_id = $2 AND token_hash = $3 \
             AND auth_version = 1 AND revoked_at IS NULL AND created_at <= $4 \
             AND credential_issued_at <= $4 \
             AND expires_at > $4 AND absolute_expires_at > $4 \
             RETURNING id, workspace_id, user_id, client_instance_id, client_kind, \
             device_label, scopes, created_at, last_seen_at, expires_at, \
             refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
             client_contract_version, client_version, client_capabilities",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(token_hash.as_slice())
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(CredentialRepositoryError::InvalidCredential)?;
        device_session_from_row(&row)
    }

    #[allow(clippy::too_many_lines)] // Rotation, exact replay, and audit share one transaction.
    async fn refresh_device_session(
        &self,
        refresh_token: &OpaqueCredential<'_>,
        next_access_token: &OpaqueCredential<'_>,
        next_refresh_token: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError> {
        require_kind(refresh_token, CredentialKind::DeviceRefresh)?;
        require_kind(next_access_token, CredentialKind::DeviceAccess)?;
        require_kind(next_refresh_token, CredentialKind::DeviceRefresh)?;
        require_pairwise_distinct_material(&[
            refresh_token,
            next_access_token,
            next_refresh_token,
        ])?;
        let current_refresh_hash = refresh_token.persistence_digest();
        let next_access_hash = next_access_token.persistence_digest();
        let next_refresh_hash = next_refresh_token.persistence_digest();
        let current_refresh_as_access_hash =
            refresh_token.persistence_digest_for(CredentialKind::DeviceAccess);
        let next_refresh_as_access_hash =
            next_refresh_token.persistence_digest_for(CredentialKind::DeviceAccess);
        let requested_access_expiry = checked_add(now, ACCESS_TOKEN_TTL)?;
        let requested_idle_expiry = checked_add(now, DEVICE_SESSION_REFRESH_IDLE_TTL)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let row = sqlx::query(
            "UPDATE sessions SET token_hash = $4, previous_refresh_token_hash = refresh_token_hash, \
             refresh_token_hash = $5, \
             expires_at = LEAST($6, absolute_expires_at), \
             refresh_idle_expires_at = LEAST($7, absolute_expires_at), \
             last_seen_at = GREATEST(last_seen_at, $8), credential_issued_at = $8, \
             revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 AND refresh_token_hash = $3 \
             AND token_hash <> $4 AND auth_version = 1 AND revoked_at IS NULL \
             AND token_hash <> $9 AND token_hash <> $10 \
             AND created_at <= $8 \
             AND credential_issued_at <= $8 \
             AND refresh_idle_expires_at > $8 AND absolute_expires_at > $8 \
             RETURNING id, workspace_id, user_id, client_instance_id, client_kind, \
             device_label, scopes, created_at, last_seen_at, expires_at, \
             refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
             client_contract_version, client_version, client_capabilities",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(current_refresh_hash.as_slice())
        .bind(next_access_hash.as_slice())
        .bind(next_refresh_hash.as_slice())
        .bind(requested_access_expiry)
        .bind(requested_idle_expiry)
        .bind(now)
        .bind(current_refresh_as_access_hash.as_slice())
        .bind(next_refresh_as_access_hash.as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(write_error)?;
        if let Some(row) = row {
            let session = device_session_from_row(&row)?;
            insert_auth_audit(
                &mut transaction,
                self.scope,
                "auth.device_session.refreshed",
                "device_session",
                session.id,
                Some(
                    session
                        .revision
                        .saturating_sub(1)
                        .try_into()
                        .map_err(|_| CredentialRepositoryError::Internal)?,
                ),
                Some(
                    session
                        .revision
                        .try_into()
                        .map_err(|_| CredentialRepositoryError::Internal)?,
                ),
                now,
            )
            .await?;
            transaction.commit().await.map_err(storage_error)?;
            return Ok(CredentialMutation {
                value: session,
                replayed: false,
            });
        }

        // A caller must persist its proposed next pair before sending the
        // request. If the response was lost after commit, only that exact old
        // token + next pair can recover the committed result. A competing pair
        // or an older generation fails closed.
        let replay = sqlx::query(
            "SELECT id, workspace_id, user_id, client_instance_id, client_kind, \
             device_label, scopes, created_at, last_seen_at, expires_at, \
             refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
             client_contract_version, client_version, client_capabilities \
             FROM sessions WHERE workspace_id = $1 AND user_id = $2 \
             AND previous_refresh_token_hash = $3 AND token_hash = $4 \
             AND refresh_token_hash = $5 AND auth_version = 1 AND revoked_at IS NULL \
             AND refresh_idle_expires_at > $6 AND absolute_expires_at > $6",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(current_refresh_hash.as_slice())
        .bind(next_access_hash.as_slice())
        .bind(next_refresh_hash.as_slice())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(CredentialRepositoryError::InvalidCredential)?;
        let session = device_session_from_row(&replay)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(CredentialMutation {
            value: session,
            replayed: true,
        })
    }

    async fn list_device_sessions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<DeviceSession>, CredentialRepositoryError> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, user_id, client_instance_id, client_kind, \
             device_label, scopes, created_at, last_seen_at, expires_at, \
             refresh_idle_expires_at, absolute_expires_at, credential_issued_at, revision, \
             client_contract_version, client_version, client_capabilities \
             FROM sessions WHERE workspace_id = $1 AND user_id = $2 AND auth_version = 1 \
             AND revoked_at IS NULL AND refresh_idle_expires_at > $3 AND absolute_expires_at > $3 \
             ORDER BY last_seen_at DESC, id LIMIT $4",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .bind(
            i64::try_from(MAX_ACTIVE_DEVICE_SESSIONS + 1)
                .map_err(|_| CredentialRepositoryError::Internal)?,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        if rows.len() > MAX_ACTIVE_DEVICE_SESSIONS {
            return Err(CredentialRepositoryError::Internal);
        }
        rows.iter().map(device_session_from_row).collect()
    }

    async fn revoke_device_session(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, CredentialRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let changed = sqlx::query(
            "UPDATE sessions SET revoked_at = GREATEST(created_at, $4), revision = revision + 1 \
             WHERE workspace_id = $1 AND user_id = $2 AND id = $3 \
             AND auth_version = 1 AND revoked_at IS NULL RETURNING revision",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(session_id)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some(row) = changed {
            let revision: i64 = row.try_get("revision").map_err(storage_error)?;
            insert_auth_audit(
                &mut transaction,
                self.scope,
                "auth.device_session.revoked",
                "device_session",
                session_id,
                Some(revision - 1),
                Some(revision),
                now,
            )
            .await?;
            transaction.commit().await.map_err(storage_error)?;
            Ok(true)
        } else {
            transaction.commit().await.map_err(storage_error)?;
            Ok(false)
        }
    }

    async fn register_mcp_client(
        &self,
        spec: McpClientSpec,
        credential: &OpaqueCredential<'_>,
        authorizing_session_id: Option<Uuid>,
    ) -> Result<McpClient, CredentialRepositoryError> {
        require_kind(credential, CredentialKind::McpClient)?;
        validate_mcp_client(&spec)?;
        let default_expires_at = checked_add(spec.created_at, MCP_CREDENTIAL_DEFAULT_TTL)?;
        let max_expires_at = checked_add(spec.created_at, MAX_MCP_CREDENTIAL_TTL)?;
        let expires_at = spec.requested_expires_at.unwrap_or(default_expires_at);
        if expires_at <= spec.created_at || expires_at > max_expires_at {
            return Err(CredentialRepositoryError::InvalidInput);
        }
        let credential_hash = credential.persistence_digest();
        let scopes = scope_names(&spec.scopes);
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_device_authority_space(&mut transaction, self.scope).await?;
        validate_authorizing_device_session(
            &mut transaction,
            self.scope,
            authorizing_session_id,
            None,
            spec.created_at,
        )
        .await?;
        let row = sqlx::query(
            "INSERT INTO mcp_clients (id, workspace_id, created_by_user_id, client_identifier, \
             display_name, credential_hash, scopes, allowed_origins, status, revision, \
             created_at, updated_at, expires_at, auth_version, client_contract_version, \
             client_version, client_capabilities) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active', 1, $9, $9, $10, 1, \
             $11, $12, $13) \
             RETURNING id, workspace_id, created_by_user_id, client_identifier, display_name, \
             scopes, allowed_origins, created_at, last_seen_at, expires_at, revision, \
             client_contract_version, client_version, client_capabilities",
        )
        .bind(spec.id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(spec.client_identifier)
        .bind(spec.display_name)
        .bind(credential_hash.as_slice())
        .bind(scopes)
        .bind(spec.allowed_origins)
        .bind(spec.created_at)
        .bind(expires_at)
        .bind(
            i16::try_from(spec.client_contract_version)
                .map_err(|_| CredentialRepositoryError::InvalidInput)?,
        )
        .bind(spec.client_version)
        .bind(spec.client_capabilities)
        .fetch_one(&mut *transaction)
        .await
        .map_err(write_error)?;
        let client = mcp_client_from_row(&row)?;
        insert_auth_audit(
            &mut transaction,
            self.scope,
            "auth.mcp_client.created",
            "mcp_client",
            client.id,
            None,
            Some(1),
            client.created_at,
        )
        .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(client)
    }

    async fn authenticate_mcp_client(
        &self,
        credential: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<McpClient, CredentialRepositoryError> {
        require_kind(credential, CredentialKind::McpClient)?;
        let credential_hash = credential.persistence_digest();
        let row = sqlx::query(
            "UPDATE mcp_clients SET last_seen_at = GREATEST(COALESCE(last_seen_at, $4), $4) \
             WHERE workspace_id = $1 AND created_by_user_id = $2 AND credential_hash = $3 \
             AND auth_version = 1 AND status = 'active' AND revoked_at IS NULL \
             AND created_at <= $4 AND expires_at > $4 \
             RETURNING id, workspace_id, created_by_user_id, client_identifier, display_name, \
             scopes, allowed_origins, created_at, last_seen_at, expires_at, revision, \
             client_contract_version, client_version, client_capabilities",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(credential_hash.as_slice())
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(CredentialRepositoryError::InvalidCredential)?;
        mcp_client_from_row(&row)
    }

    async fn list_mcp_clients(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<McpClient>, CredentialRepositoryError> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, created_by_user_id, client_identifier, display_name, \
             scopes, allowed_origins, created_at, last_seen_at, expires_at, revision, \
             client_contract_version, client_version, client_capabilities \
             FROM mcp_clients WHERE workspace_id = $1 AND created_by_user_id = $2 \
             AND auth_version = 1 AND status = 'active' AND revoked_at IS NULL \
             AND expires_at > $3 ORDER BY COALESCE(last_seen_at, created_at) DESC, id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(mcp_client_from_row).collect()
    }

    async fn revoke_mcp_client(
        &self,
        client_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, CredentialRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let changed = sqlx::query(
            "UPDATE mcp_clients SET status = 'revoked', revoked_at = GREATEST(created_at, $4), \
             updated_at = GREATEST(updated_at, $4), revision = revision + 1 \
             WHERE workspace_id = $1 AND created_by_user_id = $2 AND id = $3 \
             AND auth_version = 1 AND status <> 'revoked' AND revoked_at IS NULL \
             RETURNING revision",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(client_id)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some(row) = changed {
            let revision: i64 = row.try_get("revision").map_err(storage_error)?;
            insert_auth_audit(
                &mut transaction,
                self.scope,
                "auth.mcp_client.revoked",
                "mcp_client",
                client_id,
                Some(revision - 1),
                Some(revision),
                now,
            )
            .await?;
            transaction.commit().await.map_err(storage_error)?;
            Ok(true)
        } else {
            transaction.commit().await.map_err(storage_error)?;
            Ok(false)
        }
    }
}

fn require_kind(
    credential: &OpaqueCredential<'_>,
    expected: CredentialKind,
) -> Result<(), CredentialRepositoryError> {
    if credential.kind() == expected {
        Ok(())
    } else {
        Err(CredentialRepositoryError::InvalidCredential)
    }
}

fn require_pairwise_distinct_material(
    credentials: &[&OpaqueCredential<'_>],
) -> Result<(), CredentialRepositoryError> {
    for (index, credential) in credentials.iter().enumerate() {
        for other in &credentials[index + 1..] {
            if credential
                .has_same_secret_material(other)
                .map_err(|_| CredentialRepositoryError::InvalidCredential)?
            {
                return Err(CredentialRepositoryError::InvalidInput);
            }
        }
    }
    Ok(())
}

fn checked_add(
    value: DateTime<Utc>,
    duration: chrono::Duration,
) -> Result<DateTime<Utc>, CredentialRepositoryError> {
    value
        .checked_add_signed(duration)
        .ok_or(CredentialRepositoryError::InvalidInput)
}

/// Serializes capacity-changing device-authority transactions for one owner.
///
/// The advisory namespace is domain-separated, and every caller acquires this
/// lock before an enrollment row lock to keep the ordering deadlock-free.
async fn lock_device_authority_space(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), CredentialRepositoryError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'dayweave.device-authority.v1:' || $1::text || ':' || $2::text, 0))",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

/// Rechecks a pre-authenticated device only after acquiring the owner
/// authority lock. `None` is reserved for the configured legacy bootstrap
/// principal during the bounded hybrid rollout.
async fn validate_authorizing_device_session(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    authorizing_session_id: Option<Uuid>,
    required_contract_version: Option<u16>,
    now: DateTime<Utc>,
) -> Result<(), CredentialRepositoryError> {
    let Some(session_id) = authorizing_session_id else {
        return Ok(());
    };
    if session_id.is_nil() {
        return Err(CredentialRepositoryError::InvalidCredential);
    }
    let required_contract_version = required_contract_version
        .map(|version| i16::try_from(version).map_err(|_| CredentialRepositoryError::Internal))
        .transpose()?;
    let authorized = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE workspace_id = $1 AND user_id = $2 \
         AND id = $3 AND auth_version = 1 AND revoked_at IS NULL AND created_at <= $4 \
         AND credential_issued_at <= $4 AND expires_at > $4 \
         AND refresh_idle_expires_at > $4 AND absolute_expires_at > $4 \
         AND ($5::smallint IS NULL OR client_contract_version = $5))",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(session_id)
    .bind(now)
    .bind(required_contract_version)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if authorized {
        Ok(())
    } else {
        Err(CredentialRepositoryError::InvalidCredential)
    }
}

async fn ensure_pending_enrollment_capacity(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), CredentialRepositoryError> {
    let pending = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM device_enrollments \
         WHERE workspace_id = $1 AND user_id = $2 \
         AND consumed_at IS NULL AND revoked_at IS NULL AND expires_at > $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    let maximum = i64::try_from(MAX_PENDING_DEVICE_ENROLLMENTS)
        .map_err(|_| CredentialRepositoryError::Internal)?;
    if pending >= maximum {
        return Err(CredentialRepositoryError::Conflict);
    }
    Ok(())
}

async fn ensure_active_session_capacity(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), CredentialRepositoryError> {
    // This is intentionally identical to the active-authority predicate used
    // by list_device_sessions. Access-token expiry is not session expiry: a
    // refreshable session still occupies a slot after its access token lapses.
    let active = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM sessions \
         WHERE workspace_id = $1 AND user_id = $2 AND auth_version = 1 \
         AND revoked_at IS NULL AND refresh_idle_expires_at > $3 AND absolute_expires_at > $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    let maximum = i64::try_from(MAX_ACTIVE_DEVICE_SESSIONS)
        .map_err(|_| CredentialRepositoryError::Internal)?;
    if active >= maximum {
        return Err(CredentialRepositoryError::Conflict);
    }
    Ok(())
}

fn validate_device_enrollment(
    spec: &DeviceEnrollmentSpec,
) -> Result<(), CredentialRepositoryError> {
    if spec.id.is_nil()
        || spec.client_instance_id.is_nil()
        || !valid_label(&spec.device_label, 200)
        || !valid_scopes(&spec.scopes)
        || !spec.scopes.iter().all(|scope| scope.is_rest())
        || !valid_client_metadata(
            spec.client_contract_version,
            DEVICE_CLIENT_CONTRACT_VERSION,
            &spec.client_version,
            &spec.client_capabilities,
        )
    {
        return Err(CredentialRepositoryError::InvalidInput);
    }
    Ok(())
}

fn validate_account_recovery_code(
    spec: &AccountRecoveryCodeSpec,
) -> Result<(), CredentialRepositoryError> {
    let predecessor_is_valid = match (
        spec.replaces_recovery_code_id,
        spec.replaces_recovery_code_revision,
    ) {
        (None, None) => true,
        (Some(id), Some(revision)) => !id.is_nil() && id != spec.id && revision > 0,
        (None, Some(_)) | (Some(_), None) => false,
    };
    if spec.id.is_nil() || !predecessor_is_valid {
        return Err(CredentialRepositoryError::InvalidInput);
    }
    Ok(())
}

fn validate_account_recovery_session(
    spec: &AccountRecoverySessionSpec,
) -> Result<(), CredentialRepositoryError> {
    if spec.session_id.is_nil()
        || spec.client_instance_id.is_nil()
        || spec.successor_recovery_code_id.is_nil()
        || !valid_label(&spec.device_label, 200)
        || !valid_client_metadata(
            spec.client_contract_version,
            DEVICE_CLIENT_CONTRACT_VERSION,
            &spec.client_version,
            &spec.client_capabilities,
        )
    {
        return Err(CredentialRepositoryError::InvalidInput);
    }
    Ok(())
}

fn validate_mcp_client(spec: &McpClientSpec) -> Result<(), CredentialRepositoryError> {
    if !valid_label(&spec.client_identifier, 300)
        || !valid_label(&spec.display_name, 200)
        || !valid_scopes(&spec.scopes)
        || !spec.scopes.iter().all(|scope| scope.is_mcp())
        || spec.allowed_origins.len() > 100
        || !valid_client_metadata(
            spec.client_contract_version,
            MCP_CLIENT_CONTRACT_VERSION,
            &spec.client_version,
            &spec.client_capabilities,
        )
    {
        return Err(CredentialRepositoryError::InvalidInput);
    }
    let mut origins = BTreeSet::new();
    if !spec
        .allowed_origins
        .iter()
        .all(|origin| origins.insert(origin.as_str()) && valid_origin(origin))
    {
        return Err(CredentialRepositoryError::InvalidInput);
    }
    Ok(())
}

fn valid_label(value: &str, maximum_characters: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= maximum_characters
        && !value.chars().any(char::is_control)
}

fn valid_client_metadata(
    contract_version: u16,
    expected_contract_version: u16,
    version: &str,
    capabilities: &[String],
) -> bool {
    contract_version == expected_contract_version
        && valid_label(version, 100)
        && capabilities.len() <= 100
        && capabilities
            .iter()
            .all(|capability| valid_label(capability, 100))
        && capabilities.iter().collect::<BTreeSet<_>>().len() == capabilities.len()
}

fn valid_device_stored_metadata(
    contract_version: u16,
    scopes: &[Scope],
    version: &str,
    capabilities: &[String],
) -> bool {
    matches!(contract_version, 1 | DEVICE_CLIENT_CONTRACT_VERSION)
        && (contract_version != 1 || !scopes.contains(&Scope::SchedulePublish))
        && valid_label(version, 100)
        && capabilities.len() <= 100
        && capabilities
            .iter()
            .all(|capability| valid_label(capability, 100))
        && capabilities.iter().collect::<BTreeSet<_>>().len() == capabilities.len()
}

fn valid_scopes(scopes: &[Scope]) -> bool {
    if scopes.is_empty() || scopes.len() > Scope::ALL.len() {
        return false;
    }
    let mut names = BTreeSet::new();
    scopes
        .iter()
        .all(|scope| names.insert(scope.as_storage_name()))
}

#[allow(clippy::too_many_arguments)] // Counts are the bounded, content-free recovery evidence.
async fn insert_account_recovery_consumption_audit(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    recovery_code_id: Uuid,
    base_revision: i64,
    result_revision: i64,
    revoked_device_sessions: u64,
    revoked_device_enrollments: u64,
    revoked_mcp_clients: u64,
    occurred_at: DateTime<Utc>,
) -> Result<(), CredentialRepositoryError> {
    let metadata = serde_json::json!({
        "revoked_device_sessions": revoked_device_sessions,
        "revoked_device_enrollments": revoked_device_enrollments,
        "revoked_mcp_clients": revoked_mcp_clients,
    });
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, 'auth.account_recovery_code.consumed', \
         'account_recovery_code', $4, $5, $6, 'succeeded', $7, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(recovery_code_id)
    .bind(base_revision)
    .bind(result_revision)
    .bind(metadata)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // A flat, content-free row makes every audit field explicit.
async fn insert_auth_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: DatabaseScope,
    operation_type: &'static str,
    entity_type: &'static str,
    entity_id: Uuid,
    base_revision: Option<i64>,
    result_revision: Option<i64>,
    occurred_at: DateTime<Utc>,
) -> Result<(), CredentialRepositoryError> {
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'succeeded', '{}'::jsonb, $9)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(operation_type)
    .bind(entity_type)
    .bind(entity_id)
    .bind(base_revision)
    .bind(result_revision)
    .bind(occurred_at)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn valid_origin(value: &str) -> bool {
    if value.len() > 2_048 || value.contains('*') {
        return false;
    }
    let Ok(origin) = Url::parse(value) else {
        return false;
    };
    let Some(host) = origin.host_str() else {
        return false;
    };
    let transport_allowed = origin.scheme() == "https"
        || (origin.scheme() == "http"
            && (host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())));
    transport_allowed
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.query().is_none()
        && origin.fragment().is_none()
        && origin.path() == "/"
}

fn scope_names(scopes: &[Scope]) -> Vec<String> {
    scopes
        .iter()
        .map(|scope| scope.as_storage_name().to_owned())
        .collect()
}

fn parse_scopes(values: &[String]) -> Result<Vec<Scope>, CredentialRepositoryError> {
    let scopes: Option<Vec<_>> = values
        .iter()
        .map(|value| Scope::from_storage_name(value))
        .collect();
    let scopes = scopes.ok_or(CredentialRepositoryError::Internal)?;
    if valid_scopes(&scopes) {
        Ok(scopes)
    } else {
        Err(CredentialRepositoryError::Internal)
    }
}

fn parse_client_kind(value: &str) -> Result<DeviceClientKind, CredentialRepositoryError> {
    DeviceClientKind::from_storage_name(value).ok_or(CredentialRepositoryError::Internal)
}

fn device_session_from_row(row: &PgRow) -> Result<DeviceSession, CredentialRepositoryError> {
    let revision: i64 = row.try_get("revision").map_err(storage_error)?;
    let session = DeviceSession {
        id: row.try_get("id").map_err(storage_error)?,
        workspace_id: row.try_get("workspace_id").map_err(storage_error)?,
        user_id: row.try_get("user_id").map_err(storage_error)?,
        client_instance_id: row.try_get("client_instance_id").map_err(storage_error)?,
        client_kind: parse_client_kind(
            row.try_get::<String, _>("client_kind")
                .map_err(storage_error)?
                .as_str(),
        )?,
        device_label: row.try_get("device_label").map_err(storage_error)?,
        scopes: parse_scopes(
            &row.try_get::<Vec<String>, _>("scopes")
                .map_err(storage_error)?,
        )?,
        client_contract_version: parse_contract_version(row)?,
        client_version: row.try_get("client_version").map_err(storage_error)?,
        client_capabilities: row.try_get("client_capabilities").map_err(storage_error)?,
        created_at: row.try_get("created_at").map_err(storage_error)?,
        last_seen_at: row.try_get("last_seen_at").map_err(storage_error)?,
        credential_issued_at: row.try_get("credential_issued_at").map_err(storage_error)?,
        access_expires_at: row.try_get("expires_at").map_err(storage_error)?,
        refresh_idle_expires_at: row
            .try_get("refresh_idle_expires_at")
            .map_err(storage_error)?,
        absolute_expires_at: row.try_get("absolute_expires_at").map_err(storage_error)?,
        revision: revision
            .try_into()
            .map_err(|_| CredentialRepositoryError::Internal)?,
    };
    if !valid_label(&session.device_label, 200)
        || !valid_scopes(&session.scopes)
        || !session.scopes.iter().all(|scope| scope.is_rest())
        || !valid_device_stored_metadata(
            session.client_contract_version,
            &session.scopes,
            &session.client_version,
            &session.client_capabilities,
        )
        || !valid_device_session_timestamps(&session)
    {
        return Err(CredentialRepositoryError::Internal);
    }
    Ok(session)
}

fn account_recovery_code_from_row(
    row: &PgRow,
) -> Result<AccountRecoveryCode, CredentialRepositoryError> {
    let id: Uuid = row.try_get("id").map_err(storage_error)?;
    let revision: i64 = row.try_get("revision").map_err(storage_error)?;
    if id.is_nil() || revision <= 0 {
        return Err(CredentialRepositoryError::Internal);
    }
    Ok(AccountRecoveryCode {
        id,
        created_at: row.try_get("created_at").map_err(storage_error)?,
        revision: revision
            .try_into()
            .map_err(|_| CredentialRepositoryError::Internal)?,
    })
}

fn valid_device_session_timestamps(session: &DeviceSession) -> bool {
    let Some(maximum_access_expiry) = session
        .credential_issued_at
        .checked_add_signed(ACCESS_TOKEN_TTL)
    else {
        return false;
    };
    let Some(maximum_idle_expiry) = session
        .credential_issued_at
        .checked_add_signed(DEVICE_SESSION_REFRESH_IDLE_TTL)
    else {
        return false;
    };
    let Some(maximum_absolute_expiry) = session
        .created_at
        .checked_add_signed(DEVICE_SESSION_ABSOLUTE_TTL)
    else {
        return false;
    };

    session.created_at <= session.credential_issued_at
        && session.credential_issued_at <= session.last_seen_at
        && session.credential_issued_at < session.access_expires_at
        && session.access_expires_at <= maximum_access_expiry
        && session.access_expires_at <= session.absolute_expires_at
        && session.credential_issued_at < session.refresh_idle_expires_at
        && session.refresh_idle_expires_at <= maximum_idle_expiry
        && session.refresh_idle_expires_at <= session.absolute_expires_at
        && session.credential_issued_at < session.absolute_expires_at
        && session.absolute_expires_at <= maximum_absolute_expiry
}

fn mcp_client_from_row(row: &PgRow) -> Result<McpClient, CredentialRepositoryError> {
    let revision: i64 = row.try_get("revision").map_err(storage_error)?;
    let client = McpClient {
        id: row.try_get("id").map_err(storage_error)?,
        workspace_id: row.try_get("workspace_id").map_err(storage_error)?,
        user_id: row.try_get("created_by_user_id").map_err(storage_error)?,
        client_identifier: row.try_get("client_identifier").map_err(storage_error)?,
        display_name: row.try_get("display_name").map_err(storage_error)?,
        scopes: parse_scopes(
            &row.try_get::<Vec<String>, _>("scopes")
                .map_err(storage_error)?,
        )?,
        allowed_origins: row.try_get("allowed_origins").map_err(storage_error)?,
        client_contract_version: parse_contract_version(row)?,
        client_version: row.try_get("client_version").map_err(storage_error)?,
        client_capabilities: row.try_get("client_capabilities").map_err(storage_error)?,
        created_at: row.try_get("created_at").map_err(storage_error)?,
        last_seen_at: row.try_get("last_seen_at").map_err(storage_error)?,
        expires_at: row.try_get("expires_at").map_err(storage_error)?,
        revision: revision
            .try_into()
            .map_err(|_| CredentialRepositoryError::Internal)?,
    };
    let shape = McpClientSpec {
        id: client.id,
        client_identifier: client.client_identifier.clone(),
        display_name: client.display_name.clone(),
        scopes: client.scopes.clone(),
        allowed_origins: client.allowed_origins.clone(),
        client_contract_version: client.client_contract_version,
        client_version: client.client_version.clone(),
        client_capabilities: client.client_capabilities.clone(),
        created_at: client.created_at,
        requested_expires_at: Some(client.expires_at),
    };
    validate_mcp_client(&shape).map_err(|_| CredentialRepositoryError::Internal)?;
    Ok(client)
}

fn parse_contract_version(row: &PgRow) -> Result<u16, CredentialRepositoryError> {
    let value: i16 = row
        .try_get("client_contract_version")
        .map_err(storage_error)?;
    value
        .try_into()
        .map_err(|_| CredentialRepositoryError::Internal)
}

fn write_error(error: sqlx::Error) -> CredentialRepositoryError {
    let result = if error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
    {
        CredentialRepositoryError::Conflict
    } else {
        CredentialRepositoryError::Internal
    };
    drop(error);
    result
}

fn storage_error<T>(_error: T) -> CredentialRepositoryError {
    CredentialRepositoryError::Internal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_mcp_origins_and_scopes_conservatively() {
        assert!(valid_origin("https://chat.example.test"));
        assert!(valid_origin("http://127.0.0.1:8787"));
        assert!(!valid_origin("http://chat.example.test"));
        assert!(!valid_origin("https://*.example.test"));
        assert!(!valid_origin("https://example.test/path"));
        assert!(!valid_origin("https://user@example.test"));

        assert!(valid_scopes(&[Scope::ScheduleRead]));
        assert!(!valid_scopes(&[]));
        assert!(!valid_scopes(&[Scope::ScheduleRead, Scope::ScheduleRead]));
    }
}
