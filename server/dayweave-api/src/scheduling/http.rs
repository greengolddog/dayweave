use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    routing::post,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppState, auth::Principal, error::ApiError};

use super::{
    ComposeScheduleError, ComposeScheduleRequest, ComposeScheduleResult, PublishScheduleSpec,
    ScheduleAccess, SchedulePublication, SchedulePublicationError, compose_canonical_schedule,
    postgres::decode_prefixed_sha256,
};

pub const SCHEDULE_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/schedule/preview", post(preview_schedule))
        .route("/schedule/publish", post(publish_schedule))
        .layer(DefaultBodyLimit::max(SCHEDULE_BODY_LIMIT_BYTES))
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishScheduleRequest {
    pub idempotency_key: Uuid,
    pub expected_input_digest: String,
    pub schedule: ComposeScheduleRequest,
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
        (status = 413, description = "Schedule request exceeds 16 MiB", body = crate::error::ErrorEnvelope),
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

#[utoipa::path(
    post,
    path = "/v1/schedule/publish",
    tag = "schedule",
    security(("bearer_token" = [])),
    request_body = PublishScheduleRequest,
    responses(
        (status = 200, description = "Exact schedule publication or durable idempotent replay; inspect replayed", body = SchedulePublication),
        (status = 400, description = "Malformed publication JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing schedule_publish or principal scope mismatch", body = crate::error::ErrorEnvelope),
        (status = 409, description = "schedule_publication_stale or schedule_publication_idempotency_conflict", body = crate::error::ErrorEnvelope),
        (status = 413, description = "Schedule request exceeds 16 MiB", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid digest, horizon, timezone, bounds, metadata, or scheduling input", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Durable schedule publication is not configured", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn publish_schedule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Result<Json<PublishScheduleRequest>, JsonRejection>,
) -> Result<Json<SchedulePublication>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let expected_input_digest =
        decode_prefixed_sha256(&request.expected_input_digest).ok_or_else(|| {
            ApiError::validation("expected_input_digest must be canonical sha256 hex")
        })?;
    let repository = state
        .scheduling
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Durable schedule publication is not configured"))?;
    let access = ScheduleAccess {
        subject: principal.subject,
        include_sensitive: false,
        workspace_id: principal.workspace_id,
        user_id: principal.user_id,
    };
    let request_hash = publication_request_hash(&request)
        .map_err(|_| ApiError::validation("Schedule publication request cannot be encoded"))?;
    if let Some(receipt) = repository
        .publication_receipt(&access, request.idempotency_key, &request_hash)
        .await
        .map_err(map_publication_error)?
    {
        return Ok(Json(receipt));
    }

    let timezone_name = request.schedule.timezone_name.clone();
    let result = compose_canonical_schedule(&state.items, request.schedule)
        .await
        .map_err(|error| map_compose_error(&error))?;
    if result.input_digest != request.expected_input_digest {
        return Err(ApiError::schedule_publication_stale(
            "Schedule preview is stale; preview again before publishing",
        ));
    }
    let publication = repository
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: request.idempotency_key,
                request_hash,
                input_digest: expected_input_digest,
                timezone_name,
                result,
                published_at: state.clock.now(),
            },
        )
        .await
        .map_err(map_publication_error)?;
    Ok(Json(publication))
}

fn publication_request_hash(
    request: &PublishScheduleRequest,
) -> Result<[u8; 32], serde_json::Error> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        domain: &'static str,
        expected_input_digest: &'a str,
        schedule: &'a ComposeScheduleRequest,
    }
    let bytes = serde_json::to_vec(&Fingerprint {
        domain: "dayweave.schedule-publication-request.v1",
        expected_input_digest: &request.expected_input_digest,
        schedule: &request.schedule,
    })?;
    Ok(Sha256::digest(bytes).into())
}

fn map_publication_error(error: SchedulePublicationError) -> ApiError {
    match error {
        SchedulePublicationError::AccessDenied => ApiError::forbidden(),
        SchedulePublicationError::IdempotencyConflict => {
            ApiError::schedule_publication_idempotency_conflict(
                "idempotency_key was already used for different publication content",
            )
        }
        SchedulePublicationError::StaleComposition => ApiError::schedule_publication_stale(
            "Canonical items changed during publication; preview again",
        ),
        SchedulePublicationError::InvalidPayload => {
            ApiError::validation("Schedule publication payload is invalid")
        }
        SchedulePublicationError::Unavailable => {
            ApiError::unavailable("Durable schedule publication is temporarily unavailable")
        }
    }
}

fn map_compose_error(error: &ComposeScheduleError) -> ApiError {
    if error.is_client_error() {
        ApiError::validation(error.to_string())
    } else {
        ApiError::internal()
    }
}
