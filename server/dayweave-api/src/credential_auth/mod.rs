//! Durable credential primitives for the future device-session and MCP auth cutover.
//!
//! This module deliberately has no HTTP/runtime wiring yet. It defines strict
//! opaque-token parsing and the persistence contract so a later cutover can be
//! reviewed independently from credential issuance and client migration.

mod domain;
mod repository;
mod token;

pub use domain::{
    ACCESS_TOKEN_TTL, DEVICE_SESSION_ABSOLUTE_TTL, DEVICE_SESSION_REFRESH_IDLE_TTL,
    DeviceClientKind, DeviceEnrollmentSpec, DeviceSession, ENROLLMENT_TOKEN_TTL,
    MAX_MCP_CREDENTIAL_TTL, MCP_CREDENTIAL_DEFAULT_TTL, McpClient, McpClientSpec,
};
pub use repository::{CredentialRepository, CredentialRepositoryError};
pub use token::{CredentialKind, OpaqueCredential, TokenParseError};
