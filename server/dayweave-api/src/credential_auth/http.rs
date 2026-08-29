use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    AppState,
    auth::{Principal, Scope, bearer_token_from_headers},
    config::AuthMode,
    error::ApiError,
};

use super::{
    CredentialKind, CredentialRepository, CredentialRepositoryError,
    DEVICE_CLIENT_CONTRACT_VERSION, DeviceClientKind, DeviceEnrollmentSpec, DeviceSession,
    GeneratedCredential, MCP_CLIENT_CONTRACT_VERSION, McpClient, McpClientSpec, OpaqueCredential,
};

const AUTH_BODY_LIMIT: usize = 32 * 1024;

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/device-enrollments", post(create_device_enrollment))
        .route(
            "/auth/device-enrollments/{id}",
            delete(revoke_device_enrollment),
        )
        .route("/auth/sessions", get(list_sessions))
        .route("/auth/sessions/{id}", delete(revoke_session))
        .route(
            "/auth/mcp-clients",
            get(list_mcp_clients).post(create_mcp_client),
        )
        .route("/auth/mcp-clients/{id}", delete(revoke_mcp_client))
        .layer(middleware::map_response(add_no_store))
        .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT))
}

pub(crate) fn public_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/auth/device-enrollments/consume",
            post(consume_device_enrollment),
        )
        .route("/v1/auth/sessions/refresh", post(refresh_session))
        .layer(middleware::map_response(add_no_store))
        .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDeviceEnrollmentRequest {
    id: Uuid,
    enrollment_token: SecretInput,
    client_instance_id: Uuid,
    client_kind: DeviceClientKind,
    device_label: String,
    #[serde(default = "default_device_scopes")]
    scopes: Vec<Scope>,
    #[serde(default = "current_device_contract_version")]
    client_contract_version: u16,
    client_version: String,
    #[serde(default)]
    client_capabilities: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct DeviceEnrollmentResponse {
    id: Uuid,
    enrollment_token: String,
    expires_at: DateTime<Utc>,
    client_contract_version: u16,
    replayed: bool,
}

impl Drop for DeviceEnrollmentResponse {
    fn drop(&mut self) {
        self.enrollment_token.zeroize();
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(transparent)]
struct SecretInput(String);

impl Drop for SecretInput {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsumeDeviceEnrollmentRequest {
    session_id: Uuid,
    access_token: SecretInput,
    refresh_token: SecretInput,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshSessionRequest {
    next_access_token: SecretInput,
    next_refresh_token: SecretInput,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct DeviceSessionResponse {
    id: Uuid,
    client_instance_id: Uuid,
    client_kind: DeviceClientKind,
    device_label: String,
    scopes: Vec<Scope>,
    client_contract_version: u16,
    client_version: String,
    client_capabilities: Vec<String>,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    credential_issued_at: DateTime<Utc>,
    access_expires_at: DateTime<Utc>,
    refresh_idle_expires_at: DateTime<Utc>,
    absolute_expires_at: DateTime<Utc>,
    revision: u64,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct DeviceSessionMutationResponse {
    session: DeviceSessionResponse,
    replayed: bool,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct DeviceSessionListResponse {
    sessions: Vec<DeviceSessionResponse>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateMcpClientRequest {
    client_identifier: String,
    display_name: String,
    #[serde(default = "default_mcp_scopes")]
    scopes: Vec<Scope>,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default = "current_mcp_contract_version")]
    client_contract_version: u16,
    client_version: String,
    #[serde(default)]
    client_capabilities: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct McpClientResponse {
    id: Uuid,
    client_identifier: String,
    display_name: String,
    scopes: Vec<Scope>,
    allowed_origins: Vec<String>,
    client_contract_version: u16,
    client_version: String,
    client_capabilities: Vec<String>,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    revision: u64,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct McpClientIssuedResponse {
    client: McpClientResponse,
    credential: String,
}

impl Drop for McpClientIssuedResponse {
    fn drop(&mut self) {
        self.credential.zeroize();
    }
}

#[derive(Serialize, ToSchema)]
pub(crate) struct McpClientListResponse {
    clients: Vec<McpClientResponse>,
}

#[utoipa::path(
    post,
    path = "/v1/auth/device-enrollments",
    tag = "authentication",
    security(("bearer_token" = [])),
    request_body = CreateDeviceEnrollmentRequest,
    responses(
        (status = 201, description = "One-time enrollment credential issued", body = DeviceEnrollmentResponse),
        (status = 200, description = "Exact still-pending enrollment creation replayed", body = DeviceEnrollmentResponse),
        (status = 401, description = "Missing or invalid management credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Scope delegation is not allowed", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Enrollment conflicts with existing state", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid client or scope contract", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn create_device_enrollment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Result<Json<CreateDeviceEnrollmentRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let mut request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    validate_requested_scopes(&request.scopes, Scope::is_rest)?;
    if request
        .scopes
        .iter()
        .any(|scope| !principal.has_scope(*scope))
    {
        return Err(ApiError::forbidden());
    }
    let repository = active_repository(&state)?;
    let now = state.clock.now();
    let result = {
        let credential =
            OpaqueCredential::parse(CredentialKind::Enrollment, &request.enrollment_token.0)
                .map_err(|_| ApiError::validation("Invalid enrollment credential"))?;
        repository
            .create_or_replay_device_enrollment(
                DeviceEnrollmentSpec {
                    id: request.id,
                    client_instance_id: request.client_instance_id,
                    client_kind: request.client_kind,
                    device_label: request.device_label,
                    scopes: request.scopes,
                    client_contract_version: request.client_contract_version,
                    client_version: request.client_version,
                    client_capabilities: request.client_capabilities,
                    created_at: now,
                },
                &credential,
            )
            .await
            .map_err(map_repository_error)?
    };
    let enrollment_raw = std::mem::take(&mut request.enrollment_token.0);
    let status = if result.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(DeviceEnrollmentResponse {
            id: request.id,
            enrollment_token: enrollment_raw,
            expires_at: result.expires_at,
            client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
            replayed: result.replayed,
        }),
    )
        .into_response())
}

#[utoipa::path(
    post,
    path = "/v1/auth/device-enrollments/consume",
    tag = "authentication",
    security(("bearer_token" = [])),
    request_body = ConsumeDeviceEnrollmentRequest,
    responses(
        (status = 201, description = "Device session issued", body = DeviceSessionMutationResponse),
        (status = 200, description = "Exact committed issuance replayed", body = DeviceSessionMutationResponse),
        (status = 401, description = "Invalid, expired, consumed-with-another-tuple, or revoked enrollment", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Credential tuple is invalid", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn consume_device_enrollment(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<ConsumeDeviceEnrollmentRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let repository = active_repository(&state)?;
    let enrollment_raw = bearer_token_from_headers(&headers).ok_or_else(ApiError::unauthorized)?;
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, enrollment_raw)
        .map_err(|_| ApiError::unauthorized())?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &request.access_token.0)
        .map_err(|_| ApiError::unauthorized())?;
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &request.refresh_token.0)
        .map_err(|_| ApiError::unauthorized())?;
    let result = repository
        .consume_device_enrollment(
            &enrollment,
            request.session_id,
            &access,
            &refresh,
            state.clock.now(),
        )
        .await
        .map_err(map_repository_error)?;
    let status = if result.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(DeviceSessionMutationResponse {
            session: DeviceSessionResponse::from(&result.value),
            replayed: result.replayed,
        }),
    )
        .into_response())
}

#[utoipa::path(
    post,
    path = "/v1/auth/sessions/refresh",
    tag = "authentication",
    security(("bearer_token" = [])),
    request_body = RefreshSessionRequest,
    responses(
        (status = 200, description = "Credential pair rotated or exactly replayed", body = DeviceSessionMutationResponse),
        (status = 401, description = "Invalid, expired, revoked, replayed-with-another-tuple, or stale refresh credential", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Next credential tuple is invalid", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn refresh_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<RefreshSessionRequest>, JsonRejection>,
) -> Result<Json<DeviceSessionMutationResponse>, ApiError> {
    let repository = active_repository(&state)?;
    let current_raw = bearer_token_from_headers(&headers).ok_or_else(ApiError::unauthorized)?;
    let current = OpaqueCredential::parse(CredentialKind::DeviceRefresh, current_raw)
        .map_err(|_| ApiError::unauthorized())?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let next_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &request.next_access_token.0)
            .map_err(|_| ApiError::unauthorized())?;
    let next_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &request.next_refresh_token.0)
            .map_err(|_| ApiError::unauthorized())?;
    let result = repository
        .refresh_device_session(&current, &next_access, &next_refresh, state.clock.now())
        .await
        .map_err(map_repository_error)?;
    Ok(Json(DeviceSessionMutationResponse {
        session: DeviceSessionResponse::from(&result.value),
        replayed: result.replayed,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/auth/sessions",
    tag = "authentication",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Active refreshable device sessions", body = DeviceSessionListResponse),
        (status = 401, description = "Missing or invalid credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing auth_sessions_read", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<DeviceSessionListResponse>, ApiError> {
    let sessions = active_repository(&state)?
        .list_device_sessions(state.clock.now())
        .await
        .map_err(map_repository_error)?;
    Ok(Json(DeviceSessionListResponse {
        sessions: sessions.iter().map(DeviceSessionResponse::from).collect(),
    }))
}

#[utoipa::path(
    delete,
    path = "/v1/auth/sessions/{id}",
    tag = "authentication",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Device session ID")),
    responses(
        (status = 204, description = "Session revoked"),
        (status = 401, description = "Missing or invalid credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing auth_sessions_write", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Active session not found", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn revoke_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let changed = active_repository(&state)?
        .revoke_device_session(id, state.clock.now())
        .await
        .map_err(map_repository_error)?;
    if changed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Device session"))
    }
}

#[utoipa::path(
    delete,
    path = "/v1/auth/device-enrollments/{id}",
    tag = "authentication",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Pending enrollment ID")),
    responses(
        (status = 204, description = "Pending enrollment revoked"),
        (status = 401, description = "Missing or invalid credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing auth_sessions_write", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Pending enrollment not found", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn revoke_device_enrollment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let changed = active_repository(&state)?
        .revoke_device_enrollment(id, state.clock.now())
        .await
        .map_err(map_repository_error)?;
    if changed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Device enrollment"))
    }
}

#[utoipa::path(
    post,
    path = "/v1/auth/mcp-clients",
    tag = "authentication",
    security(("bearer_token" = [])),
    request_body = CreateMcpClientRequest,
    responses(
        (status = 201, description = "MCP credential issued once", body = McpClientIssuedResponse),
        (status = 401, description = "Missing or invalid credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing auth_mcp_clients_write or invalid audience scope", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Client identifier or credential state conflicts", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid MCP client contract", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn create_mcp_client(
    State(state): State<AppState>,
    request: Result<Json<CreateMcpClientRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    // `auth_mcp_clients_write` is the explicit cross-audience delegation
    // authority. A device principal cannot itself carry the MCP-only
    // `suggestions_submit` scope, so target scopes are validated against the
    // MCP allowlist rather than intersected with the caller's REST scopes.
    validate_requested_scopes(&request.scopes, Scope::is_mcp)?;
    let repository = active_repository(&state)?;
    let generated = GeneratedCredential::generate(CredentialKind::McpClient)
        .map_err(|_| ApiError::internal())?;
    let credential = generated.parsed().map_err(|_| ApiError::internal())?;
    let client = repository
        .register_mcp_client(
            McpClientSpec {
                id: Uuid::new_v4(),
                client_identifier: request.client_identifier,
                display_name: request.display_name,
                scopes: request.scopes,
                allowed_origins: request.allowed_origins,
                client_contract_version: request.client_contract_version,
                client_version: request.client_version,
                client_capabilities: request.client_capabilities,
                created_at: state.clock.now(),
                requested_expires_at: request.expires_at,
            },
            &credential,
        )
        .await
        .map_err(map_repository_error)?;
    Ok((
        StatusCode::CREATED,
        Json(McpClientIssuedResponse {
            client: McpClientResponse::from(&client),
            credential: generated.expose().to_owned(),
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/v1/auth/mcp-clients",
    tag = "authentication",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Active MCP clients without credential plaintext", body = McpClientListResponse),
        (status = 401, description = "Missing or invalid credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing auth_mcp_clients_read", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn list_mcp_clients(
    State(state): State<AppState>,
) -> Result<Json<McpClientListResponse>, ApiError> {
    let clients = active_repository(&state)?
        .list_mcp_clients(state.clock.now())
        .await
        .map_err(map_repository_error)?;
    Ok(Json(McpClientListResponse {
        clients: clients.iter().map(McpClientResponse::from).collect(),
    }))
}

#[utoipa::path(
    delete,
    path = "/v1/auth/mcp-clients/{id}",
    tag = "authentication",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "MCP client ID")),
    responses(
        (status = 204, description = "MCP client revoked"),
        (status = 401, description = "Missing or invalid credential", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing auth_mcp_clients_write", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Active MCP client not found", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn revoke_mcp_client(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let changed = active_repository(&state)?
        .revoke_mcp_client(id, state.clock.now())
        .await
        .map_err(map_repository_error)?;
    if changed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("MCP client"))
    }
}

fn active_repository(state: &AppState) -> Result<&dyn CredentialRepository, ApiError> {
    if state.auth_mode == AuthMode::LegacyStatic {
        return Err(ApiError::unavailable(
            "Durable authentication is not enabled",
        ));
    }
    state
        .credential_repository
        .as_deref()
        .ok_or_else(ApiError::internal)
}

fn validate_requested_scopes(
    requested: &[Scope],
    allowed_for_audience: fn(Scope) -> bool,
) -> Result<(), ApiError> {
    if requested.is_empty()
        || requested.iter().any(|scope| {
            !allowed_for_audience(*scope)
                || requested.iter().filter(|other| *other == scope).count() != 1
        })
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

fn default_device_scopes() -> Vec<Scope> {
    Scope::ALL
        .iter()
        .copied()
        .filter(|scope| scope.is_rest())
        .collect()
}

fn default_mcp_scopes() -> Vec<Scope> {
    Scope::ALL
        .iter()
        .copied()
        .filter(|scope| scope.is_mcp())
        .collect()
}

const fn current_device_contract_version() -> u16 {
    DEVICE_CLIENT_CONTRACT_VERSION
}

const fn current_mcp_contract_version() -> u16 {
    MCP_CLIENT_CONTRACT_VERSION
}

fn map_repository_error(error: CredentialRepositoryError) -> ApiError {
    match error {
        CredentialRepositoryError::InvalidCredential => ApiError::unauthorized(),
        CredentialRepositoryError::InvalidInput => {
            ApiError::validation("Credential request is invalid")
        }
        CredentialRepositoryError::Conflict => {
            ApiError::conflict("Credential state conflicts with an existing record")
        }
        CredentialRepositoryError::Internal => ApiError::internal(),
    }
}

async fn add_no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

impl From<&DeviceSession> for DeviceSessionResponse {
    fn from(session: &DeviceSession) -> Self {
        Self {
            id: session.id,
            client_instance_id: session.client_instance_id,
            client_kind: session.client_kind,
            device_label: session.device_label.clone(),
            scopes: session.scopes.clone(),
            client_contract_version: session.client_contract_version,
            client_version: session.client_version.clone(),
            client_capabilities: session.client_capabilities.clone(),
            created_at: session.created_at,
            last_seen_at: session.last_seen_at,
            credential_issued_at: session.credential_issued_at,
            access_expires_at: session.access_expires_at,
            refresh_idle_expires_at: session.refresh_idle_expires_at,
            absolute_expires_at: session.absolute_expires_at,
            revision: session.revision,
        }
    }
}

impl From<&McpClient> for McpClientResponse {
    fn from(client: &McpClient) -> Self {
        Self {
            id: client.id,
            client_identifier: client.client_identifier.clone(),
            display_name: client.display_name.clone(),
            scopes: client.scopes.clone(),
            allowed_origins: client.allowed_origins.clone(),
            client_contract_version: client.client_contract_version,
            client_version: client.client_version.clone(),
            client_capabilities: client.client_capabilities.clone(),
            created_at: client.created_at,
            last_seen_at: client.last_seen_at,
            expires_at: client.expires_at,
            revision: client.revision,
        }
    }
}
