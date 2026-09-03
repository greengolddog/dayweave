use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    AppState,
    auth::{Principal, PrincipalAudience, Scope},
    error::ApiError,
    google_oauth::{GoogleOAuthRepositoryError, GoogleOAuthServiceError, OAuthScope},
    items::{ItemRepositoryError, ItemServiceError},
};

use super::{
    GoogleCalendarPolicy, GoogleOutboundAccepted, GoogleOutboundApproval, GoogleOutboundPreview,
    GoogleSyncCollection, GoogleSyncRefreshAccepted, GoogleSyncRepositoryError, GoogleSyncRole,
    GoogleSyncService, GoogleSyncServiceError, GoogleSyncStatus, OutboundOperation,
    OutboundRequest, ScheduleGooglePublicationAccepted, ScheduleGooglePublicationApproval,
    ScheduleGooglePublicationPreview, ScheduleGooglePublicationStatus,
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/integrations/google/accounts/{account_id}/collections",
            get(list_collections),
        )
        .route(
            "/integrations/google/accounts/{account_id}/collections/discover",
            post(discover_collections),
        )
        .route(
            "/integrations/google/accounts/{account_id}/collections/{collection_id}",
            put(configure_collection),
        )
        .route(
            "/integrations/google/accounts/{account_id}/sync",
            get(sync_status),
        )
        .route(
            "/integrations/google/accounts/{account_id}/sync/refresh",
            post(manual_refresh),
        )
        .route(
            "/integrations/google/accounts/{account_id}/outbound/previews",
            post(preview_outbound),
        )
        .route(
            "/integrations/google/accounts/{account_id}/outbound/previews/{preview_id}/approve",
            post(approve_outbound),
        )
        .route(
            "/integrations/google/accounts/{account_id}/outbound",
            post(enqueue_outbound),
        )
        .route(
            "/integrations/google/accounts/{account_id}/schedule-publications/previews",
            post(preview_schedule_publication),
        )
        .route(
            "/integrations/google/accounts/{account_id}/schedule-publications/previews/{preview_id}/approve",
            post(approve_schedule_publication),
        )
        .route(
            "/integrations/google/accounts/{account_id}/schedule-publications",
            post(enqueue_schedule_publication),
        )
        .route(
            "/integrations/google/accounts/{account_id}/schedule-publications/{publication_id}",
            get(schedule_publication_status),
        )
        .layer(middleware::map_response(add_no_store))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GoogleCollectionsResponse {
    pub collections: Vec<GoogleSyncCollection>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigureGoogleCollectionRequest {
    pub expected_revision: u64,
    pub selected: bool,
    pub visible: bool,
    pub sync_role: GoogleSyncRole,
    #[serde(default)]
    pub calendar_policy: GoogleCalendarPolicy,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GoogleCollectionResponse {
    pub collection: GoogleSyncCollection,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GoogleSyncStatusResponse {
    pub sync: GoogleSyncStatus,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GoogleSyncRefreshResponse {
    pub refresh: GoogleSyncRefreshAccepted,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GoogleSyncRefreshRequest {
    pub request_id: Uuid,
}

// Deliberately not `Debug`: the body contains a one-time bearer capability.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EnqueueGoogleOutboundRequest {
    pub collection_id: Uuid,
    pub item_id: Uuid,
    pub expected_item_revision: u64,
    pub operation: OutboundOperation,
    pub approval_capability: String,
}

impl Drop for EnqueueGoogleOutboundRequest {
    fn drop(&mut self) {
        self.approval_capability.zeroize();
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewGoogleOutboundRequest {
    pub collection_id: Uuid,
    pub item_id: Uuid,
    pub expected_item_revision: u64,
    pub operation: OutboundOperation,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GoogleOutboundPreviewResponse {
    pub preview: GoogleOutboundPreview,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveGoogleOutboundRequest {
    pub expected_preview_hash: String,
}

// Deliberately not `Debug`: the response contains a one-time bearer capability.
#[derive(Serialize, ToSchema)]
pub struct GoogleOutboundApprovalResponse {
    pub approval: GoogleOutboundApproval,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GoogleOutboundResponse {
    pub outbound: GoogleOutboundAccepted,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewScheduleGooglePublicationRequest {
    pub collection_id: Uuid,
    pub expected_schedule_revision_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveScheduleGooglePublicationRequest {
    #[schema(min_length = 64, max_length = 64, pattern = "^[0-9a-f]{64}$")]
    pub expected_preview_hash: String,
}

// Deliberately not `Debug`: the body contains a one-time bearer capability.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EnqueueScheduleGooglePublicationRequest {
    pub preview_id: Uuid,
    pub collection_id: Uuid,
    pub expected_schedule_revision_id: Uuid,
    #[schema(
        min_length = 51,
        max_length = 51,
        pattern = "^dw_gsa1_[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$"
    )]
    pub approval_capability: String,
}

impl Drop for EnqueueScheduleGooglePublicationRequest {
    fn drop(&mut self) {
        self.approval_capability.zeroize();
    }
}

#[utoipa::path(
    get,
    path = "/v1/integrations/google/accounts/{account_id}/collections",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    responses(
        (status = 200, description = "Durably discovered Calendar and Tasks collections", body = GoogleCollectionsResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Google account is not active for sync", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn list_collections(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<GoogleCollectionsResponse>, ApiError> {
    let collections = configured_service(&state)?
        .collections(account_id)
        .await
        .map_err(map_service_error)?;
    Ok(Json(GoogleCollectionsResponse { collections }))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/collections/discover",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    responses(
        (status = 200, description = "Complete paginated provider discovery persisted", body = GoogleCollectionsResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Google authorization is not active", body = crate::error::ErrorEnvelope),
        (status = 502, description = "Google discovery failed", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured or temporarily unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn discover_collections(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<GoogleCollectionsResponse>, ApiError> {
    let collections = configured_service(&state)?
        .discover(account_id)
        .await
        .map_err(map_service_error)?;
    Ok(Json(GoogleCollectionsResponse { collections }))
}

#[utoipa::path(
    put,
    path = "/v1/integrations/google/accounts/{account_id}/collections/{collection_id}",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(
        ("account_id" = Uuid, Path, description = "Connected Google account"),
        ("collection_id" = Uuid, Path, description = "Discovered collection")
    ),
    request_body = ConfigureGoogleCollectionRequest,
    responses(
        (status = 200, description = "Collection selection and role updated", body = GoogleCollectionResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account or collection not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Revision, provider access, deletion, or scope conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid collection configuration", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn configure_collection(
    State(state): State<AppState>,
    Path((account_id, collection_id)): Path<(Uuid, Uuid)>,
    request: Result<Json<ConfigureGoogleCollectionRequest>, JsonRejection>,
) -> Result<Json<GoogleCollectionResponse>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let collection = configured_service(&state)?
        .configure_collection(
            account_id,
            collection_id,
            request.expected_revision,
            request.selected,
            request.visible,
            request.sync_role,
            request.calendar_policy,
        )
        .await
        .map_err(map_service_error)?;
    Ok(Json(GoogleCollectionResponse { collection }))
}

#[utoipa::path(
    get,
    path = "/v1/integrations/google/accounts/{account_id}/sync",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    responses(
        (status = 200, description = "Reconciliation, backoff, conflict, and outbound status", body = GoogleSyncStatusResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Google account is not active for sync", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn sync_status(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<GoogleSyncStatusResponse>, ApiError> {
    let sync = configured_service(&state)?
        .status(account_id)
        .await
        .map_err(map_service_error)?;
    Ok(Json(GoogleSyncStatusResponse { sync }))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/sync/refresh",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    request_body = GoogleSyncRefreshRequest,
    responses(
        (status = 202, description = "Durable manual reconciliation request accepted", body = GoogleSyncRefreshResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Google account is not active for sync", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn manual_refresh(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    request: Result<Json<GoogleSyncRefreshRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let refresh = configured_service(&state)?
        .request_refresh(account_id, request.request_id)
        .await
        .map_err(map_service_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(GoogleSyncRefreshResponse { refresh }),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/outbound/previews",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    request_body = PreviewGoogleOutboundRequest,
    responses(
        (status = 200, description = "Exact provider mutation preview with a content-bound review hash", body = GoogleOutboundPreviewResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account, collection, or canonical item not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Publication disabled or revision, ownership, policy, scope, or role conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Canonical item cannot form a safe provider write", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn preview_outbound(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    request: Result<Json<PreviewGoogleOutboundRequest>, JsonRejection>,
) -> Result<Json<GoogleOutboundPreviewResponse>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let preview = configured_service(&state)?
        .preview_outbound(
            account_id,
            OutboundRequest {
                collection_id: request.collection_id,
                item_id: request.item_id,
                expected_item_revision: request.expected_item_revision,
                operation: request.operation,
            },
        )
        .await
        .map_err(map_service_error)?;
    Ok(Json(GoogleOutboundPreviewResponse { preview }))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/outbound/previews/{preview_id}/approve",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(
        ("account_id" = Uuid, Path, description = "Connected Google account"),
        ("preview_id" = Uuid, Path, description = "Server-minted outbound preview")
    ),
    request_body = ApproveGoogleOutboundRequest,
    responses(
        (status = 200, description = "Single-use expiring approval capability returned exactly once", body = GoogleOutboundApprovalResponse),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Preview expired, changed, already approved, or publication disabled", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn approve_outbound(
    State(state): State<AppState>,
    Path((account_id, preview_id)): Path<(Uuid, Uuid)>,
    request: Result<Json<ApproveGoogleOutboundRequest>, JsonRejection>,
) -> Result<Json<GoogleOutboundApprovalResponse>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let approval = configured_service(&state)?
        .approve_outbound(account_id, preview_id, &request.expected_preview_hash)
        .await
        .map_err(map_service_error)?;
    Ok(Json(GoogleOutboundApprovalResponse { approval }))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/outbound",
    tag = "google_sync",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    request_body = EnqueueGoogleOutboundRequest,
    responses(
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google account, collection, or canonical item not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "External publication is disabled until a server-minted audited approval exists, or another revision/ownership/trash/scope/role conflict applies", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Canonical item cannot form a safe provider write", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn enqueue_outbound(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    request: Result<Json<EnqueueGoogleOutboundRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let mut request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let approval_capability = std::mem::take(&mut request.approval_capability);
    let outbound = configured_service(&state)?
        .enqueue_outbound(
            account_id,
            OutboundRequest {
                collection_id: request.collection_id,
                item_id: request.item_id,
                expected_item_revision: request.expected_item_revision,
                operation: request.operation,
            },
            approval_capability,
        )
        .await
        .map_err(map_service_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(GoogleOutboundResponse { outbound }),
    ))
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/schedule-publications/previews",
    tag = "google_sync",
    description = "Requires a native Device principal with GoogleWrite (`google_write`) and ScheduleRead (`schedule_read`) scopes and exact user/workspace tenant binding.",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    request_body = PreviewScheduleGooglePublicationRequest,
    responses(
        (status = 200, description = "Exact review-safe Google Calendar changes for one immutable generated schedule", body = ScheduleGooglePublicationPreview),
        (status = 400, description = "Invalid JSON or UUID path parameter", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Native Device principal with google_write and schedule_read is required", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google integration or requested publication source is unavailable for the authenticated tenant", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Schedule publication is stale or conflicts with Google ownership, scope, or collection state", body = crate::error::ErrorEnvelope),
        (status = 413, description = "Request body exceeds the 1 MiB API limit", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Nil UUID or invalid generated-schedule publication input", body = crate::error::ErrorEnvelope),
        (status = 429, description = "Active generated-schedule preview quota exceeded", body = crate::error::ErrorEnvelope),
        (status = 502, description = "Generated schedule exceeds the bounded provider projection or Google returned an invalid response", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Generated-schedule Google publication is disabled or unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn preview_schedule_publication(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(account_id): Path<Uuid>,
    request: Result<Json<PreviewScheduleGooglePublicationRequest>, JsonRejection>,
) -> Result<Json<ScheduleGooglePublicationPreview>, ApiError> {
    let service = configured_schedule_publication_service(&state, &principal)?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    service
        .preview_schedule_publication(
            account_id,
            request.collection_id,
            request.expected_schedule_revision_id,
        )
        .await
        .map(Json)
        .map_err(map_service_error)
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/schedule-publications/previews/{preview_id}/approve",
    tag = "google_sync",
    description = "Requires a native Device principal with GoogleWrite (`google_write`) and ScheduleRead (`schedule_read`) scopes and exact user/workspace tenant binding. Unknown previews, account bindings, or preview hashes are reported as a non-enumerating conflict.",
    security(("bearer_token" = [])),
    params(
        ("account_id" = Uuid, Path, description = "Connected Google account"),
        ("preview_id" = Uuid, Path, description = "Server-minted generated-schedule preview")
    ),
    request_body = ApproveScheduleGooglePublicationRequest,
    responses(
        (status = 200, description = "Single-use expiring approval capability returned exactly once", body = ScheduleGooglePublicationApproval),
        (status = 400, description = "Invalid JSON or UUID path parameter", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Native Device principal with google_write and schedule_read is required", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google integration unavailable for authenticated tenant", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Preview, account binding, or hash is unknown or mismatched; preview expired, changed, or was already approved", body = crate::error::ErrorEnvelope),
        (status = 413, description = "Request body exceeds the 1 MiB API limit", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Nil account or preview UUID", body = crate::error::ErrorEnvelope),
        (status = 502, description = "Current generated schedule cannot be projected within the bounded Google contract", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Generated-schedule Google publication is disabled or unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn approve_schedule_publication(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((account_id, preview_id)): Path<(Uuid, Uuid)>,
    request: Result<Json<ApproveScheduleGooglePublicationRequest>, JsonRejection>,
) -> Result<Json<ScheduleGooglePublicationApproval>, ApiError> {
    let service = configured_schedule_publication_service(&state, &principal)?;
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    service
        .approve_schedule_publication(account_id, preview_id, &request.expected_preview_hash)
        .await
        .map(Json)
        .map_err(map_service_error)
}

#[utoipa::path(
    post,
    path = "/v1/integrations/google/accounts/{account_id}/schedule-publications",
    tag = "google_sync",
    description = "Requires a native Device principal with GoogleWrite (`google_write`) and ScheduleRead (`schedule_read`) scopes and exact user/workspace tenant binding. Unknown or malformed approval capabilities and mismatched immutable bindings are reported as a non-enumerating conflict.",
    security(("bearer_token" = [])),
    params(("account_id" = Uuid, Path, description = "Connected Google account")),
    request_body = EnqueueScheduleGooglePublicationRequest,
    responses(
        (status = 202, description = "Generated-schedule publication accepted for durable delivery", body = ScheduleGooglePublicationAccepted),
        (status = 400, description = "Invalid JSON or UUID path parameter", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Native Device principal with google_write and schedule_read is required", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google integration unavailable for authenticated tenant", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Approval capability is unknown, malformed, or expired, or immutable publication bindings changed", body = crate::error::ErrorEnvelope),
        (status = 413, description = "Request body exceeds the 1 MiB API limit", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Nil UUID or invalid generated-schedule publication input", body = crate::error::ErrorEnvelope),
        (status = 502, description = "Current generated schedule cannot be projected within the bounded Google contract", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Generated-schedule Google publication is disabled or unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn enqueue_schedule_publication(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(account_id): Path<Uuid>,
    request: Result<Json<EnqueueScheduleGooglePublicationRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let service = configured_schedule_publication_service(&state, &principal)?;
    let mut request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let approval_capability = std::mem::take(&mut request.approval_capability);
    let accepted = service
        .enqueue_schedule_publication(
            account_id,
            request.preview_id,
            request.collection_id,
            request.expected_schedule_revision_id,
            approval_capability,
        )
        .await
        .map_err(map_service_error)?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

#[utoipa::path(
    get,
    path = "/v1/integrations/google/accounts/{account_id}/schedule-publications/{publication_id}",
    tag = "google_sync",
    description = "Requires a native Device principal with GoogleRead (`google_read`) and ScheduleRead (`schedule_read`) scopes and exact user/workspace tenant binding. Status remains readable when generated-schedule publication writes are disabled.",
    security(("bearer_token" = [])),
    params(
        ("account_id" = Uuid, Path, description = "Connected Google account"),
        ("publication_id" = Uuid, Path, description = "Accepted generated-schedule publication")
    ),
    responses(
        (status = 200, description = "Content-free aggregate delivery status", body = ScheduleGooglePublicationStatus),
        (status = 400, description = "Invalid UUID path parameter", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid DayWeave token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Native Device principal with google_read and schedule_read is required", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Google integration or generated-schedule publication is unavailable for the authenticated tenant", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Nil account or publication UUID", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Google sync is not configured; an existing status remains readable when only the write gate is disabled", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn schedule_publication_status(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((account_id, publication_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ScheduleGooglePublicationStatus>, ApiError> {
    let service = configured_schedule_publication_service(&state, &principal)?;
    service
        .schedule_publication_status(account_id, publication_id)
        .await
        .map(Json)
        .map_err(map_service_error)
}

fn configured_schedule_publication_service<'a>(
    state: &'a AppState,
    principal: &Principal,
) -> Result<&'a Arc<GoogleSyncService>, ApiError> {
    require_schedule_publication_device(principal)?;
    let service = configured_service(state)?;
    require_schedule_publication_scope(principal, service.scope())?;
    Ok(service)
}

fn require_schedule_publication_device(principal: &Principal) -> Result<(), ApiError> {
    if principal.audience != PrincipalAudience::Device || !principal.has_scope(Scope::ScheduleRead)
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

fn require_schedule_publication_scope(
    principal: &Principal,
    scope: OAuthScope,
) -> Result<(), ApiError> {
    if principal.workspace_id != Some(scope.workspace_id)
        || principal.user_id != Some(scope.user_id)
    {
        return Err(ApiError::not_found("Google schedule publication"));
    }
    Ok(())
}

fn configured_service(state: &AppState) -> Result<&Arc<GoogleSyncService>, ApiError> {
    state
        .google_sync
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Google sync is not configured"))
}

async fn add_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn map_service_error(error: GoogleSyncServiceError) -> ApiError {
    match error {
        GoogleSyncServiceError::InvalidRequest => ApiError::validation("request is invalid"),
        GoogleSyncServiceError::MissingReadScope => {
            ApiError::conflict("Google read authorization is required")
        }
        GoogleSyncServiceError::MissingWriteScope => {
            ApiError::conflict("Google write authorization is required for this collection role")
        }
        GoogleSyncServiceError::InvalidOutboundItem | GoogleSyncServiceError::MissingFirmBlock => {
            ApiError::validation(error.to_string())
        }
        GoogleSyncServiceError::DeleteRequiresTrash
        | GoogleSyncServiceError::ProviderIdentityUnresolved
        | GoogleSyncServiceError::ExternalPublicationDisabled
        | GoogleSyncServiceError::InvalidApprovalCapability
        | GoogleSyncServiceError::OutboundPolicyDenied => ApiError::conflict(error.to_string()),
        GoogleSyncServiceError::SchedulePublicationDisabled => {
            ApiError::unavailable("Generated-schedule Google publication is disabled")
        }
        GoogleSyncServiceError::Repository(repository) => map_repository_error(&repository),
        GoogleSyncServiceError::OAuth(oauth) => map_oauth_error(&oauth),
        GoogleSyncServiceError::Item(ItemServiceError::Repository(
            ItemRepositoryError::NotFound(_),
        )) => ApiError::not_found("item"),
        GoogleSyncServiceError::Item(ItemServiceError::Repository(
            ItemRepositoryError::RevisionConflict { expected, actual },
        )) => revision_conflict(expected, actual),
        GoogleSyncServiceError::Google(
            dayweave_google::GoogleError::RateLimited { .. }
            | dayweave_google::GoogleError::Temporary { .. }
            | dayweave_google::GoogleError::Transport(_),
        )
        | GoogleSyncServiceError::DispatchPreparationTimeout => {
            ApiError::unavailable("Google is temporarily unavailable")
        }
        GoogleSyncServiceError::Google(dayweave_google::GoogleError::Unauthorized) => {
            ApiError::conflict("Google authorization must be renewed")
        }
        GoogleSyncServiceError::Google(_)
        | GoogleSyncServiceError::ProviderLimitExceeded
        | GoogleSyncServiceError::ProviderProtocol => {
            ApiError::bad_gateway("Google sync request failed")
        }
        GoogleSyncServiceError::CursorCorrupt
        | GoogleSyncServiceError::OutboundPayloadCorrupt
        | GoogleSyncServiceError::Crypto(_)
        | GoogleSyncServiceError::Randomness
        | GoogleSyncServiceError::Item(_)
        | GoogleSyncServiceError::Internal => ApiError::internal(),
    }
}

fn map_repository_error(error: &GoogleSyncRepositoryError) -> ApiError {
    match error {
        GoogleSyncRepositoryError::AccountNotFound => ApiError::not_found("Google account"),
        GoogleSyncRepositoryError::CollectionNotFound => ApiError::not_found("Google collection"),
        GoogleSyncRepositoryError::ScheduleRevisionNotFound => {
            ApiError::not_found("schedule revision")
        }
        GoogleSyncRepositoryError::SchedulePublicationNotFound => {
            ApiError::not_found("generated-schedule publication")
        }
        GoogleSyncRepositoryError::ItemNotFound => ApiError::not_found("item"),
        GoogleSyncRepositoryError::RevisionConflict { expected, actual } => {
            revision_conflict(*expected, *actual)
        }
        GoogleSyncRepositoryError::ScheduleRevisionConflict { expected, actual } => {
            ApiError::conflict("schedule revision was changed by another publication").with_details(
                json!({
                    "expected_schedule_revision_id": expected,
                    "actual_schedule_revision_id": actual,
                }),
            )
        }
        GoogleSyncRepositoryError::InvalidSchedulePublication => {
            ApiError::conflict("generated-schedule publication is invalid")
        }
        GoogleSyncRepositoryError::PreviewLimitExceeded => {
            ApiError::rate_limited("too many active generated-schedule publication previews")
        }
        GoogleSyncRepositoryError::SchedulePublicationPreviewTooLarge => {
            ApiError::bad_gateway("generated-schedule publication preview is too large")
        }
        GoogleSyncRepositoryError::InvalidCollectionRole => {
            ApiError::validation("collection does not permit that sync role")
        }
        GoogleSyncRepositoryError::CollectionDeleted => {
            ApiError::conflict("provider collection was deleted")
        }
        GoogleSyncRepositoryError::CollectionNotWritable => {
            ApiError::conflict("collection must be selected with writable role")
        }
        GoogleSyncRepositoryError::WriteScopeMissing => {
            ApiError::conflict("Google authorization does not include the required write scope")
        }
        GoogleSyncRepositoryError::ReadScopeMissing => {
            ApiError::conflict("Google authorization does not include the required read scope")
        }
        GoogleSyncRepositoryError::ConditionalWriteUnavailable => ApiError::conflict(
            "Google resource must be reconciled before another conditional write",
        ),
        GoogleSyncRepositoryError::ExternalMutationForbidden => ApiError::conflict(
            "only DayWeave-owned provider records can be changed by this endpoint",
        ),
        GoogleSyncRepositoryError::ApprovalInvalid => {
            ApiError::conflict("outbound preview or approval capability is invalid")
        }
        GoogleSyncRepositoryError::ApprovalExpired => {
            ApiError::conflict("outbound preview or approval capability expired")
        }
        GoogleSyncRepositoryError::ApprovalAlreadyIssued => {
            ApiError::conflict("outbound preview was already approved")
        }
        GoogleSyncRepositoryError::ItemExecutionActive => ApiError::conflict(
            "an active execution session must close before Google can close the canonical item",
        ),
        GoogleSyncRepositoryError::ClaimLost
        | GoogleSyncRepositoryError::CursorConflict
        | GoogleSyncRepositoryError::InvalidProjectionBatch
        | GoogleSyncRepositoryError::IdentityRootMismatch
        | GoogleSyncRepositoryError::Internal => ApiError::internal(),
    }
}

fn map_oauth_error(error: &GoogleOAuthServiceError) -> ApiError {
    match error {
        GoogleOAuthServiceError::Repository(GoogleOAuthRepositoryError::AccountNotFound) => {
            ApiError::not_found("Google account")
        }
        GoogleOAuthServiceError::Repository(GoogleOAuthRepositoryError::AccountStateConflict) => {
            ApiError::conflict("Google account is not active for sync")
        }
        GoogleOAuthServiceError::IntegrationTimeout => {
            ApiError::unavailable("Google authorization is temporarily unavailable")
        }
        _ => ApiError::internal(),
    }
}

fn revision_conflict(expected: u64, actual: u64) -> ApiError {
    ApiError::conflict("resource was changed by another request").with_details(json!({
        "expected_revision": expected,
        "actual_revision": actual,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(
        audience: PrincipalAudience,
        scopes: Vec<Scope>,
        workspace_id: Option<Uuid>,
        user_id: Option<Uuid>,
    ) -> Principal {
        Principal {
            subject: "test-principal".to_owned(),
            scopes,
            audience,
            workspace_id,
            user_id,
            credential_id: Some(Uuid::new_v4()),
            allowed_origins: Vec::new(),
        }
    }

    fn rejection_status(result: Result<(), ApiError>) -> StatusCode {
        result
            .expect_err("principal must be rejected")
            .into_response()
            .status()
    }

    #[test]
    fn schedule_publication_requests_deny_unknown_fields() {
        let collection_id = Uuid::new_v4();
        let schedule_revision_id = Uuid::new_v4();
        let preview_id = Uuid::new_v4();

        let preview = json!({
            "collection_id": collection_id,
            "expected_schedule_revision_id": schedule_revision_id,
        });
        assert!(
            serde_json::from_value::<PreviewScheduleGooglePublicationRequest>(preview.clone())
                .is_ok()
        );
        let mut preview_with_extra = preview;
        preview_with_extra["unreviewed"] = json!(true);
        assert!(
            serde_json::from_value::<PreviewScheduleGooglePublicationRequest>(preview_with_extra)
                .is_err()
        );

        let approve = json!({ "expected_preview_hash": "00".repeat(32) });
        assert!(
            serde_json::from_value::<ApproveScheduleGooglePublicationRequest>(approve.clone())
                .is_ok()
        );
        let mut approve_with_extra = approve;
        approve_with_extra["approval_capability"] = json!("caller-chosen");
        assert!(
            serde_json::from_value::<ApproveScheduleGooglePublicationRequest>(approve_with_extra)
                .is_err()
        );

        let enqueue = json!({
            "preview_id": preview_id,
            "collection_id": collection_id,
            "expected_schedule_revision_id": schedule_revision_id,
            "approval_capability": "dw_gsa1_secret",
        });
        assert!(
            serde_json::from_value::<EnqueueScheduleGooglePublicationRequest>(enqueue.clone())
                .is_ok()
        );
        let mut enqueue_with_extra = enqueue;
        enqueue_with_extra["expected_preview_hash"] = json!("00".repeat(32));
        assert!(
            serde_json::from_value::<EnqueueScheduleGooglePublicationRequest>(enqueue_with_extra)
                .is_err()
        );
    }

    #[test]
    fn schedule_publication_principal_guard_requires_matching_native_device() {
        let scope = OAuthScope {
            workspace_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
        };
        let matching_device = principal(
            PrincipalAudience::Device,
            vec![Scope::GoogleWrite, Scope::ScheduleRead],
            Some(scope.workspace_id),
            Some(scope.user_id),
        );
        assert!(require_schedule_publication_device(&matching_device).is_ok());
        assert!(require_schedule_publication_scope(&matching_device, scope).is_ok());

        let no_schedule_read = principal(
            PrincipalAudience::Device,
            vec![Scope::GoogleWrite],
            Some(scope.workspace_id),
            Some(scope.user_id),
        );
        assert_eq!(
            rejection_status(require_schedule_publication_device(&no_schedule_read)),
            StatusCode::FORBIDDEN
        );

        for audience in [
            PrincipalAudience::Legacy,
            PrincipalAudience::Mcp,
            PrincipalAudience::McpOAuth,
        ] {
            let non_device = principal(
                audience,
                vec![Scope::GoogleWrite, Scope::GoogleRead, Scope::ScheduleRead],
                Some(scope.workspace_id),
                Some(scope.user_id),
            );
            assert_eq!(
                rejection_status(require_schedule_publication_device(&non_device)),
                StatusCode::FORBIDDEN
            );
        }

        for (workspace_id, user_id) in [
            (Some(Uuid::new_v4()), Some(scope.user_id)),
            (Some(scope.workspace_id), Some(Uuid::new_v4())),
            (None, Some(scope.user_id)),
            (Some(scope.workspace_id), None),
        ] {
            let wrong_tenant = principal(
                PrincipalAudience::Device,
                vec![Scope::GoogleRead, Scope::ScheduleRead],
                workspace_id,
                user_id,
            );
            assert!(require_schedule_publication_device(&wrong_tenant).is_ok());
            assert_eq!(
                rejection_status(require_schedule_publication_scope(&wrong_tenant, scope)),
                StatusCode::NOT_FOUND
            );
        }
    }

    #[tokio::test]
    async fn schedule_publication_errors_use_public_contract_statuses() {
        assert_eq!(
            map_service_error(GoogleSyncServiceError::SchedulePublicationDisabled)
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            map_repository_error(&GoogleSyncRepositoryError::ScheduleRevisionNotFound)
                .into_response()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            map_repository_error(&GoogleSyncRepositoryError::SchedulePublicationNotFound)
                .into_response()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            map_repository_error(&GoogleSyncRepositoryError::PreviewLimitExceeded)
                .into_response()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let oversized =
            map_repository_error(&GoogleSyncRepositoryError::SchedulePublicationPreviewTooLarge)
                .into_response();
        assert_eq!(oversized.status(), StatusCode::BAD_GATEWAY);
        let oversized_body = axum::body::to_bytes(oversized.into_body(), 1024)
            .await
            .expect("bounded error body");
        let oversized_body: serde_json::Value =
            serde_json::from_slice(&oversized_body).expect("JSON error envelope");
        assert_eq!(oversized_body["error"]["code"], "bad_gateway");
        assert_eq!(
            map_repository_error(&GoogleSyncRepositoryError::ScheduleRevisionConflict {
                expected: Uuid::new_v4(),
                actual: Uuid::new_v4(),
            })
            .into_response()
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            map_repository_error(&GoogleSyncRepositoryError::InvalidSchedulePublication)
                .into_response()
                .status(),
            StatusCode::CONFLICT
        );
    }
}
