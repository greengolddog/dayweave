use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderName, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use utoipa::{
    IntoParams, Modify, OpenApi, ToSchema,
    openapi::security::{Http, HttpAuthScheme, SecurityScheme},
};
use uuid::Uuid;

use crate::{
    AppState,
    auth::{Principal, require_authentication},
    error::{ApiError, ErrorEnvelope},
    proposals::{
        DecisionKind, EditProposal, NewProposal, Proposal, ProposalDomainError, ProposalKind,
        ProposalQuery, ProposalServiceError, ProposalSource, ProposalStatus, RepositoryError,
    },
};

const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: usize = 200;

#[derive(OpenApi)]
#[openapi(
    paths(
        health,
        readiness,
        version,
        create_suggestion,
        list_suggestions,
        get_suggestion,
        edit_suggestion,
        accept_suggestion,
        reject_suggestion,
        delete_suggestion,
        crate::items::http::create_item,
        crate::items::http::list_items,
        crate::items::http::item_delta,
        crate::items::http::get_item,
        crate::items::http::replace_item,
        crate::items::http::delete_item,
        crate::items::http::restore_item,
        crate::scheduling::http::preview_schedule,
        crate::execution::http::get_execution,
        crate::execution::http::apply_execution_command,
        crate::execution::http::execution_history,
        crate::google_oauth::http::start,
        crate::google_oauth::http::accounts,
        crate::google_oauth::http::pause,
        crate::google_oauth::http::resume,
        crate::google_oauth::http::disconnect,
        crate::google_oauth::http::acknowledge_recovery,
        crate::google_oauth::http::callback,
        crate::google_sync::http::list_collections,
        crate::google_sync::http::discover_collections,
        crate::google_sync::http::configure_collection,
        crate::google_sync::http::sync_status,
        crate::google_sync::http::manual_refresh,
        crate::google_sync::http::enqueue_outbound,
        crate::credential_auth::http::create_device_enrollment,
        crate::credential_auth::http::consume_device_enrollment,
        crate::credential_auth::http::refresh_session,
        crate::credential_auth::http::list_sessions,
        crate::credential_auth::http::revoke_session,
        crate::credential_auth::http::revoke_device_enrollment,
        crate::credential_auth::http::create_mcp_client,
        crate::credential_auth::http::list_mcp_clients,
        crate::credential_auth::http::revoke_mcp_client,
    ),
    components(schemas(
        HealthResponse,
        VersionResponse,
        CreateSuggestionRequest,
        EditSuggestionRequest,
        DecisionRequest,
        SuggestionEnvelope,
        SuggestionListEnvelope,
        Proposal,
        ProposalSource,
        ProposalKind,
        ProposalStatus,
        ErrorEnvelope,
        crate::items::NewItem,
        crate::items::ReplaceItem,
        crate::items::Item,
        crate::items::SplitPolicy,
        crate::items::ItemKind,
        crate::items::ItemStatus,
        crate::items::DeltaChange,
        crate::items::ItemTombstone,
        crate::items::http::ReplaceItemRequest,
        crate::items::http::RevisionRequest,
        crate::items::http::ItemEnvelope,
        crate::items::http::ItemListEnvelope,
        crate::items::http::ItemDeltaEnvelope,
        crate::scheduling::ComposeScheduleRequest,
        crate::scheduling::ComposeScheduleResult,
        crate::scheduling::AvailabilityInput,
        crate::scheduling::EnergyInput,
        crate::scheduling::FixedBlockInput,
        crate::scheduling::FixedBlockSourceInput,
        crate::scheduling::PreviousAssignmentInput,
        crate::scheduling::PreviousBlockInput,
        crate::scheduling::SchedulerConfigInput,
        crate::scheduling::RejectedScheduleItem,
        crate::scheduling::IgnoredPreviousAssignment,
        crate::execution::ExecutionStatus,
        crate::execution::ExecutionSession,
        crate::execution::ExecutionCommand,
        crate::execution::StartExecution,
        crate::execution::PauseExecution,
        crate::execution::ResumeExecution,
        crate::execution::FinishExecution,
        crate::execution::ExecutionSnapshot,
        crate::execution::ExecutionMutation,
        crate::execution::http::ExecutionCommandRequest,
        crate::execution::http::ExecutionSnapshotEnvelope,
        crate::execution::http::ExecutionMutationEnvelope,
        crate::execution::http::ExecutionHistoryEnvelope,
        crate::google_oauth::GoogleAccount,
        crate::google_oauth::GoogleAccountStatus,
        crate::google_oauth::GoogleOAuthCleanupStatus,
        crate::google_oauth::http::StartGoogleOAuthRequest,
        crate::google_oauth::http::StartGoogleOAuthResponse,
        crate::google_oauth::http::GoogleAccountsResponse,
        crate::google_oauth::http::AccountRevisionRequest,
        crate::google_oauth::http::AcknowledgeGoogleOAuthRecoveryRequest,
        crate::google_oauth::http::AcknowledgeGoogleOAuthRecoveryResponse,
        crate::google_sync::GoogleCollectionKind,
        crate::google_sync::GoogleSyncRole,
        crate::google_sync::GoogleSyncCollection,
        crate::google_sync::GoogleSyncRunState,
        crate::google_sync::GoogleSyncRunStatus,
        crate::google_sync::GoogleSyncStatus,
        crate::google_sync::GoogleSyncRefreshAccepted,
        crate::google_sync::GoogleOutboundAccepted,
        crate::google_sync::OutboundOperation,
        crate::google_sync::http::GoogleCollectionsResponse,
        crate::google_sync::http::ConfigureGoogleCollectionRequest,
        crate::google_sync::http::GoogleCollectionResponse,
        crate::google_sync::http::GoogleSyncStatusResponse,
        crate::google_sync::http::GoogleSyncRefreshResponse,
        crate::google_sync::http::EnqueueGoogleOutboundRequest,
        crate::google_sync::http::GoogleOutboundResponse,
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "system", description = "Liveness, readiness, and build identity"),
        (name = "suggestions", description = "Reviewable proposals from AI and external tools"),
        (name = "items", description = "Canonical offline-first planner items and delta sync"),
        (name = "schedule", description = "Deterministic side-effect-free planning previews"),
        (name = "execution", description = "Server-authoritative cross-device timers and breaks"),
        (name = "google", description = "Google Calendar and Tasks identity authorization"),
        (name = "google_sync", description = "Durable Google Calendar and Tasks reconciliation"),
        (name = "authentication", description = "Device enrollment, credential rotation, and MCP client management")
    )
)]
pub struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_token",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route(
            "/suggestions",
            get(list_suggestions).post(create_suggestion),
        )
        .route(
            "/suggestions/{id}",
            get(get_suggestion)
                .patch(edit_suggestion)
                .delete(delete_suggestion),
        )
        .route("/suggestions/{id}/accept", post(accept_suggestion))
        .route("/suggestions/{id}/reject", post(reject_suggestion))
        .merge(crate::items::http::routes())
        .merge(crate::scheduling::http::routes())
        .merge(crate::execution::http::routes())
        .merge(crate::google_oauth::http::protected_routes())
        .merge(crate::google_sync::http::routes())
        .merge(crate::credential_auth::http::protected_routes())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_authentication,
        ));

    let request_id_header = HeaderName::from_static("x-request-id");
    Router::new()
        .route("/healthz", get(health))
        .route("/health", get(health))
        .route("/readyz", get(readiness))
        .route("/ready", get(readiness))
        .route("/version", get(version))
        .route("/openapi.json", get(openapi))
        .route("/mcp", post(crate::mcp::handle_post))
        .merge(crate::google_oauth::http::public_routes())
        .merge(crate::credential_auth::http::public_routes())
        .nest("/v1", protected)
        .fallback(not_found)
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(
                    request_id_header.clone(),
                    MakeRequestUuid,
                ))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &axum::http::Request<_>| {
                            // OAuth callbacks carry state and an authorization code in the
                            // query. Deliberately omit every URI from request spans.
                            tracing::info_span!("http_request", method = %request.method())
                        })
                        .on_response(
                            DefaultOnResponse::new()
                                .include_headers(false)
                                .latency_unit(tower_http::LatencyUnit::Millis),
                        ),
                )
                .layer(PropagateRequestIdLayer::new(request_id_header)),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024))
}

#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    responses((status = 200, description = "Process is alive", body = HealthResponse))
)]
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[utoipa::path(
    get,
    path = "/ready",
    tag = "system",
    responses(
        (status = 200, description = "Process can serve traffic", body = HealthResponse),
        (status = 503, description = "Process is not ready", body = ErrorEnvelope)
    )
)]
async fn readiness(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    if state.readiness.check().await {
        Ok(Json(HealthResponse { status: "ready" }))
    } else {
        Err(ApiError::unavailable("DayWeave API is not ready"))
    }
}

#[derive(Serialize, ToSchema)]
pub struct VersionResponse {
    pub name: &'static str,
    pub version: &'static str,
    pub git_revision: Option<&'static str>,
}

#[utoipa::path(
    get,
    path = "/version",
    tag = "system",
    responses((status = 200, description = "Build identity", body = VersionResponse))
)]
async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        git_revision: option_env!("DAYWEAVE_GIT_REVISION"),
    })
}

async fn openapi() -> Json<Value> {
    Json(serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI document is serializable"))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSuggestionRequest {
    pub source: ProposalSource,
    pub source_reference: Option<String>,
    pub kind: ProposalKind,
    pub title: String,
    pub explanation: Option<String>,
    #[schema(value_type = Object)]
    pub payload: Value,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EditSuggestionRequest {
    pub expected_revision: u64,
    pub title: Option<String>,
    pub explanation: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub payload: Option<Value>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DecisionRequest {
    pub expected_revision: u64,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SuggestionListQuery {
    pub status: Option<ProposalStatus>,
    pub source: Option<ProposalSource>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct DeleteSuggestionQuery {
    pub expected_revision: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SuggestionEnvelope {
    pub suggestion: Proposal,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SuggestionListEnvelope {
    pub suggestions: Vec<Proposal>,
}

#[utoipa::path(
    post,
    path = "/v1/suggestions",
    tag = "suggestions",
    security(("bearer_token" = [])),
    request_body = CreateSuggestionRequest,
    responses(
        (status = 201, description = "Suggestion submitted for review", body = SuggestionEnvelope),
        (status = 401, description = "Missing or invalid token", body = ErrorEnvelope),
        (status = 422, description = "Invalid suggestion", body = ErrorEnvelope)
    )
)]
async fn create_suggestion(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Result<Json<CreateSuggestionRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let proposal = state
        .proposals
        .create(NewProposal {
            submitted_by: principal.subject,
            source: request.source,
            source_reference: request.source_reference,
            kind: request.kind,
            title: request.title,
            explanation: request.explanation,
            payload: request.payload,
            expires_at: request
                .expires_at
                .unwrap_or_else(|| state.proposals.default_expiration()),
        })
        .await
        .map_err(map_service_error)?;
    Ok((
        StatusCode::CREATED,
        Json(SuggestionEnvelope {
            suggestion: proposal,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/suggestions",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(SuggestionListQuery),
    responses(
        (status = 200, description = "Suggestions matching the filter", body = SuggestionListEnvelope),
        (status = 401, description = "Missing or invalid token", body = ErrorEnvelope)
    )
)]
async fn list_suggestions(
    State(state): State<AppState>,
    Query(query): Query<SuggestionListQuery>,
) -> Result<Json<SuggestionListEnvelope>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(ApiError::validation(format!(
            "limit must be between 1 and {MAX_LIST_LIMIT}"
        )));
    }
    let suggestions = state
        .proposals
        .list(ProposalQuery {
            status: query.status,
            source: query.source,
            limit,
        })
        .await
        .map_err(map_service_error)?;
    Ok(Json(SuggestionListEnvelope { suggestions }))
}

#[utoipa::path(
    get,
    path = "/v1/suggestions/{id}",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Suggestion identifier")),
    responses(
        (status = 200, description = "Suggestion", body = SuggestionEnvelope),
        (status = 404, description = "Suggestion not found", body = ErrorEnvelope)
    )
)]
async fn get_suggestion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SuggestionEnvelope>, ApiError> {
    let suggestion = state.proposals.get(id).await.map_err(map_service_error)?;
    Ok(Json(SuggestionEnvelope { suggestion }))
}

#[utoipa::path(
    patch,
    path = "/v1/suggestions/{id}",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Suggestion identifier")),
    request_body = EditSuggestionRequest,
    responses(
        (status = 200, description = "Edited pending suggestion", body = SuggestionEnvelope),
        (status = 409, description = "Stale revision or terminal suggestion", body = ErrorEnvelope),
        (status = 422, description = "Invalid edit", body = ErrorEnvelope)
    )
)]
async fn edit_suggestion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Result<Json<EditSuggestionRequest>, JsonRejection>,
) -> Result<Json<SuggestionEnvelope>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let suggestion = state
        .proposals
        .edit(
            id,
            request.expected_revision,
            EditProposal {
                title: request.title,
                explanation: request.explanation,
                payload: request.payload,
                expires_at: request.expires_at,
            },
        )
        .await
        .map_err(map_service_error)?;
    Ok(Json(SuggestionEnvelope { suggestion }))
}

#[utoipa::path(
    post,
    path = "/v1/suggestions/{id}/accept",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Suggestion identifier")),
    request_body = DecisionRequest,
    responses(
        (status = 200, description = "Suggestion approved", body = SuggestionEnvelope),
        (status = 409, description = "Stale revision or terminal suggestion", body = ErrorEnvelope)
    )
)]
async fn accept_suggestion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Json<SuggestionEnvelope>, ApiError> {
    decide_suggestion(state, id, DecisionKind::Accept, request).await
}

#[utoipa::path(
    post,
    path = "/v1/suggestions/{id}/reject",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Suggestion identifier")),
    request_body = DecisionRequest,
    responses(
        (status = 200, description = "Suggestion rejected", body = SuggestionEnvelope),
        (status = 409, description = "Stale revision or terminal suggestion", body = ErrorEnvelope)
    )
)]
async fn reject_suggestion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Json<SuggestionEnvelope>, ApiError> {
    decide_suggestion(state, id, DecisionKind::Reject, request).await
}

async fn decide_suggestion(
    state: AppState,
    id: Uuid,
    decision: DecisionKind,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Json<SuggestionEnvelope>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let suggestion = state
        .proposals
        .decide(id, request.expected_revision, decision, request.note)
        .await
        .map_err(map_service_error)?;
    Ok(Json(SuggestionEnvelope { suggestion }))
}

#[utoipa::path(
    delete,
    path = "/v1/suggestions/{id}",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Suggestion identifier"),
        DeleteSuggestionQuery
    ),
    responses(
        (status = 204, description = "Suggestion deleted"),
        (status = 409, description = "Stale revision", body = ErrorEnvelope)
    )
)]
async fn delete_suggestion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteSuggestionQuery>,
) -> Result<StatusCode, ApiError> {
    state
        .proposals
        .delete(id, query.expected_revision)
        .await
        .map_err(map_service_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn not_found() -> ApiError {
    ApiError::not_found("route")
}

fn map_service_error(error: ProposalServiceError) -> ApiError {
    match error {
        ProposalServiceError::Domain(ProposalDomainError::NotPending(status)) => {
            ApiError::conflict(format!("suggestion is already {status:?}"))
        }
        ProposalServiceError::Domain(error) => ApiError::validation(error.to_string()),
        ProposalServiceError::Repository(RepositoryError::NotFound(_)) => {
            ApiError::not_found("suggestion")
        }
        ProposalServiceError::Repository(RepositoryError::RevisionConflict {
            expected,
            actual,
        }) => ApiError::conflict("suggestion was changed by another request").with_details(json!({
            "expected_revision": expected,
            "actual_revision": actual,
        })),
        ProposalServiceError::Repository(RepositoryError::Duplicate(_)) => {
            ApiError::conflict("suggestion already exists")
        }
        ProposalServiceError::Repository(RepositoryError::Internal) => ApiError::internal(),
    }
}
