use std::{fmt::Write, sync::Arc};

use async_trait::async_trait;
use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, Method, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::{Choice, ConstantTimeEq};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    credential_auth::{CredentialKind, CredentialRepository, OpaqueCredential},
    error::ApiError,
    proposals::Clock,
};

pub type TokenHash = [u8; 32];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    SuggestionsRead,
    SuggestionsWrite,
    ScheduleRead,
    ScheduleSimulate,
    SchedulePublish,
    SuggestionsSubmit,
    ItemsRead,
    ItemsWrite,
    ExecutionRead,
    ExecutionWrite,
    GoogleRead,
    GoogleWrite,
    AuthSessionsRead,
    AuthSessionsWrite,
    AuthMcpClientsRead,
    AuthMcpClientsWrite,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalAudience {
    Legacy,
    Device,
    Mcp,
    McpOAuth,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Principal {
    pub subject: String,
    pub scopes: Vec<Scope>,
    pub audience: PrincipalAudience,
    pub workspace_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub credential_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
}

impl Principal {
    #[must_use]
    pub fn has_scope(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    #[must_use]
    pub fn legacy(subject: String) -> Self {
        Self {
            subject,
            scopes: Scope::ALL.to_vec(),
            audience: PrincipalAudience::Legacy,
            workspace_id: None,
            user_id: None,
            credential_id: None,
            allowed_origins: Vec::new(),
        }
    }
}

impl Scope {
    pub const ALL: [Self; 16] = [
        Self::SuggestionsRead,
        Self::SuggestionsWrite,
        Self::ScheduleRead,
        Self::ScheduleSimulate,
        Self::SchedulePublish,
        Self::SuggestionsSubmit,
        Self::ItemsRead,
        Self::ItemsWrite,
        Self::ExecutionRead,
        Self::ExecutionWrite,
        Self::GoogleRead,
        Self::GoogleWrite,
        Self::AuthSessionsRead,
        Self::AuthSessionsWrite,
        Self::AuthMcpClientsRead,
        Self::AuthMcpClientsWrite,
    ];

    #[must_use]
    pub const fn is_mcp(self) -> bool {
        matches!(
            self,
            Self::ScheduleRead | Self::ScheduleSimulate | Self::SuggestionsSubmit
        )
    }

    #[must_use]
    pub const fn is_rest(self) -> bool {
        !matches!(self, Self::SuggestionsSubmit)
    }

    #[must_use]
    pub const fn as_storage_name(self) -> &'static str {
        match self {
            Self::SuggestionsRead => "suggestions_read",
            Self::SuggestionsWrite => "suggestions_write",
            Self::ScheduleRead => "schedule_read",
            Self::ScheduleSimulate => "schedule_simulate",
            Self::SchedulePublish => "schedule_publish",
            Self::SuggestionsSubmit => "suggestions_submit",
            Self::ItemsRead => "items_read",
            Self::ItemsWrite => "items_write",
            Self::ExecutionRead => "execution_read",
            Self::ExecutionWrite => "execution_write",
            Self::GoogleRead => "google_read",
            Self::GoogleWrite => "google_write",
            Self::AuthSessionsRead => "auth_sessions_read",
            Self::AuthSessionsWrite => "auth_sessions_write",
            Self::AuthMcpClientsRead => "auth_mcp_clients_read",
            Self::AuthMcpClientsWrite => "auth_mcp_clients_write",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "suggestions_read" => Some(Self::SuggestionsRead),
            "suggestions_write" => Some(Self::SuggestionsWrite),
            "schedule_read" => Some(Self::ScheduleRead),
            "schedule_simulate" => Some(Self::ScheduleSimulate),
            "schedule_publish" => Some(Self::SchedulePublish),
            "suggestions_submit" => Some(Self::SuggestionsSubmit),
            "items_read" => Some(Self::ItemsRead),
            "items_write" => Some(Self::ItemsWrite),
            "execution_read" => Some(Self::ExecutionRead),
            "execution_write" => Some(Self::ExecutionWrite),
            "google_read" => Some(Self::GoogleRead),
            "google_write" => Some(Self::GoogleWrite),
            "auth_sessions_read" => Some(Self::AuthSessionsRead),
            "auth_sessions_write" => Some(Self::AuthSessionsWrite),
            "auth_mcp_clients_read" => Some(Self::AuthMcpClientsRead),
            "auth_mcp_clients_write" => Some(Self::AuthMcpClientsWrite),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AuthenticationError {
    #[error("invalid credentials")]
    InvalidCredentials,
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError>;
}

#[derive(Clone, Debug)]
pub struct StaticTokenAuthenticator {
    token_hashes: Arc<Vec<TokenHash>>,
}

impl StaticTokenAuthenticator {
    #[must_use]
    pub fn from_hashes(token_hashes: Arc<Vec<TokenHash>>) -> Self {
        Self { token_hashes }
    }

    #[must_use]
    pub fn from_plaintext(tokens: &[&str]) -> Self {
        Self {
            token_hashes: Arc::new(tokens.iter().map(|token| hash_token(token)).collect()),
        }
    }
}

#[async_trait]
impl Authenticator for StaticTokenAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        if token.starts_with("dw_") {
            return Err(AuthenticationError::InvalidCredentials);
        }
        let candidate = hash_token(token);
        let mut matched = Choice::from(0);
        for expected in self.token_hashes.iter() {
            matched |= candidate.ct_eq(expected);
        }
        if !bool::from(matched) {
            return Err(AuthenticationError::InvalidCredentials);
        }

        Ok(Principal::legacy(token_fingerprint(&candidate)))
    }
}

/// Dispatches versioned durable credentials without permitting downgrade to a
/// static token when a reserved `dw_` credential is malformed, expired, or
/// revoked. Static credentials are optional so the same type serves hybrid and
/// credential-only rollout modes.
pub struct RuntimeAuthenticator {
    static_authenticator: Option<StaticTokenAuthenticator>,
    credentials: Arc<dyn CredentialRepository>,
    clock: Arc<dyn Clock>,
}

impl RuntimeAuthenticator {
    #[must_use]
    pub fn new(
        static_token_hashes: Option<Arc<Vec<TokenHash>>>,
        credentials: Arc<dyn CredentialRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            static_authenticator: static_token_hashes.map(StaticTokenAuthenticator::from_hashes),
            credentials,
            clock,
        }
    }
}

#[async_trait]
impl Authenticator for RuntimeAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        if token.starts_with(CredentialKind::DeviceAccess.prefix()) {
            let credential = OpaqueCredential::parse(CredentialKind::DeviceAccess, token)
                .map_err(|_| AuthenticationError::InvalidCredentials)?;
            let session = self
                .credentials
                .authenticate_device_access(&credential, self.clock.now())
                .await
                .map_err(|_| AuthenticationError::InvalidCredentials)?;
            if session.scopes.iter().any(|scope| !scope.is_rest()) {
                return Err(AuthenticationError::InvalidCredentials);
            }
            return Ok(Principal {
                subject: format!("device-session:{}", session.id),
                scopes: session.scopes,
                audience: PrincipalAudience::Device,
                workspace_id: Some(session.workspace_id),
                user_id: Some(session.user_id),
                credential_id: Some(session.id),
                allowed_origins: Vec::new(),
            });
        }
        if token.starts_with(CredentialKind::McpClient.prefix()) {
            let credential = OpaqueCredential::parse(CredentialKind::McpClient, token)
                .map_err(|_| AuthenticationError::InvalidCredentials)?;
            let client = self
                .credentials
                .authenticate_mcp_client(&credential, self.clock.now())
                .await
                .map_err(|_| AuthenticationError::InvalidCredentials)?;
            if client.scopes.iter().any(|scope| !scope.is_mcp()) {
                return Err(AuthenticationError::InvalidCredentials);
            }
            return Ok(Principal {
                subject: format!("mcp-client:{}", client.id),
                scopes: client.scopes,
                audience: PrincipalAudience::Mcp,
                workspace_id: Some(client.workspace_id),
                user_id: Some(client.user_id),
                credential_id: Some(client.id),
                allowed_origins: client.allowed_origins,
            });
        }
        if token.starts_with("dw_") {
            return Err(AuthenticationError::InvalidCredentials);
        }
        let static_authenticator = self
            .static_authenticator
            .as_ref()
            .ok_or(AuthenticationError::InvalidCredentials)?;
        static_authenticator.authenticate(token).await
    }
}

#[must_use]
pub fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

fn token_fingerprint(hash: &TokenHash) -> String {
    let prefix = hash[..6]
        .iter()
        .fold(String::with_capacity(12), |mut fingerprint, byte| {
            let _ = write!(fingerprint, "{byte:02x}");
            fingerprint
        });
    format!("token:{prefix}")
}

/// Authenticates a protected request and attaches its [`Principal`].
///
/// # Errors
///
/// Returns an unauthorized API error when the authorization header is missing,
/// malformed, or rejected by the configured authenticator.
pub async fn require_authentication(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = bearer_token_from_headers(request.headers()).ok_or_else(ApiError::unauthorized)?;

    let principal = state
        .authenticator
        .authenticate(token)
        .await
        .map_err(|_| ApiError::unauthorized())?;
    if !matches!(
        principal.audience,
        PrincipalAudience::Legacy | PrincipalAudience::Device
    ) {
        return Err(ApiError::unauthorized());
    }
    let required_scope = required_rest_scope(
        request.method(),
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
    )
    .ok_or_else(ApiError::forbidden)?;
    if !principal.has_scope(required_scope) {
        return Err(ApiError::forbidden());
    }
    request.extensions_mut().insert(principal);

    Ok(next.run(request).await)
}

fn required_rest_scope(method: &Method, matched_path: Option<&str>) -> Option<Scope> {
    let matched_path = matched_path?;
    let path = matched_path.strip_prefix("/v1").unwrap_or(matched_path);
    match (method, path) {
        (
            &Method::GET,
            "/suggestions"
            | "/suggestions/{id}"
            | "/suggestions/{id}/application"
            | "/suggestions/applications/{id}",
        ) => Some(Scope::SuggestionsRead),
        (
            &Method::POST | &Method::PATCH | &Method::DELETE,
            "/suggestions"
            | "/suggestions/{id}"
            | "/suggestions/{id}/accept"
            | "/suggestions/{id}/reject"
            | "/suggestions/application-previews"
            | "/suggestions/application-previews/{id}/apply"
            | "/suggestions/applications/{id}/undo",
        ) => Some(Scope::SuggestionsWrite),
        (&Method::GET, "/items" | "/items/delta" | "/items/{id}") => Some(Scope::ItemsRead),
        (
            &Method::POST | &Method::PUT | &Method::DELETE,
            "/items" | "/items/{id}" | "/items/{id}/restore",
        ) => Some(Scope::ItemsWrite),
        (&Method::GET, "/schedule/manual-placements") => Some(Scope::ScheduleRead),
        (&Method::POST, "/schedule/preview") => Some(Scope::ScheduleSimulate),
        (&Method::POST, "/schedule/publish") => Some(Scope::SchedulePublish),
        (&Method::GET, "/execution" | "/execution/history") => Some(Scope::ExecutionRead),
        (&Method::POST, "/execution/commands" | "/execution/defer-assessments") => {
            Some(Scope::ExecutionWrite)
        }
        (
            &Method::GET,
            "/integrations/google/accounts"
            | "/integrations/google/accounts/{account_id}/collections"
            | "/integrations/google/accounts/{account_id}/sync",
        ) => Some(Scope::GoogleRead),
        (
            &Method::POST | &Method::PUT | &Method::DELETE,
            "/integrations/google/oauth/start"
            | "/integrations/google/accounts/{id}/pause"
            | "/integrations/google/accounts/{id}/resume"
            | "/integrations/google/accounts/{id}"
            | "/integrations/google/oauth/recovery/acknowledge"
            | "/integrations/google/accounts/{account_id}/collections/discover"
            | "/integrations/google/accounts/{account_id}/collections/{collection_id}"
            | "/integrations/google/accounts/{account_id}/sync/refresh"
            | "/integrations/google/accounts/{account_id}/outbound/previews"
            | "/integrations/google/accounts/{account_id}/outbound/previews/{preview_id}/approve"
            | "/integrations/google/accounts/{account_id}/outbound",
        ) => Some(Scope::GoogleWrite),
        (&Method::GET, "/auth/sessions") => Some(Scope::AuthSessionsRead),
        (
            &Method::POST | &Method::DELETE,
            "/auth/device-enrollments" | "/auth/device-enrollments/{id}" | "/auth/sessions/{id}",
        ) => Some(Scope::AuthSessionsWrite),
        (&Method::GET, "/auth/mcp-clients") => Some(Scope::AuthMcpClientsRead),
        (&Method::POST | &Method::DELETE, "/auth/mcp-clients" | "/auth/mcp-clients/{id}") => {
            Some(Scope::AuthMcpClientsWrite)
        }
        _ => None,
    }
}

fn parse_bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() || token.contains(' ') {
        return None;
    }
    Some(token)
}

#[must_use]
pub fn bearer_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_auth::{
        CredentialMutation, CredentialRepositoryError, DeviceClientKind, DeviceEnrollmentSpec,
        DeviceSession, McpClient, McpClientSpec,
    };
    use chrono::{DateTime, Duration, Utc};

    #[derive(Clone)]
    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    struct SyntheticCredentialRepository {
        session: DeviceSession,
        mcp: McpClient,
    }

    #[async_trait]
    impl CredentialRepository for SyntheticCredentialRepository {
        async fn create_device_enrollment(
            &self,
            _spec: DeviceEnrollmentSpec,
            _enrollment_token: &OpaqueCredential<'_>,
        ) -> Result<(), CredentialRepositoryError> {
            Err(CredentialRepositoryError::InvalidCredential)
        }

        async fn consume_device_enrollment(
            &self,
            _enrollment_token: &OpaqueCredential<'_>,
            _session_id: Uuid,
            _access_token: &OpaqueCredential<'_>,
            _refresh_token: &OpaqueCredential<'_>,
            _now: DateTime<Utc>,
        ) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError> {
            Err(CredentialRepositoryError::InvalidCredential)
        }

        async fn revoke_device_enrollment(
            &self,
            _enrollment_id: Uuid,
            _now: DateTime<Utc>,
        ) -> Result<bool, CredentialRepositoryError> {
            Ok(false)
        }

        async fn authenticate_device_access(
            &self,
            _access_token: &OpaqueCredential<'_>,
            _now: DateTime<Utc>,
        ) -> Result<DeviceSession, CredentialRepositoryError> {
            Ok(self.session.clone())
        }

        async fn refresh_device_session(
            &self,
            _refresh_token: &OpaqueCredential<'_>,
            _next_access_token: &OpaqueCredential<'_>,
            _next_refresh_token: &OpaqueCredential<'_>,
            _now: DateTime<Utc>,
        ) -> Result<CredentialMutation<DeviceSession>, CredentialRepositoryError> {
            Err(CredentialRepositoryError::InvalidCredential)
        }

        async fn list_device_sessions(
            &self,
            _now: DateTime<Utc>,
        ) -> Result<Vec<DeviceSession>, CredentialRepositoryError> {
            Ok(vec![self.session.clone()])
        }

        async fn revoke_device_session(
            &self,
            _session_id: Uuid,
            _now: DateTime<Utc>,
        ) -> Result<bool, CredentialRepositoryError> {
            Ok(false)
        }

        async fn register_mcp_client(
            &self,
            _spec: McpClientSpec,
            _credential: &OpaqueCredential<'_>,
        ) -> Result<McpClient, CredentialRepositoryError> {
            Err(CredentialRepositoryError::InvalidCredential)
        }

        async fn authenticate_mcp_client(
            &self,
            _credential: &OpaqueCredential<'_>,
            _now: DateTime<Utc>,
        ) -> Result<McpClient, CredentialRepositoryError> {
            Ok(self.mcp.clone())
        }

        async fn list_mcp_clients(
            &self,
            _now: DateTime<Utc>,
        ) -> Result<Vec<McpClient>, CredentialRepositoryError> {
            Ok(vec![self.mcp.clone()])
        }

        async fn revoke_mcp_client(
            &self,
            _client_id: Uuid,
            _now: DateTime<Utc>,
        ) -> Result<bool, CredentialRepositoryError> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn authenticates_known_token_without_storing_plaintext() {
        let authenticator = StaticTokenAuthenticator::from_plaintext(&["known-secret"]);

        let principal = authenticator
            .authenticate("known-secret")
            .await
            .expect("known token");

        assert!(principal.subject.starts_with("token:"));
        assert_eq!(principal.scopes, Scope::ALL);
        assert!(principal.has_scope(Scope::ScheduleRead));
        assert!(principal.has_scope(Scope::ScheduleSimulate));
        assert!(principal.has_scope(Scope::SuggestionsSubmit));
        assert!(authenticator.authenticate("wrong-secret").await.is_err());

        let reserved = StaticTokenAuthenticator::from_plaintext(&[
            "dw_reserved-static-token-that-must-never-authenticate",
        ]);
        assert!(
            reserved
                .authenticate("dw_reserved-static-token-that-must-never-authenticate")
                .await
                .is_err()
        );
    }

    #[test]
    fn bearer_parser_is_strict_but_case_insensitive() {
        assert_eq!(parse_bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(parse_bearer_token("bearer abc"), Some("abc"));
        assert_eq!(parse_bearer_token("Basic abc"), None);
        assert_eq!(parse_bearer_token("Bearer"), None);
        assert_eq!(parse_bearer_token("Bearer two tokens"), None);
    }

    #[test]
    fn rest_scope_matrix_is_explicit_and_fail_closed() {
        assert_eq!(
            required_rest_scope(&Method::GET, Some("/v1/items/{id}")),
            Some(Scope::ItemsRead)
        );
        assert_eq!(
            required_rest_scope(&Method::PUT, Some("/v1/items/{id}")),
            Some(Scope::ItemsWrite)
        );
        assert_eq!(
            required_rest_scope(
                &Method::POST,
                Some("/v1/integrations/google/accounts/{account_id}/sync/refresh")
            ),
            Some(Scope::GoogleWrite)
        );
        assert_eq!(
            required_rest_scope(&Method::GET, Some("/v1/auth/mcp-clients")),
            Some(Scope::AuthMcpClientsRead)
        );
        assert_eq!(
            required_rest_scope(&Method::GET, Some("/v1/schedule/manual-placements")),
            Some(Scope::ScheduleRead)
        );
        assert_eq!(
            required_rest_scope(&Method::POST, Some("/v1/unclassified")),
            None
        );
        assert_eq!(required_rest_scope(&Method::GET, None), None);
    }

    #[tokio::test]
    async fn runtime_separates_audiences_and_never_downgrades_reserved_tokens() {
        let now = Utc::now();
        let workspace_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let mcp_id = Uuid::new_v4();
        let repository = Arc::new(SyntheticCredentialRepository {
            session: DeviceSession {
                id: session_id,
                workspace_id,
                user_id,
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Synthetic Mac".to_owned(),
                scopes: vec![Scope::ItemsRead],
                client_contract_version: 1,
                client_version: "test-1".to_owned(),
                client_capabilities: Vec::new(),
                created_at: now,
                last_seen_at: now,
                credential_issued_at: now,
                access_expires_at: now + Duration::minutes(15),
                refresh_idle_expires_at: now + Duration::days(30),
                absolute_expires_at: now + Duration::days(180),
                revision: 1,
            },
            mcp: McpClient {
                id: mcp_id,
                workspace_id,
                user_id,
                client_identifier: "synthetic-mcp".to_owned(),
                display_name: "Synthetic MCP".to_owned(),
                scopes: vec![Scope::ScheduleRead],
                allowed_origins: vec!["https://assistant.example.test".to_owned()],
                client_contract_version: 1,
                client_version: "test-1".to_owned(),
                client_capabilities: Vec::new(),
                created_at: now,
                last_seen_at: None,
                expires_at: now + Duration::days(90),
                revision: 1,
            },
        });
        let malformed_reserved = "dw_da1_this-is-also-configured-as-a-static-token";
        let static_hashes = Arc::new(vec![
            hash_token("ordinary-static-token"),
            hash_token(malformed_reserved),
        ]);
        let authenticator =
            RuntimeAuthenticator::new(Some(static_hashes), repository, Arc::new(FixedClock(now)));

        let legacy = authenticator
            .authenticate("ordinary-static-token")
            .await
            .expect("hybrid legacy token");
        assert_eq!(legacy.audience, PrincipalAudience::Legacy);
        assert!(
            authenticator
                .authenticate(malformed_reserved)
                .await
                .is_err()
        );
        assert!(
            authenticator
                .authenticate("dw_future_opaque")
                .await
                .is_err()
        );

        let access = format!(
            "{}{}",
            CredentialKind::DeviceAccess.prefix(),
            "A".repeat(43)
        );
        let device = authenticator
            .authenticate(&access)
            .await
            .expect("durable device access");
        assert_eq!(device.audience, PrincipalAudience::Device);
        assert_eq!(device.subject, format!("device-session:{session_id}"));
        assert_eq!(device.credential_id, Some(session_id));

        let mcp_raw = format!("{}{}", CredentialKind::McpClient.prefix(), "A".repeat(43));
        let mcp = authenticator
            .authenticate(&mcp_raw)
            .await
            .expect("durable MCP access");
        assert_eq!(mcp.audience, PrincipalAudience::Mcp);
        assert_eq!(mcp.subject, format!("mcp-client:{mcp_id}"));
        assert_eq!(
            mcp.allowed_origins,
            vec!["https://assistant.example.test".to_owned()]
        );
    }
}
