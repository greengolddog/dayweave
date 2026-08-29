//! Durable, revocable device-session and MCP authentication.

mod domain;
pub(crate) mod http;
mod repository;
mod token;

pub use domain::{
    ACCESS_TOKEN_TTL, AUTH_CLIENT_CONTRACT_VERSION, CredentialMutation,
    DEVICE_SESSION_ABSOLUTE_TTL, DEVICE_SESSION_REFRESH_IDLE_TTL, DeviceClientKind,
    DeviceEnrollmentSpec, DeviceSession, ENROLLMENT_TOKEN_TTL, MAX_MCP_CREDENTIAL_TTL,
    MCP_CREDENTIAL_DEFAULT_TTL, McpClient, McpClientSpec,
};
pub use repository::{CredentialRepository, CredentialRepositoryError};
pub use token::{
    CredentialKind, GeneratedCredential, OpaqueCredential, TokenGenerationError, TokenParseError,
};
