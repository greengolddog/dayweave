use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row, postgres::PgRow};
use url::Url;
use uuid::Uuid;

use crate::{
    auth::Scope,
    credential_auth::{
        ACCESS_TOKEN_TTL, AUTH_CLIENT_CONTRACT_VERSION, CredentialKind, CredentialMutation,
        CredentialRepository, CredentialRepositoryError, DEVICE_SESSION_ABSOLUTE_TTL,
        DEVICE_SESSION_REFRESH_IDLE_TTL, DeviceClientKind, DeviceEnrollmentSpec, DeviceSession,
        ENROLLMENT_TOKEN_TTL, MAX_MCP_CREDENTIAL_TTL, MCP_CREDENTIAL_DEFAULT_TTL, McpClient,
        McpClientSpec, OpaqueCredential,
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

#[async_trait]
impl CredentialRepository for PostgresCredentialRepository {
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

        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
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
            || !valid_client_metadata(
                client_contract_version,
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
             ORDER BY last_seen_at DESC, id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
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

fn validate_device_enrollment(
    spec: &DeviceEnrollmentSpec,
) -> Result<(), CredentialRepositoryError> {
    if !valid_label(&spec.device_label, 200)
        || !valid_scopes(&spec.scopes)
        || !spec.scopes.iter().all(|scope| scope.is_rest())
        || !valid_client_metadata(
            spec.client_contract_version,
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

fn valid_client_metadata(contract_version: u16, version: &str, capabilities: &[String]) -> bool {
    contract_version == AUTH_CLIENT_CONTRACT_VERSION
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
        || !valid_client_metadata(
            session.client_contract_version,
            &session.client_version,
            &session.client_capabilities,
        )
        || !valid_device_session_timestamps(&session)
    {
        return Err(CredentialRepositoryError::Internal);
    }
    Ok(session)
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
