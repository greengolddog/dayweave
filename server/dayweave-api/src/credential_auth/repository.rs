use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::{
    CredentialMutation, DeviceEnrollmentSpec, DeviceSession, McpClient, McpClientSpec,
    OpaqueCredential,
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
    async fn create_device_enrollment(
        &self,
        spec: DeviceEnrollmentSpec,
        enrollment_token: &OpaqueCredential<'_>,
    ) -> Result<(), CredentialRepositoryError>;

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
