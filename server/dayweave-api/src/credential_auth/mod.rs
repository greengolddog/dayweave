//! Durable, revocable device-session and MCP authentication.

mod domain;
pub(crate) mod http;
mod repository;
mod token;

pub use domain::{
    ACCESS_TOKEN_TTL, AccountRecoveryCode, AccountRecoveryCodeSpec, AccountRecoveryConsumption,
    AccountRecoverySessionSpec, CredentialMutation, DEVICE_CLIENT_CONTRACT_VERSION,
    DEVICE_SESSION_ABSOLUTE_TTL, DEVICE_SESSION_REFRESH_IDLE_TTL, DeviceClientKind,
    DeviceEnrollmentCreation, DeviceEnrollmentSpec, DeviceSession, ENROLLMENT_TOKEN_TTL,
    MAX_ACTIVE_DEVICE_SESSIONS, MAX_MCP_CREDENTIAL_TTL, MAX_PENDING_DEVICE_ENROLLMENTS,
    MCP_CLIENT_CONTRACT_VERSION, MCP_CREDENTIAL_DEFAULT_TTL, McpClient, McpClientSpec,
    full_owner_device_scopes,
};
pub use repository::{CredentialRepository, CredentialRepositoryError};
pub use token::{
    CredentialKind, GeneratedCredential, OpaqueCredential, TokenGenerationError, TokenParseError,
};
