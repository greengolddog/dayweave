use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::Scope;

pub const ACCESS_TOKEN_TTL: Duration = Duration::minutes(15);
pub const DEVICE_SESSION_REFRESH_IDLE_TTL: Duration = Duration::days(30);
pub const DEVICE_SESSION_ABSOLUTE_TTL: Duration = Duration::days(180);
pub const ENROLLMENT_TOKEN_TTL: Duration = Duration::minutes(10);
pub const MCP_CREDENTIAL_DEFAULT_TTL: Duration = Duration::days(90);
pub const MAX_MCP_CREDENTIAL_TTL: Duration = Duration::days(365);
pub const AUTH_CLIENT_CONTRACT_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialMutation<T> {
    pub value: T,
    pub replayed: bool,
}

impl<T> std::ops::Deref for CredentialMutation<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClientKind {
    Macos,
    Android,
}

impl DeviceClientKind {
    #[must_use]
    pub const fn as_storage_name(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Android => "android",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "macos" => Some(Self::Macos),
            "android" => Some(Self::Android),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceEnrollmentSpec {
    pub id: Uuid,
    pub client_instance_id: Uuid,
    pub client_kind: DeviceClientKind,
    pub device_label: String,
    pub scopes: Vec<Scope>,
    pub client_contract_version: u16,
    pub client_version: String,
    pub client_capabilities: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSession {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub client_instance_id: Uuid,
    pub client_kind: DeviceClientKind,
    pub device_label: String,
    pub scopes: Vec<Scope>,
    pub client_contract_version: u16,
    pub client_version: String,
    pub client_capabilities: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub credential_issued_at: DateTime<Utc>,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_idle_expires_at: DateTime<Utc>,
    pub absolute_expires_at: DateTime<Utc>,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpClientSpec {
    pub id: Uuid,
    pub client_identifier: String,
    pub display_name: String,
    pub scopes: Vec<Scope>,
    pub allowed_origins: Vec<String>,
    pub client_contract_version: u16,
    pub client_version: String,
    pub client_capabilities: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// `None` selects the 90-day default. Explicit values cannot exceed 365 days.
    pub requested_expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpClient {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub client_identifier: String,
    pub display_name: String,
    pub scopes: Vec<Scope>,
    pub allowed_origins: Vec<String>,
    pub client_contract_version: u16,
    pub client_version: String,
    pub client_capabilities: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revision: u64,
}
