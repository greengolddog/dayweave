use super::{ItemProgressCommand, ItemProgressError, ItemProgressMutation, ItemProgressSnapshot};
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
        "/items/{item_id}/progress",
        get(get_progress).put(put_progress),
    )
}

#[utoipa::path(get, path = "/v1/items/{item_id}/progress", tag = "items",
    security(("bearer_token" = [])), params(("item_id" = Uuid, Path)),
    responses((status = 200, body = ItemProgressSnapshot),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn get_progress(
    State(state): State<AppState>,
    Path(item_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let snapshot = state.item_progress.get(item_id).await.map_err(map_error)?;
    Ok(no_store(Json(snapshot).into_response()))
}

#[utoipa::path(put, path = "/v1/items/{item_id}/progress", tag = "items",
    security(("bearer_token" = [])), params(("item_id" = Uuid, Path)),
    request_body = ItemProgressCommand,
    responses((status = 200, body = ItemProgressMutation),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn put_progress(
    State(state): State<AppState>,
    Path(item_id): Path<Uuid>,
    Extension(principal): Extension<Principal>,
    request: Result<Json<ItemProgressCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    // Progress labels are private user content; parser diagnostics must not echo them.
    let command = request
        .map_err(|error| {
            if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::payload_too_large("Item progress request exceeds the route limit")
            } else {
                ApiError::item_progress(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "Invalid item progress JSON",
                )
            }
        })?
        .0;
    let mutation = state
        .item_progress
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

fn map_error(error: ItemProgressError) -> ApiError {
    let (status, code, message) = match error {
        ItemProgressError::Invalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "item_progress_invalid",
            "The progress command is invalid",
        ),
        ItemProgressError::ItemStale => (
            StatusCode::CONFLICT,
            "item_progress_item_stale",
            "The canonical item revision changed",
        ),
        ItemProgressError::ProgressStale => (
            StatusCode::CONFLICT,
            "item_progress_revision_stale",
            "The independent progress revision changed",
        ),
        ItemProgressError::ItemMissing => (
            StatusCode::NOT_FOUND,
            "item_progress_item_missing",
            "The canonical item was not found",
        ),
        ItemProgressError::OperationReused => (
            StatusCode::CONFLICT,
            "item_progress_operation_reused",
            "The operation identity belongs to different content",
        ),
        ItemProgressError::Unavailable => {
            return ApiError::unavailable("Item progress authority is unavailable");
        }
    };
    ApiError::item_progress(status, code, message)
}
