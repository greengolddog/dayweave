use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    routing::post,
};

use crate::{AppState, error::ApiError};

use super::{
    ComposeScheduleError, ComposeScheduleRequest, ComposeScheduleResult, compose_canonical_schedule,
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/schedule/preview", post(preview_schedule))
}

#[utoipa::path(
    post,
    path = "/v1/schedule/preview",
    tag = "schedule",
    security(("bearer_token" = [])),
    request_body = ComposeScheduleRequest,
    responses(
        (status = 200, description = "Deterministic side-effect-free schedule preview", body = ComposeScheduleResult),
        (status = 400, description = "Malformed preview JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid horizon, timezone, bounds, metadata, or scheduling input", body = crate::error::ErrorEnvelope),
        (status = 500, description = "Canonical item storage or encoding failure", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn preview_schedule(
    State(state): State<AppState>,
    request: Result<Json<ComposeScheduleRequest>, JsonRejection>,
) -> Result<Json<ComposeScheduleResult>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let result = compose_canonical_schedule(&state.items, request)
        .await
        .map_err(|error| map_compose_error(&error))?;
    Ok(Json(result))
}

fn map_compose_error(error: &ComposeScheduleError) -> ApiError {
    if error.is_client_error() {
        ApiError::validation(error.to_string())
    } else {
        ApiError::internal()
    }
}
