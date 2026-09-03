use std::convert::Infallible;

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppState, auth::Principal, error::ApiError};

use super::{
    ComposeScheduleError, ComposeScheduleRequest, ComposeScheduleResult, CurrentPublishedSchedule,
    ManualPlacementApproval, PublishScheduleSpec, RetainedManualPlacementCatalog, ScheduleAccess,
    ScheduleInvalidationOpenError, ScheduleInvalidationSignal, SchedulePublication,
    SchedulePublicationError, SchedulingPortError, compose_canonical_schedule,
    compose_canonical_schedule_unfenced, postgres::decode_prefixed_sha256,
};

pub const SCHEDULE_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
const EVENT_STREAM_MEDIA_TYPE: &str = "text/event-stream";
const INVALIDATION_EVENT: &str = "schedule-invalidation";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/schedule/current", get(get_current_schedule))
        .route("/schedule/stream", get(schedule_stream))
        .route("/schedule/preview", post(preview_schedule))
        .route("/schedule/publish", post(publish_schedule))
        .route(
            "/schedule/manual-placements",
            get(list_retained_manual_placements),
        )
        .layer(DefaultBodyLimit::max(SCHEDULE_BODY_LIMIT_BYTES))
}

#[utoipa::path(
    get,
    path = "/v1/schedule/current",
    tag = "schedule",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Exact current immutable publication for a trusted native replica", body = CurrentPublishedSchedule),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Credential lacks schedule_read", body = crate::error::ErrorEnvelope),
        (status = 404, description = "No schedule has been published", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Current schedule predates the supported durable snapshot schema", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Durable schedule storage is unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn get_current_schedule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Response, ApiError> {
    let repository = state
        .scheduling
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Durable schedule storage is not configured"))?;
    let access = ScheduleAccess {
        subject: principal.subject,
        include_sensitive: true,
        workspace_id: principal.workspace_id,
        user_id: principal.user_id,
    };
    let current = match repository.current_native_schedule(&access).await {
        Ok(current) => current,
        Err(error) => {
            let mut response = map_schedule_read_error(error).into_response();
            apply_current_schedule_cache_headers(&mut response);
            return Ok(response);
        }
    };
    let etag = HeaderValue::from_str(&format!(r#""{}""#, current.revision.revision))
        .map_err(|_| ApiError::unavailable("Published schedule revision is invalid"))?;
    let mut response = Json(current).into_response();
    apply_current_schedule_cache_headers(&mut response);
    response.headers_mut().insert(header::ETAG, etag);
    Ok(response)
}

fn apply_current_schedule_cache_headers(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

#[utoipa::path(
    get,
    path = "/v1/schedule/stream",
    tag = "schedule",
    security(("bearer_token" = [])),
    params(
        ("Accept" = String, Header, description = "Must be exactly text/event-stream"),
        ("Last-Event-ID" = Option<String>, Header, description = "Last installed published schedule revision as canonical unsigned decimal; omitted means 0")
    ),
    responses(
        (status = 200, description = "Content-free published schedule revision invalidations", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Malformed Last-Event-ID", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Credential lacks schedule_read", body = crate::error::ErrorEnvelope),
        (status = 406, description = "Accept is not exactly text/event-stream", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Last-Event-ID is ahead of the authoritative published revision", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Stream capacity or durable schedule state is unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn schedule_stream(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_event_stream_accept(&headers)?;
    let cursor = parse_last_event_id(&headers)?;
    let repository = state
        .scheduling
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Durable schedule storage is not configured"))?;
    let access = ScheduleAccess {
        subject: principal.subject,
        include_sensitive: false,
        workspace_id: principal.workspace_id,
        user_id: principal.user_id,
    };
    let stream = repository
        .invalidation_stream(&access, cursor)
        .await
        .map_err(|error| map_invalidation_open_error(&error))?
        .into_stream()
        .map(|signal| {
            let event = match signal {
                ScheduleInvalidationSignal::Revision(revision) => Event::default()
                    .id(revision.to_string())
                    .event(INVALIDATION_EVENT)
                    .data(format!(r#"{{"revision":{revision}}}"#)),
                ScheduleInvalidationSignal::Heartbeat => Event::default().comment("heartbeat"),
            };
            Ok::<Event, Infallible>(event)
        });
    let mut response = Sse::new(stream).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(response)
}

fn require_event_stream_accept(headers: &HeaderMap) -> Result<(), ApiError> {
    let mut values = headers.get_all(header::ACCEPT).iter();
    let accepted = values
        .next()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case(EVENT_STREAM_MEDIA_TYPE));
    if !accepted || values.next().is_some() {
        return Err(ApiError::not_acceptable(
            "Accept must be exactly text/event-stream",
        ));
    }
    Ok(())
}

fn parse_last_event_id(headers: &HeaderMap) -> Result<u64, ApiError> {
    let name = HeaderName::from_static(LAST_EVENT_ID_HEADER);
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(0);
    };
    if values.next().is_some() {
        return Err(invalid_last_event_id());
    }
    let value = value.to_str().map_err(|_| invalid_last_event_id())?;
    let canonical = value == "0"
        || (value.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && !value.starts_with('0')
            && value.bytes().all(|byte| byte.is_ascii_digit()));
    if !canonical {
        return Err(invalid_last_event_id());
    }
    value.parse().map_err(|_| invalid_last_event_id())
}

fn invalid_last_event_id() -> ApiError {
    ApiError::bad_request("Last-Event-ID must be canonical unsigned decimal")
}

fn map_invalidation_open_error(error: &ScheduleInvalidationOpenError) -> ApiError {
    match error {
        ScheduleInvalidationOpenError::AccessDenied => ApiError::forbidden(),
        ScheduleInvalidationOpenError::Capacity => {
            ApiError::unavailable("schedule stream capacity is temporarily exhausted")
        }
        ScheduleInvalidationOpenError::CursorAhead { cursor, head } => {
            ApiError::conflict("schedule stream cursor is ahead of authoritative state")
                .with_details(serde_json::json!({
                    "cursor_revision": cursor,
                    "head_revision": head,
                }))
        }
        ScheduleInvalidationOpenError::Repository(_) => {
            ApiError::unavailable("schedule stream cannot read authoritative state")
        }
    }
}

fn map_schedule_read_error(error: SchedulingPortError) -> ApiError {
    match error {
        SchedulingPortError::NotFound => ApiError::not_found("Published schedule"),
        SchedulingPortError::RepublishRequired => ApiError::conflict(
            "Published schedule must be recomposed before this client can install it",
        ),
        SchedulingPortError::RevisionConflict { .. } => {
            ApiError::conflict("Published schedule revision changed; retry")
        }
        SchedulingPortError::InvalidQuery(message) => ApiError::validation(message),
        SchedulingPortError::Unavailable(_) => {
            ApiError::unavailable("Durable schedule storage is temporarily unavailable")
        }
    }
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
        (status = 422, description = "Invalid horizon, timezone, bounds, metadata, or scheduling input; scheduler preflight exhaustion uses scheduler_resource_limit", body = crate::error::ErrorEnvelope),
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
        (status = 422, description = "Invalid digest, horizon, timezone, bounds, metadata, or scheduling input; scheduler preflight exhaustion uses scheduler_resource_limit", body = crate::error::ErrorEnvelope),
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
        ComposeScheduleError::SchedulerResourceLimit => ApiError::scheduler_resource_limit(
            "Schedule preview exceeds the bounded scheduler work budget",
        ),
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

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    #[tokio::test]
    async fn scheduler_preflight_limit_has_a_stable_client_error() {
        let response =
            map_compose_error(&ComposeScheduleError::SchedulerResourceLimit).into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        let body = to_bytes(response.into_body(), 1_024)
            .await
            .expect("bounded error envelope");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON error envelope");
        assert_eq!(body["error"]["code"], "scheduler_resource_limit");
        assert_eq!(
            body["error"]["message"],
            "Schedule preview exceeds the bounded scheduler work budget"
        );
        assert!(body["error"].get("details").is_none());
    }
}
