use std::sync::Arc;

use axum::{
    Json, Router,
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
    error::ApiError,
    google_oauth::{GoogleOAuthRepositoryError, GoogleOAuthServiceError},
    items::{ItemRepositoryError, ItemServiceError},
};

use super::{
    GoogleCalendarPolicy, GoogleOutboundAccepted, GoogleOutboundApproval, GoogleOutboundPreview,
    GoogleSyncCollection, GoogleSyncRefreshAccepted, GoogleSyncRepositoryError, GoogleSyncRole,
    GoogleSyncService, GoogleSyncServiceError, GoogleSyncStatus, OutboundOperation,
    OutboundRequest,
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
        GoogleSyncRepositoryError::ItemNotFound => ApiError::not_found("item"),
        GoogleSyncRepositoryError::RevisionConflict { expected, actual } => {
            revision_conflict(*expected, *actual)
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
