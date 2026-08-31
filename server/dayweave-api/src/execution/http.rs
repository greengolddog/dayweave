use std::convert::Infallible;

use axum::{
    Json, Router,
    extract::{
        Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use utoipa::{IntoParams, ToSchema};

use crate::{
    AppState,
    error::ApiError,
    items::{ItemRepositoryError, ItemServiceError},
};

use super::{
    DeferAssessment, DeferAssessmentRequest, ExecutionCommand, ExecutionDomainError,
    ExecutionIdempotencyKey, ExecutionMutation, ExecutionRepositoryError, ExecutionServiceError,
    ExecutionSession, ExecutionSnapshot,
    invalidation::{ExecutionInvalidationOpenError, ExecutionInvalidationSignal},
};

const DEFAULT_HISTORY_LIMIT: usize = 50;
const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const REPLAY_HEADER: &str = "idempotency-replayed";
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
const EVENT_STREAM_MEDIA_TYPE: &str = "text/event-stream";
const INVALIDATION_EVENT: &str = "execution-invalidation";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/execution", get(get_execution))
        .route("/execution/stream", get(execution_stream))
        .route("/execution/commands", post(apply_execution_command))
        .route("/execution/defer-assessments", post(assess_defer))
        .route("/execution/history", get(execution_history))
}

#[utoipa::path(
    get,
    path = "/v1/execution/stream",
    tag = "execution",
    security(("bearer_token" = [])),
    params(
        ("Accept" = String, Header, description = "Must be exactly text/event-stream"),
        ("Last-Event-ID" = Option<String>, Header, description = "Last applied execution revision as canonical unsigned decimal; omitted means 0")
    ),
    responses(
        (status = 200, description = "Content-free execution revision invalidations", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Malformed Last-Event-ID", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Credential lacks execution_read", body = crate::error::ErrorEnvelope),
        (status = 406, description = "Accept is not exactly text/event-stream", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Last-Event-ID is ahead of the authoritative execution revision", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Stream capacity or durable execution state is unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn execution_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_event_stream_accept(&headers)?;
    let cursor = parse_last_event_id(&headers)?;
    let stream = state
        .execution
        .invalidation_stream(cursor)
        .await
        .map_err(|error| map_invalidation_open_error(&error))?
        .into_stream()
        .map(|signal| {
            let event = match signal {
                ExecutionInvalidationSignal::Revision(revision) => Event::default()
                    .id(revision.to_string())
                    .event(INVALIDATION_EVENT)
                    .data(format!(r#"{{"revision":{revision}}}"#)),
                ExecutionInvalidationSignal::Heartbeat => Event::default().comment("heartbeat"),
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

fn map_invalidation_open_error(error: &ExecutionInvalidationOpenError) -> ApiError {
    match error {
        ExecutionInvalidationOpenError::Capacity => {
            ApiError::unavailable("execution stream capacity is temporarily exhausted")
        }
        ExecutionInvalidationOpenError::CursorAhead { cursor, head } => {
            ApiError::conflict("execution stream cursor is ahead of authoritative state")
                .with_details(json!({ "cursor_revision": cursor, "head_revision": head }))
        }
        ExecutionInvalidationOpenError::Repository(_) => {
            ApiError::unavailable("execution stream cannot read authoritative state")
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionCommandRequest {
    pub expected_revision: u64,
    pub command: ExecutionCommand,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionHistoryQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ExecutionSnapshotEnvelope {
    pub execution: ExecutionSnapshot,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ExecutionMutationEnvelope {
    pub mutation: ExecutionMutation,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DeferAssessmentEnvelope {
    pub assessment: DeferAssessment,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ExecutionHistoryEnvelope {
    pub sessions: Vec<ExecutionSession>,
    pub next_offset: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/v1/execution",
    tag = "execution",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "Canonical cross-device execution lease", body = ExecutionSnapshotEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn get_execution(
    State(state): State<AppState>,
) -> Result<Json<ExecutionSnapshotEnvelope>, ApiError> {
    let execution = state
        .execution
        .snapshot()
        .await
        .map_err(map_execution_error)?;
    Ok(Json(ExecutionSnapshotEnvelope { execution }))
}

#[utoipa::path(
    post,
    path = "/v1/execution/commands",
    tag = "execution",
    security(("bearer_token" = [])),
    request_body = ExecutionCommandRequest,
    responses(
        (status = 200, description = "Execution command applied or replayed", body = ExecutionMutationEnvelope),
        (status = 400, description = "Malformed command JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Item or session not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision, active lease, exhausted index space, or idempotency conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid execution command", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn apply_execution_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<ExecutionCommandRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let key = execution_idempotency(&headers, &request)?;
    let mutation = state
        .execution
        .command(request.expected_revision, request.command, key)
        .await
        .map_err(map_execution_error)?;
    let replayed = mutation.replayed;
    let mut response =
        (StatusCode::OK, Json(ExecutionMutationEnvelope { mutation })).into_response();
    response.headers_mut().insert(
        REPLAY_HEADER,
        HeaderValue::from_static(if replayed { "true" } else { "false" }),
    );
    Ok(response)
}

#[utoipa::path(
    post,
    path = "/v1/execution/defer-assessments",
    tag = "execution",
    security(("bearer_token" = [])),
    request_body = DeferAssessmentRequest,
    responses(
        (status = 200, description = "Exact content-free assessment for a paused session defer", body = DeferAssessmentEnvelope),
        (status = 400, description = "Malformed assessment JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Execution session not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale execution, schedule, item, Calendar, or target evidence", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid defer target", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Durable assessment support is unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn assess_defer(
    State(state): State<AppState>,
    request: Result<Json<DeferAssessmentRequest>, JsonRejection>,
) -> Result<Json<DeferAssessmentEnvelope>, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let assessment = state
        .execution
        .assess_defer(request)
        .await
        .map_err(map_execution_error)?;
    Ok(Json(DeferAssessmentEnvelope { assessment }))
}

#[utoipa::path(
    get,
    path = "/v1/execution/history",
    tag = "execution",
    security(("bearer_token" = [])),
    params(ExecutionHistoryQuery),
    responses(
        (status = 200, description = "Newest-first execution history", body = ExecutionHistoryEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid history limit", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn execution_history(
    State(state): State<AppState>,
    query: Result<Query<ExecutionHistoryQuery>, QueryRejection>,
) -> Result<Json<ExecutionHistoryEnvelope>, ApiError> {
    let query = query
        .map(|Query(query)| query)
        .map_err(|_| ApiError::validation("query parameters are invalid"))?;
    let page = state
        .execution
        .history_page(
            query.limit.unwrap_or(DEFAULT_HISTORY_LIMIT),
            query.offset.unwrap_or(0),
        )
        .await
        .map_err(map_execution_error)?;
    Ok(Json(ExecutionHistoryEnvelope {
        sessions: page.sessions,
        next_offset: page.next_offset,
    }))
}

fn execution_idempotency(
    headers: &HeaderMap,
    request: &ExecutionCommandRequest,
) -> Result<ExecutionIdempotencyKey, ApiError> {
    let key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::validation("Idempotency-Key header is required"))?;
    let mut digest = Sha256::new();
    digest.update(b"execution.command\0");
    digest.update(serde_json::to_vec(request).map_err(|_| ApiError::internal())?);
    Ok(ExecutionIdempotencyKey {
        key: key.to_owned(),
        fingerprint: digest.finalize().into(),
    })
}

fn map_execution_error(error: ExecutionServiceError) -> ApiError {
    match error {
        ExecutionServiceError::Domain(error)
        | ExecutionServiceError::Repository(ExecutionRepositoryError::InvalidCommand(error)) => {
            map_domain_error(&error)
        }
        ExecutionServiceError::Item(ItemServiceError::Repository(
            ItemRepositoryError::NotFound(_),
        )) => ApiError::not_found("item"),
        ExecutionServiceError::ItemRevisionConflict { expected, actual } => {
            ApiError::conflict("item was changed before execution started").with_details(json!({
                "expected_revision": expected,
                "actual_revision": actual,
            }))
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
            expected,
            actual,
        }) => ApiError::conflict("execution state was changed by another device")
            .with_details(json!({ "expected_revision": expected, "actual_revision": actual })),
        ExecutionServiceError::Repository(ExecutionRepositoryError::SessionNotFound(id)) => {
            ApiError::not_found("execution session").with_details(json!({ "session_id": id }))
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DuplicateSession(id)) => {
            ApiError::conflict("execution session already exists")
                .with_details(json!({ "session_id": id }))
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::ActiveSessionConflict) => {
            ApiError::conflict("another item is already active or paused")
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::IdempotencyConflict) => {
            ApiError::conflict("Idempotency-Key was already used for different content")
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::ItemRevisionConflict) => {
            ApiError::conflict("item changed while execution was starting")
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::ItemNotExecutable)
        | ExecutionServiceError::ItemNotExecutable => {
            ApiError::conflict("only an active leaf item can be executed")
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale) => {
            ApiError::execution_schedule_stale(
                "the execution slot is not startable from the current published schedule",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::IndexExhausted) => {
            ApiError::execution_index_exhausted(
                "no additional execution session index can be allocated",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferDurationConflict) => {
            ApiError::execution_defer_duration_conflict(
                "the deferred move window must exactly match the unfinished planned duration",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferRequiresPause) => {
            ApiError::execution_defer_requires_pause(
                "pause the current execution before choosing a later time",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferAssessmentUnavailable) => {
            ApiError::unavailable(
                "durable defer assessment requires PostgreSQL and a fresh published schedule",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferAssessmentStale) => {
            ApiError::execution_defer_assessment_stale(
                "the defer assessment is missing, expired, or no longer matches current state",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferApprovalRequired) => {
            ApiError::execution_defer_approval_required(
                "the current defer assessment requires exact user approval",
            )
        }
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferApprovalInvalid) => {
            ApiError::execution_defer_approval_invalid(
                "the supplied defer approval does not exactly match the current assessment",
            )
        }
        ExecutionServiceError::InvalidIdempotencyKey => {
            ApiError::validation("Idempotency-Key must be 8-128 URL-safe ASCII characters")
        }
        ExecutionServiceError::InvalidRevision => {
            ApiError::validation("expected_revision is outside the supported range")
        }
        ExecutionServiceError::InvalidHistoryLimit => {
            ApiError::validation("limit must be between 1 and 100")
        }
        ExecutionServiceError::InvalidHistoryOffset => {
            ApiError::validation("offset is outside the supported range")
        }
        ExecutionServiceError::Item(_)
        | ExecutionServiceError::Repository(ExecutionRepositoryError::Internal)
        | ExecutionServiceError::Internal => ApiError::internal(),
    }
}

fn map_domain_error(error: &ExecutionDomainError) -> ApiError {
    match error {
        ExecutionDomainError::InvalidTransition => {
            ApiError::conflict("command does not match the active execution state")
        }
        _ => ApiError::validation(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    #[tokio::test]
    async fn index_exhaustion_is_a_stable_detail_free_conflict() {
        let response = map_execution_error(ExecutionServiceError::Repository(
            ExecutionRepositoryError::IndexExhausted,
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read error envelope");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("decode error envelope");
        assert_eq!(body["error"]["code"], "execution_index_exhausted");
        assert!(body["error"].get("details").is_none());
    }

    #[tokio::test]
    async fn defer_duration_conflict_is_stable_and_detail_free() {
        let response = map_execution_error(ExecutionServiceError::Repository(
            ExecutionRepositoryError::DeferDurationConflict,
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read error envelope");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("decode error envelope");
        assert_eq!(body["error"]["code"], "execution_defer_duration_conflict");
        assert!(body["error"].get("details").is_none());
    }

    #[tokio::test]
    async fn changed_assessment_evidence_has_its_own_stable_recovery_code() {
        let response = map_execution_error(ExecutionServiceError::Repository(
            ExecutionRepositoryError::DeferAssessmentStale,
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read error envelope");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("decode error envelope");
        assert_eq!(body["error"]["code"], "execution_defer_assessment_stale");
        assert!(body["error"].get("details").is_none());
    }
}
