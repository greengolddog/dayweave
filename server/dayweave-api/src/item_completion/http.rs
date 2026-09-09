use super::{
    ItemCompletionCommand, ItemCompletionError, ItemCompletionMutation, ItemCompletionSnapshot,
};
use crate::{AppState, auth::Principal, error::ApiError};
use axum::{
    Extension, Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use uuid::Uuid;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(
        "/items/{item_id}/completion",
        get(get_completion).put(put_completion),
    )
}

#[utoipa::path(get, path = "/v1/items/{item_id}/completion", tag = "items",
    security(("bearer_token" = [])), params(("item_id" = Uuid, Path)),
    responses((status = 200, body = ItemCompletionSnapshot),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn get_completion(
    State(state): State<AppState>,
    Path(item_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let snapshot = state
        .item_completion
        .get(item_id)
        .await
        .map_err(map_error)?;
    Ok(no_store(Json(snapshot).into_response()))
}

#[utoipa::path(put, path = "/v1/items/{item_id}/completion", tag = "items",
    security(("bearer_token" = [])), params(("item_id" = Uuid, Path)),
    request_body = ItemCompletionCommand,
    responses((status = 200, body = ItemCompletionMutation),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn put_completion(
    State(state): State<AppState>,
    Path(item_id): Path<Uuid>,
    Extension(principal): Extension<Principal>,
    request: Result<Json<ItemCompletionCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    // Reopening blocker details are private user content; do not echo parser diagnostics.
    let command = request
        .map_err(|error| {
            if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::payload_too_large("Item completion request exceeds the route limit")
            } else {
                ApiError::item_completion(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "Invalid item completion JSON",
                )
            }
        })?
        .0;
    let mutation = state
        .item_completion
        .put(item_id, command, principal.credential_id)
        .await
        .map_err(map_error)?;
    let replayed = mutation.replayed;
    let mut response = no_store(Json(mutation).into_response());
    response.headers_mut().insert(
        "idempotency-replayed",
        HeaderValue::from_static(if replayed { "true" } else { "false" }),
    );
    Ok(response)
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn map_error(error: ItemCompletionError) -> ApiError {
    let (status, code, message) = match error {
        ItemCompletionError::Invalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "item_completion_invalid",
            "The completion command is invalid",
        ),
        ItemCompletionError::ItemStale => (
            StatusCode::CONFLICT,
            "item_completion_item_stale",
            "The canonical item revision changed",
        ),
        ItemCompletionError::CompletionStale => (
            StatusCode::CONFLICT,
            "item_completion_revision_stale",
            "The independent completion revision changed",
        ),
        ItemCompletionError::ItemMissing => (
            StatusCode::NOT_FOUND,
            "item_completion_item_missing",
            "The canonical item was not found",
        ),
        ItemCompletionError::OperationReused => (
            StatusCode::CONFLICT,
            "item_completion_operation_reused",
            "The operation identity belongs to different content",
        ),
        ItemCompletionError::EvidenceStale => (
            StatusCode::CONFLICT,
            "item_completion_evidence_stale",
            "The reviewed completion evidence changed",
        ),
        ItemCompletionError::ParentRequired => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "item_completion_parent_required",
            "Structural completion requires a parent",
        ),
        ItemCompletionError::ReopeningReviewRequired => (
            StatusCode::CONFLICT,
            "item_completion_reopening_review_required",
            "The parent needs explicit reopening and completion review",
        ),
        ItemCompletionError::OccurrenceEvidenceRequired => (
            StatusCode::CONFLICT,
            "item_completion_occurrence_evidence_required",
            "Qualified recurring occurrence evidence is required",
        ),
        ItemCompletionError::ExecutionConflict => (
            StatusCode::CONFLICT,
            "item_completion_execution_conflict",
            "The item has a live execution session",
        ),
        ItemCompletionError::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "item_completion_too_large",
            "The complete completion forest exceeds resource bounds",
        ),
        ItemCompletionError::Unavailable => {
            return ApiError::unavailable("Item completion authority is unavailable");
        }
    };
    ApiError::item_completion(status, code, message)
}
