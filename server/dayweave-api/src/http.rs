use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, StatusCode},
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
    auth::{Principal, PrincipalAudience, Scope, require_authentication},
    error::{ApiError, ErrorEnvelope},
    persistence::{PostgresProposalApplicationRepository, ProposalApplicationError},
    proposals::{
        DecisionKind, EditProposal, NewProposal, Proposal, ProposalApplicationReceipt,
        ProposalApplicationStatus, ProposalAppliedMember, ProposalApplyRequest,
        ProposalApplyResponse, ProposalChangeSet, ProposalChangeSetPreview,
        ProposalChangeSetSchema, ProposalCommand, ProposalConflict, ProposalConflictCode,
        ProposalDomainError, ProposalImplicitChangeReason, ProposalImplicitItemDiff,
        ProposalItemDiff, ProposalItemField, ProposalKind, ProposalOperation,
        ProposalPreviewMember, ProposalPreviewRequest, ProposalQuery, ProposalRisk,
        ProposalRiskCode, ProposalRiskLevel, ProposalServiceError, ProposalSource, ProposalStatus,
        ProposalUndoRequest, ProposalUndoResponse, RepositoryError,
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
        create_suggestion_application_preview,
        apply_suggestion_application_preview,
        get_suggestion_application,
        get_suggestion_application_for_proposal,
        undo_suggestion_application,
        crate::items::http::create_item,
        crate::items::http::list_items,
        crate::items::http::item_delta,
        crate::items::http::get_item,
        crate::items::http::replace_item,
        crate::items::http::delete_item,
        crate::items::http::restore_item,
        crate::scheduling::http::preview_schedule,
        crate::scheduling::http::publish_schedule,
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
        crate::google_sync::http::preview_outbound,
        crate::google_sync::http::approve_outbound,
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
        ProposalChangeSet,
        ProposalChangeSetSchema,
        ProposalCommand,
        ProposalConflict,
        ProposalConflictCode,
        ProposalImplicitChangeReason,
        ProposalImplicitItemDiff,
        ProposalItemDiff,
        ProposalItemField,
        ProposalOperation,
        ProposalPreviewMember,
        ProposalPreviewRequest,
        ProposalChangeSetPreview,
        ProposalRisk,
        ProposalRiskCode,
        ProposalRiskLevel,
        ProposalApplyRequest,
        ProposalApplyResponse,
        ProposalApplicationReceipt,
        ProposalApplicationStatus,
        ProposalAppliedMember,
        ProposalUndoRequest,
        ProposalUndoResponse,
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
        crate::scheduling::http::PublishScheduleRequest,
        crate::scheduling::SchedulePublication,
        crate::scheduling::PublishedScheduleRevision,
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
        crate::execution::DeferExecution,
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
        crate::google_sync::GoogleEventDisposition,
        crate::google_sync::GoogleCalendarPolicy,
        crate::google_sync::GoogleSyncCollection,
        crate::google_sync::GoogleSyncRunState,
        crate::google_sync::GoogleSyncRunStatus,
        crate::google_sync::GoogleSyncStatus,
        crate::google_sync::GoogleSyncRefreshAccepted,
        crate::google_sync::GoogleOutboundAccepted,
        crate::google_sync::GoogleOutboundPreview,
        crate::google_sync::GoogleOutboundApproval,
        crate::google_sync::OutboundOperation,
        crate::google_sync::http::GoogleCollectionsResponse,
        crate::google_sync::http::ConfigureGoogleCollectionRequest,
        crate::google_sync::http::GoogleCollectionResponse,
        crate::google_sync::http::GoogleSyncStatusResponse,
        crate::google_sync::http::GoogleSyncRefreshResponse,
        crate::google_sync::http::EnqueueGoogleOutboundRequest,
        crate::google_sync::http::PreviewGoogleOutboundRequest,
        crate::google_sync::http::GoogleOutboundPreviewResponse,
        crate::google_sync::http::ApproveGoogleOutboundRequest,
        crate::google_sync::http::GoogleOutboundApprovalResponse,
        crate::google_sync::http::GoogleOutboundResponse,
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "system", description = "Liveness, readiness, and build identity"),
        (name = "suggestions", description = "Reviewable proposals from AI and external tools"),
        (name = "items", description = "Canonical offline-first planner items and delta sync"),
        (name = "schedule", description = "Deterministic previews and explicit immutable publication"),
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
    let oauth_metadata = if state.mcp_oauth.is_some() {
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(crate::mcp_oauth::protected_resource_metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                get(crate::mcp_oauth::protected_resource_metadata),
            )
    } else {
        Router::new()
    };
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
        .route(
            "/suggestions/application-previews",
            post(create_suggestion_application_preview),
        )
        .route(
            "/suggestions/application-previews/{id}/apply",
            post(apply_suggestion_application_preview),
        )
        .route(
            "/suggestions/applications/{id}",
            get(get_suggestion_application),
        )
        .route(
            "/suggestions/{id}/application",
            get(get_suggestion_application_for_proposal),
        )
        .route(
            "/suggestions/applications/{id}/undo",
            post(undo_suggestion_application),
        )
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
        .merge(oauth_metadata)
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
    /// Legacy provenance hint. Device REST submissions are always stored as
    /// `app_assistant`, regardless of this caller-controlled value.
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
    let (source, source_reference) = match principal.audience {
        PrincipalAudience::Device => (ProposalSource::AppAssistant, None),
        PrincipalAudience::Legacy => (request.source, request.source_reference),
        PrincipalAudience::Mcp | PrincipalAudience::McpOAuth => {
            return Err(ApiError::forbidden());
        }
    };
    let proposal = state
        .proposals
        .create(NewProposal {
            submitted_by: principal.subject,
            source,
            source_reference,
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
        (status = 409, description = "Stale revision, terminal suggestion, or reserved atomic change set", body = ErrorEnvelope)
    )
)]
async fn accept_suggestion(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request: Result<Json<DecisionRequest>, JsonRejection>,
) -> Result<Json<SuggestionEnvelope>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let suggestion = state
        .proposals
        .decide(
            id,
            request.expected_revision,
            DecisionKind::Accept,
            request.note,
        )
        .await
        .map_err(map_service_error)?;
    Ok(Json(SuggestionEnvelope { suggestion }))
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
    post,
    path = "/v1/suggestions/application-previews",
    tag = "suggestions",
    security(("bearer_token" = [])),
    request_body = ProposalPreviewRequest,
    responses(
        (status = 201, description = "Content-bound atomic application preview", body = ProposalChangeSetPreview),
        (status = 409, description = "Proposal or canonical item changed", body = ErrorEnvelope),
        (status = 422, description = "Legacy, untyped, or invalid proposal", body = ErrorEnvelope)
    )
)]
async fn create_suggestion_application_preview(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Result<Json<ProposalPreviewRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let repository = application_repository(
        &state,
        &principal,
        &[
            Scope::SuggestionsRead,
            Scope::SuggestionsWrite,
            Scope::ItemsRead,
        ],
    )?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let preview = repository
        .preview(request)
        .await
        .map_err(map_application_error)?;
    Ok((StatusCode::CREATED, Json(preview)))
}

#[utoipa::path(
    post,
    path = "/v1/suggestions/application-previews/{id}/apply",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Application preview identifier"),
        ("Idempotency-Key" = String, Header, description = "8-128 character retry key")
    ),
    request_body = ProposalApplyRequest,
    responses(
        (status = 200, description = "Atomic proposal application receipt", body = ProposalApplyResponse),
        (status = 409, description = "Stale preview, conflict, or idempotency mismatch", body = ErrorEnvelope)
    )
)]
async fn apply_suggestion_application_preview(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(preview_id): Path<Uuid>,
    headers: HeaderMap,
    request: Result<Json<ProposalApplyRequest>, JsonRejection>,
) -> Result<Json<ProposalApplyResponse>, ApiError> {
    let repository = application_repository(
        &state,
        &principal,
        &[Scope::SuggestionsWrite, Scope::ItemsWrite],
    )?;
    let idempotency_key = idempotency_header(&headers)?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let response = repository
        .apply(
            preview_id,
            request,
            idempotency_key,
            principal.credential_id,
        )
        .await
        .map_err(map_application_error)?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/v1/suggestions/applications/{id}",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Proposal application identifier")),
    responses(
        (status = 200, description = "Durable proposal application receipt", body = ProposalApplicationReceipt),
        (status = 404, description = "Application not found", body = ErrorEnvelope)
    )
)]
async fn get_suggestion_application(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(application_id): Path<Uuid>,
) -> Result<Json<ProposalApplicationReceipt>, ApiError> {
    let repository = application_repository(
        &state,
        &principal,
        &[Scope::SuggestionsRead, Scope::ItemsRead],
    )?;
    let response = repository
        .get(application_id)
        .await
        .map_err(map_application_error)?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/v1/suggestions/{id}/application",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Applied proposal identifier")),
    responses(
        (status = 200, description = "Application receipt linked to this proposal", body = ProposalApplicationReceipt),
        (status = 404, description = "Proposal has no application", body = ErrorEnvelope)
    )
)]
async fn get_suggestion_application_for_proposal(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(proposal_id): Path<Uuid>,
) -> Result<Json<ProposalApplicationReceipt>, ApiError> {
    let repository = application_repository(
        &state,
        &principal,
        &[Scope::SuggestionsRead, Scope::ItemsRead],
    )?;
    let response = repository
        .get_for_proposal(proposal_id)
        .await
        .map_err(map_application_error)?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/v1/suggestions/applications/{id}/undo",
    tag = "suggestions",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Proposal application identifier"),
        ("Idempotency-Key" = String, Header, description = "8-128 character retry key")
    ),
    request_body = ProposalUndoRequest,
    responses(
        (status = 200, description = "Atomic undo receipt", body = ProposalUndoResponse),
        (status = 409, description = "Undo expired or affected canonical state diverged", body = ErrorEnvelope)
    )
)]
async fn undo_suggestion_application(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(application_id): Path<Uuid>,
    headers: HeaderMap,
    request: Result<Json<ProposalUndoRequest>, JsonRejection>,
) -> Result<Json<ProposalUndoResponse>, ApiError> {
    let repository = application_repository(
        &state,
        &principal,
        &[Scope::SuggestionsWrite, Scope::ItemsWrite],
    )?;
    let idempotency_key = idempotency_header(&headers)?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let response = repository
        .undo(
            application_id,
            request,
            idempotency_key,
            principal.credential_id,
        )
        .await
        .map_err(map_application_error)?;
    Ok(Json(response))
}

fn application_repository(
    state: &AppState,
    principal: &Principal,
    required: &[Scope],
) -> Result<Arc<PostgresProposalApplicationRepository>, ApiError> {
    if principal.audience != PrincipalAudience::Device
        || required.iter().any(|scope| !principal.has_scope(*scope))
    {
        return Err(ApiError::forbidden());
    }
    let repository = state
        .proposal_applications
        .clone()
        .ok_or_else(|| ApiError::unavailable("Proposal application requires PostgreSQL"))?;
    let scope = repository.scope();
    if principal.workspace_id != Some(scope.workspace_id)
        || principal.user_id != Some(scope.user_id)
    {
        return Err(ApiError::not_found("proposal application"));
    }
    Ok(repository)
}

fn idempotency_header(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::validation("Idempotency-Key header is required"))
}

fn map_application_error(error: ProposalApplicationError) -> ApiError {
    match error {
        ProposalApplicationError::Validation(message) => ApiError::validation(message),
        ProposalApplicationError::NotFound | ProposalApplicationError::OwnerUnavailable => {
            ApiError::not_found("proposal application")
        }
        ProposalApplicationError::Stale(code) => {
            ApiError::conflict("Proposal application is stale or unsafe").with_details(json!({
                "conflict_code": code,
            }))
        }
        ProposalApplicationError::RevisionConflict { expected, actual } => {
            ApiError::conflict("Proposal application revision changed").with_details(json!({
                "expected_revision": expected,
                "actual_revision": actual,
            }))
        }
        ProposalApplicationError::IdempotencyConflict => {
            ApiError::conflict("Idempotency-Key was already used for different content")
        }
        ProposalApplicationError::Internal => ApiError::internal(),
    }
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
        ProposalServiceError::ReservedChangeSetRequiresApplication => {
            ApiError::conflict("Actionable suggestions must be previewed and applied atomically")
        }
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
