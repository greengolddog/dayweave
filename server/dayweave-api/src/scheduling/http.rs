use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppState, auth::Principal, error::ApiError};

use super::{
    ComposeScheduleError, ComposeScheduleRequest, ComposeScheduleResult, ManualPlacementApproval,
    PublishScheduleSpec, RetainedManualPlacementCatalog, ScheduleAccess, SchedulePublication,
    SchedulePublicationError, compose_canonical_schedule, compose_canonical_schedule_unfenced,
    postgres::decode_prefixed_sha256,
};

pub const SCHEDULE_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/schedule/preview", post(preview_schedule))
        .route("/schedule/publish", post(publish_schedule))
        .route(
            "/schedule/manual-placements",
            get(list_retained_manual_placements),
        )
        .layer(DefaultBodyLimit::max(SCHEDULE_BODY_LIMIT_BYTES))
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishScheduleRequest {
    pub idempotency_key: Uuid,
    pub expected_input_digest: String,
    pub schedule: ComposeScheduleRequest,
    #[serde(default)]
    pub manual_placement_approvals: Vec<ManualPlacementApproval>,
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
        (status = 503, description = "Required Google Calendar projection is incomplete or temporarily unavailable", body = crate::error::ErrorEnvelope),
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
    let result = match state.scheduling.as_deref() {
        Some(projection) => compose_canonical_schedule(&state.items, projection, request).await,
        None => compose_canonical_schedule_unfenced(&state.items, request).await,
    }
    .map_err(|error| map_compose_error(&error))?;
    Ok(Json(result))
}

#[utoipa::path(
    get,
    path = "/v1/schedule/manual-placements",
    tag = "schedule",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Owner-only content-free retained manual placement recovery catalog", body = RetainedManualPlacementCatalog),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Missing schedule_read or principal scope mismatch", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Durable schedule evidence is not configured or temporarily unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn list_retained_manual_placements(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<RetainedManualPlacementCatalog>, ApiError> {
    let repository = state
        .scheduling
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Durable schedule evidence is not configured"))?;
    let access = ScheduleAccess {
        subject: principal.subject,
        include_sensitive: false,
        workspace_id: principal.workspace_id,
        user_id: principal.user_id,
    };
    repository
        .retained_manual_placement_catalog(&access)
        .await
        .map(Json)
        .map_err(map_publication_error)
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
        (status = 503, description = "Durable publication is not configured or Google Calendar projection evidence is temporarily unavailable", body = crate::error::ErrorEnvelope)
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
    let manual_placement_approvals = request.manual_placement_approvals;
    let result = compose_canonical_schedule(&state.items, repository, request.schedule)
        .await
        .map_err(|error| map_publish_compose_error(&error))?;
    if result.input_digest != request.expected_input_digest {
        return Err(ApiError::schedule_publication_stale(
            "Schedule preview is stale; preview again before publishing",
        ));
    }
    validate_manual_placement_approvals(&result, &manual_placement_approvals)?;
    let publication = repository
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: request.idempotency_key,
                request_hash,
                input_digest: expected_input_digest,
                timezone_name,
                manual_placement_approvals,
                result,
                published_at: state.clock.now(),
            },
        )
        .await
        .map_err(map_publication_error)?;
    Ok(Json(publication))
}

fn validate_manual_placement_approvals(
    result: &ComposeScheduleResult,
    approvals: &[ManualPlacementApproval],
) -> Result<(), ApiError> {
    if approvals.len() > dayweave_compose::MAX_MANUAL_PLACEMENTS {
        return Err(ApiError::validation(
            "manual placement approval count exceeds the supported limit",
        ));
    }
    let mut supplied = std::collections::BTreeMap::new();
    for approval in approvals {
        if approval.placement_id.is_nil()
            || decode_prefixed_sha256(&approval.approval_digest).is_none()
            || supplied
                .insert(approval.placement_id, approval.approval_digest.as_str())
                .is_some()
        {
            return Err(ApiError::validation(
                "manual placement approvals must have unique ids and canonical sha256 digests",
            ));
        }
    }
    let required: std::collections::BTreeMap<_, _> = result
        .manual_placement_assessments
        .iter()
        .filter(|assessment| assessment.approval_required)
        .map(|assessment| (assessment.placement_id, assessment.approval_digest.as_str()))
        .collect();
    if supplied != required {
        return Err(ApiError::schedule_publication_stale(
            "Manual placement conflicts changed or were not approved; review the latest preview",
        ));
    }
    Ok(())
}

fn publication_request_hash(
    request: &PublishScheduleRequest,
) -> Result<[u8; 32], serde_json::Error> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        domain: &'static str,
        expected_input_digest: &'a str,
        schedule: &'a ComposeScheduleRequest,
        manual_placement_approvals: &'a [ManualPlacementApproval],
    }
    let bytes = serde_json::to_vec(&Fingerprint {
        domain: "dayweave.schedule-publication-request.v1",
        expected_input_digest: &request.expected_input_digest,
        schedule: &request.schedule,
        manual_placement_approvals: &request.manual_placement_approvals,
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
            "Schedule inputs changed during publication; preview again",
        ),
        SchedulePublicationError::DeferredPlacementRequired => {
            ApiError::schedule_publication_stale(
                "Deferred work requires its exact pinned placement; preview again",
            )
        }
        SchedulePublicationError::InvalidPayload => {
            ApiError::validation("Schedule publication payload is invalid")
        }
        SchedulePublicationError::Unavailable => {
            ApiError::unavailable("Durable schedule publication is temporarily unavailable")
        }
    }
}

fn map_compose_error(error: &ComposeScheduleError) -> ApiError {
    match error {
        ComposeScheduleError::CalendarProjectionIncomplete => ApiError::unavailable(
            "Selected Google Calendar data is not ready for this scheduling horizon; refresh sync and retry",
        ),
        ComposeScheduleError::CalendarProjectionUnavailable => {
            ApiError::unavailable("Google Calendar projection evidence is temporarily unavailable")
        }
        ComposeScheduleError::ExecutionEvidenceChanged => ApiError::unavailable(
            "Execution or published schedule evidence changed during preview; retry",
        ),
        ComposeScheduleError::ExecutionEvidenceUnavailable => {
            ApiError::unavailable("Execution planning evidence is temporarily unavailable")
        }
        _ if error.is_client_error() => ApiError::validation(error.to_string()),
        _ => ApiError::internal(),
    }
}

fn map_publish_compose_error(error: &ComposeScheduleError) -> ApiError {
    if matches!(
        error,
        ComposeScheduleError::CalendarProjectionIncomplete
            | ComposeScheduleError::ExecutionEvidenceChanged
            | ComposeScheduleError::AuthoritativeManualPlacementChanged(_)
    ) {
        return ApiError::schedule_publication_stale(
            "Schedule inputs changed after preview; preview again before publishing",
        );
    }
    map_compose_error(error)
}
