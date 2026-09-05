use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::{
    AccountRecoveryCode, AccountRecoveryCodeSpec, AccountRecoveryConsumption,
    AccountRecoverySessionSpec, CredentialMutation, DeviceEnrollmentCreation, DeviceEnrollmentSpec,
    DeviceSession, ENROLLMENT_TOKEN_TTL, McpClient, McpClientSpec, OpaqueCredential,
};

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CredentialRepositoryError {
    #[error("invalid credential")]
    InvalidCredential,
    #[error("credential input is invalid")]
    InvalidInput,
    #[error("credential state conflicts with an existing record")]
    Conflict,
    #[error("credential repository operation failed")]
    Internal,
}

#[async_trait]
pub trait CredentialRepository: Send + Sync {
    async fn get_active_account_recovery_code(
        &self,
    ) -> Result<Option<AccountRecoveryCode>, CredentialRepositoryError>;

    /// Creates the first client-journaled recovery credential, rotates the
    /// exact currently active credential, or replays only the exact active
    /// issuance after a lost response.
    async fn create_or_rotate_account_recovery_code(
        &self,
        spec: AccountRecoveryCodeSpec,
        recovery_code: &OpaqueCredential<'_>,
        authorizing_session_id: Uuid,
    ) -> Result<CredentialMutation<AccountRecoveryCode>, CredentialRepositoryError>;

    /// Atomically consumes one recovery credential, replaces every local
    /// credential authority, and installs both a successor code and a full
    /// owner device session. Only the exact committed tuple is replayable.
    #[allow(clippy::too_many_arguments)]
    async fn consume_account_recovery_code(
        &self,
        recovery_code: &OpaqueCredential<'_>,
        spec: AccountRecoverySessionSpec,
        access_token: &OpaqueCredential<'_>,
        refresh_token: &OpaqueCredential<'_>,
        successor_recovery_code: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<CredentialMutation<AccountRecoveryConsumption>, CredentialRepositoryError>;

    async fn create_device_enrollment(
        &self,
        spec: DeviceEnrollmentSpec,
        enrollment_token: &OpaqueCredential<'_>,
    ) -> Result<(), CredentialRepositoryError>;

    /// Creates a client-journaled enrollment or recovers only the exact same
    /// still-pending issuance after a lost response.
    ///
    /// The default preserves compatibility for repository test doubles. The
    /// durable `PostgreSQL` adapter overrides it with exact tuple comparison.
    async fn create_or_replay_device_enrollment(
        &self,
        spec: DeviceEnrollmentSpec,
        enrollment_token: &OpaqueCredential<'_>,
        authorizing_session_id: Option<Uuid>,
    ) -> Result<CredentialMutation<DeviceEnrollmentCreation>, CredentialRepositoryError> {
        let _ = authorizing_session_id;
        let expires_at = spec
            .created_at
            .checked_add_signed(ENROLLMENT_TOKEN_TTL)
            .ok_or(CredentialRepositoryError::InvalidInput)?;
        self.create_device_enrollment(spec, enrollment_token)
            .await?;
        Ok(CredentialMutation {
            value: DeviceEnrollmentCreation { expires_at },
            replayed: false,
        })
    }

    async fn consume_device_enrollment(
        &self,
        enrollment_token: &OpaqueCredential<'_>,
        session_id: Uuid,
        access_token: &OpaqueCredential<'_>,
        refresh_token: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError>;

    async fn revoke_device_enrollment(
        &self,
        enrollment_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, CredentialRepositoryError>;

    async fn authenticate_device_access(
        &self,
        access_token: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<DeviceSession, CredentialRepositoryError>;

    async fn refresh_device_session(
        &self,
        refresh_token: &OpaqueCredential<'_>,
        next_access_token: &OpaqueCredential<'_>,
        next_refresh_token: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError>;

    async fn list_device_sessions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<DeviceSession>, CredentialRepositoryError>;

    async fn revoke_device_session(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, CredentialRepositoryError>;

    async fn register_mcp_client(
        &self,
        spec: McpClientSpec,
        credential: &OpaqueCredential<'_>,
        authorizing_session_id: Option<Uuid>,
    ) -> Result<McpClient, CredentialRepositoryError>;

    async fn authenticate_mcp_client(
        &self,
        credential: &OpaqueCredential<'_>,
        now: DateTime<Utc>,
    ) -> Result<McpClient, CredentialRepositoryError>;

    async fn list_mcp_clients(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<McpClient>, CredentialRepositoryError>;

    async fn revoke_mcp_client(
        &self,
        client_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<bool, CredentialRepositoryError>;
}
