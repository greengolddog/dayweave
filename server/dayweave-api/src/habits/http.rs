use std::convert::Infallible;

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderName, HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
    routing::{get, post, put},
};
use chrono::NaiveDate;
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    auth::{Principal, Scope},
    error::ApiError,
};

use super::{
    HabitAnalytics, HabitAnalyticsBucket, HabitDeltaChange, HabitIdempotencyKey,
    HabitMissedReconcileCommand, HabitMissedReconcileResult, HabitMissedResolution,
    HabitMissedResolveCommand, HabitOccurrence, HabitOutcomeCommand, HabitPause,
    HabitPauseResumeCommand, HabitPauseStartCommand, HabitRepositoryError, HabitServiceError,
    invalidation::HabitInvalidationSignal,
    service::{
        DEFAULT_HABIT_PAGE_LIMIT, DEFAULT_HABIT_RECONCILE_LIMIT, MAX_HABIT_PAGE_LIMIT,
        MAX_HABIT_RECONCILE_LIMIT,
    },
};

const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const REPLAY_HEADER: &str = "idempotency-replayed";
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
const EVENT_STREAM_MEDIA_TYPE: &str = "text/event-stream";
const INVALIDATION_EVENT: &str = "habit-invalidation";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/habits/occurrences/delta", get(habit_delta))
        .route("/habits/stream", get(habit_stream))
        .route("/habits/missed/reconcile", post(reconcile_missed))
        .route("/habits/{habit_id}/occurrences", get(list_occurrences))
        .route(
            "/habits/{habit_id}/occurrences/{occurrence_id}",
            put(put_outcome),
        )
        .route(
            "/habits/{habit_id}/occurrences/{evidence_id}/missed-resolution",
            put(resolve_missed),
        )
        .route("/habits/{habit_id}/pauses", post(start_pause))
        .route(
            "/habits/{habit_id}/pauses/{pause_id}/resume",
            post(resume_pause),
        )
        .route("/habits/{habit_id}/analytics", get(habit_analytics))
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct OccurrenceListQuery {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct HabitDeltaQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct HabitAnalyticsQuery {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub bucket: HabitAnalyticsBucket,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct HabitMissedReconcileQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitOccurrenceEnvelope {
    pub occurrence: HabitOccurrence,
    pub replayed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitOccurrenceListEnvelope {
    pub occurrences: Vec<HabitOccurrence>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitPauseEnvelope {
    pub pause: HabitPause,
    pub replayed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitDeltaEnvelope {
    pub changes: Vec<HabitDeltaChange>,
    pub next_cursor: String,
    pub has_more: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitAnalyticsEnvelope {
    pub analytics: HabitAnalytics,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitMissedReconcileEnvelope {
    #[serde(flatten)]
    pub result: HabitMissedReconcileResult,
    pub replayed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HabitMissedResolutionEnvelope {
    pub resolution: HabitMissedResolution,
    pub replayed: bool,
}

#[utoipa::path(
    post,
    path = "/v1/habits/missed/reconcile",
    tag = "habits",
    security(("bearer_token" = [])),
    params(HabitMissedReconcileQuery, ("Idempotency-Key" = String, Header)),
    request_body = HabitMissedReconcileCommand,
    responses(
        (status = 200, description = "Bounded server-clock missed occurrence reconciliation", body = HabitMissedReconcileEnvelope),
        (status = 400, description = "Malformed limit, idempotency key, or body", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Credential lacks items_write or items_read", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid operation or limit", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Reconciliation receipt capacity is temporarily exhausted", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn reconcile_missed(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<Principal>,
    headers: HeaderMap,
    query: Result<Query<HabitMissedReconcileQuery>, QueryRejection>,
    request: Result<Json<HabitMissedReconcileCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    // The REST scope matrix enforces items_write. This workspace-wide response also exposes
    // occurrence resolution metadata, so it requires the corresponding read authority.
    if !principal.has_scope(Scope::ItemsRead) {
        return Err(ApiError::forbidden());
    }
    let query = strict_query(query)?;
    let command = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let limit = query.limit.unwrap_or(DEFAULT_HABIT_RECONCILE_LIMIT);
    if !(1..=MAX_HABIT_RECONCILE_LIMIT).contains(&limit) {
        return Err(ApiError::validation(format!(
            "limit must be between 1 and {MAX_HABIT_RECONCILE_LIMIT}"
        )));
    }
    let mutation = state
        .habits
        .reconcile_missed(
            command,
            limit,
            idempotency(&headers, principal.credential_id)?,
        )
        .await
        .map_err(map_habit_error)?;
    Ok(mutation_response(
        Json(HabitMissedReconcileEnvelope {
            result: mutation.value,
            replayed: mutation.replayed,
        })
        .into_response(),
        mutation.replayed,
    ))
}

#[utoipa::path(
    put,
    path = "/v1/habits/{habit_id}/occurrences/{evidence_id}/missed-resolution",
    tag = "habits",
    security(("bearer_token" = [])),
    params(
        ("habit_id" = Uuid, Path),
        ("evidence_id" = Uuid, Path, description = "Server-issued occurrence evidence id"),
        ("Idempotency-Key" = String, Header)
    ),
    request_body = HabitMissedResolveCommand,
    responses(
        (status = 200, description = "Ask-policy decision resolved or exactly replayed", body = HabitMissedResolutionEnvelope),
        (status = 404, description = "Habit or missed resolution not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision, resolved decision, or idempotency conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid resolution command", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn resolve_missed(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<Principal>,
    Path((habit_id, evidence_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    request: Result<Json<HabitMissedResolveCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    let command = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let mutation = state
        .habits
        .resolve_missed(
            habit_id,
            evidence_id,
            command,
            idempotency(&headers, principal.credential_id)?,
        )
        .await
        .map_err(map_habit_error)?;
    Ok(mutation_response(
        Json(HabitMissedResolutionEnvelope {
            resolution: mutation.value,
            replayed: mutation.replayed,
        })
        .into_response(),
        mutation.replayed,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/habits/{habit_id}/occurrences",
    tag = "habits",
    security(("bearer_token" = [])),
    params(("habit_id" = Uuid, Path), OccurrenceListQuery),
    responses(
        (status = 200, description = "Authoritative habit occurrences", body = HabitOccurrenceListEnvelope),
        (status = 400, description = "Malformed query or cursor", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Habit not found", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid date range or limit", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn list_occurrences(
    State(state): State<AppState>,
    Path(habit_id): Path<Uuid>,
    query: Result<Query<OccurrenceListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let query = strict_query(query)?;
    let page = state
        .habits
        .list_occurrences(
            habit_id,
            query.start_date,
            query.end_date,
            query.cursor.as_deref(),
            bounded_limit(query.limit)?,
        )
        .await
        .map_err(map_habit_error)?;
    Ok(no_store(
        Json(HabitOccurrenceListEnvelope {
            occurrences: page.occurrences,
            next_cursor: page.next_cursor,
            has_more: page.has_more,
        })
        .into_response(),
    ))
}

#[utoipa::path(
    put,
    path = "/v1/habits/{habit_id}/occurrences/{occurrence_id}",
    tag = "habits",
    security(("bearer_token" = [])),
    params(
        ("habit_id" = Uuid, Path),
        ("occurrence_id" = Uuid, Path, description = "Server-issued occurrence evidence id"),
        ("Idempotency-Key" = String, Header)
    ),
    request_body = HabitOutcomeCommand,
    responses(
        (status = 200, description = "Outcome created, corrected, or exactly replayed", body = HabitOccurrenceEnvelope),
        (status = 404, description = "Habit or authoritative occurrence evidence not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision or idempotency conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid outcome", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn put_outcome(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<Principal>,
    Path((habit_id, occurrence_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    request: Result<Json<HabitOutcomeCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    let command = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let key = idempotency(&headers, principal.credential_id)?;
    let mutation = state
        .habits
        .put_outcome(habit_id, occurrence_id, command, key)
        .await
        .map_err(map_habit_error)?;
    Ok(mutation_response(
        Json(HabitOccurrenceEnvelope {
            occurrence: mutation.value,
            replayed: mutation.replayed,
        })
        .into_response(),
        mutation.replayed,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/habits/{habit_id}/pauses",
    tag = "habits",
    security(("bearer_token" = [])),
    params(("habit_id" = Uuid, Path), ("Idempotency-Key" = String, Header)),
    request_body = HabitPauseStartCommand,
    responses(
        (status = 200, description = "Open pause created or exactly replayed", body = HabitPauseEnvelope),
        (status = 409, description = "An open pause exists or idempotency conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid pause", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn start_pause(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<Principal>,
    Path(habit_id): Path<Uuid>,
    headers: HeaderMap,
    request: Result<Json<HabitPauseStartCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    let command = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let mutation = state
        .habits
        .create_pause(
            habit_id,
            command,
            idempotency(&headers, principal.credential_id)?,
        )
        .await
        .map_err(map_habit_error)?;
    Ok(mutation_response(
        Json(HabitPauseEnvelope {
            pause: mutation.value,
            replayed: mutation.replayed,
        })
        .into_response(),
        mutation.replayed,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/habits/{habit_id}/pauses/{pause_id}/resume",
    tag = "habits",
    security(("bearer_token" = [])),
    params(("habit_id" = Uuid, Path), ("pause_id" = Uuid, Path), ("Idempotency-Key" = String, Header)),
    request_body = HabitPauseResumeCommand,
    responses(
        (status = 200, description = "Pause closed or exactly replayed", body = HabitPauseEnvelope),
        (status = 404, description = "Habit or pause not found", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision, closed pause, or idempotency conflict", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn resume_pause(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<Principal>,
    Path((habit_id, pause_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    request: Result<Json<HabitPauseResumeCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    let command = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let mutation = state
        .habits
        .resume_pause(
            habit_id,
            pause_id,
            command,
            idempotency(&headers, principal.credential_id)?,
        )
        .await
        .map_err(map_habit_error)?;
    Ok(mutation_response(
        Json(HabitPauseEnvelope {
            pause: mutation.value,
            replayed: mutation.replayed,
        })
        .into_response(),
        mutation.replayed,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/habits/occurrences/delta",
    tag = "habits",
    security(("bearer_token" = [])),
    params(HabitDeltaQuery),
    responses((status = 200, description = "Bounded habit ledger delta", body = HabitDeltaEnvelope))
)]
pub(crate) async fn habit_delta(
    State(state): State<AppState>,
    query: Result<Query<HabitDeltaQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let query = strict_query(query)?;
    let page = state
        .habits
        .delta(query.cursor.as_deref(), bounded_limit(query.limit)?)
        .await
        .map_err(map_habit_error)?;
    Ok(no_store(
        Json(HabitDeltaEnvelope {
            changes: page.changes,
            next_cursor: page.next_cursor,
            has_more: page.has_more,
        })
        .into_response(),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/habits/{habit_id}/analytics",
    tag = "habits",
    security(("bearer_token" = [])),
    params(("habit_id" = Uuid, Path), HabitAnalyticsQuery),
    responses((status = 200, description = "Deterministic private habit analytics", body = HabitAnalyticsEnvelope))
)]
pub(crate) async fn habit_analytics(
    State(state): State<AppState>,
    Path(habit_id): Path<Uuid>,
    query: Result<Query<HabitAnalyticsQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let query = strict_query(query)?;
    let analytics = state
        .habits
        .analytics(habit_id, query.start_date, query.end_date, query.bucket)
        .await
        .map_err(map_habit_error)?;
    Ok(no_store(
        Json(HabitAnalyticsEnvelope { analytics }).into_response(),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/habits/stream",
    tag = "habits",
    security(("bearer_token" = [])),
    params(("Accept" = String, Header), ("Last-Event-ID" = Option<String>, Header)),
    responses(
        (status = 200, description = "Content-free habit cursor invalidations", body = String, content_type = "text/event-stream"),
        (status = 406, description = "Accept is not exactly text/event-stream", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn habit_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_event_stream_accept(&headers)?;
    let cursor = single_header(&headers, LAST_EVENT_ID_HEADER, false)?;
    let stream = state
        .habits
        .invalidation_stream(cursor)
        .await
        .map_err(map_habit_error)?
        .into_stream()
        .map(|signal| {
            let event = match signal {
                HabitInvalidationSignal::Cursor(cursor) => Event::default()
                    .id(cursor.clone())
                    .event(INVALIDATION_EVENT)
                    .data(format!(r#"{{"cursor":"{cursor}"}}"#)),
                HabitInvalidationSignal::Heartbeat => Event::default().comment("heartbeat"),
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

fn strict_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(value)| value)
        .map_err(|error| ApiError::bad_request(error.body_text()))
}

fn bounded_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    let value = limit.unwrap_or(DEFAULT_HABIT_PAGE_LIMIT);
    if !(1..=MAX_HABIT_PAGE_LIMIT).contains(&value) {
        return Err(ApiError::validation(format!(
            "limit must be between 1 and {MAX_HABIT_PAGE_LIMIT}"
        )));
    }
    Ok(value)
}

fn idempotency(
    headers: &HeaderMap,
    actor_session_id: Option<Uuid>,
) -> Result<HabitIdempotencyKey, ApiError> {
    let key = single_header(headers, IDEMPOTENCY_HEADER, true)?
        .ok_or_else(|| ApiError::bad_request("Idempotency-Key is required"))?;
    Ok(HabitIdempotencyKey {
        key: key.to_owned(),
        actor_session_id,
    })
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    required: bool,
) -> Result<Option<&'a str>, ApiError> {
    let mut values = headers.get_all(HeaderName::from_static(name)).iter();
    let Some(value) = values.next() else {
        return if required {
            Err(ApiError::bad_request(format!("{name} is required")))
        } else {
            Ok(None)
        };
    };
    if values.next().is_some() {
        return Err(ApiError::bad_request(format!(
            "{name} must appear exactly once"
        )));
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| ApiError::bad_request(format!("{name} is invalid")))
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

fn mutation_response(mut response: Response, replayed: bool) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static(REPLAY_HEADER),
        HeaderValue::from_static(if replayed { "true" } else { "false" }),
    );
    no_store(response)
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

fn map_habit_error(error: HabitServiceError) -> ApiError {
    match error {
        HabitServiceError::Repository(repository) => map_repository_error(repository),
        HabitServiceError::Domain(error) => ApiError::validation(error.to_string()),
        HabitServiceError::InvalidIdentifier
        | HabitServiceError::InvalidCreateRevision
        | HabitServiceError::InvalidCorrectionRevision
        | HabitServiceError::InvalidMutationTime
        | HabitServiceError::InvalidDateRange
        | HabitServiceError::InvalidLimit
        | HabitServiceError::InvalidReconcileLimit => ApiError::validation(error.to_string()),
        HabitServiceError::InvalidIdempotencyKey | HabitServiceError::InvalidCursor => {
            ApiError::bad_request(error.to_string())
        }
        HabitServiceError::CursorAhead => ApiError::conflict(error.to_string()),
        HabitServiceError::StreamCapacity => ApiError::unavailable(error.to_string()),
        HabitServiceError::AnalyticsTooLarge => ApiError::payload_too_large(error.to_string()),
        HabitServiceError::Items(_)
        | HabitServiceError::TooManyItems
        | HabitServiceError::Internal => ApiError::internal(),
    }
}

fn map_repository_error(error: HabitRepositoryError) -> ApiError {
    match error {
        HabitRepositoryError::HabitNotFound(_)
        | HabitRepositoryError::OccurrenceNotFound(_)
        | HabitRepositoryError::PauseNotFound(_)
        | HabitRepositoryError::MissedResolutionNotFound(_) => {
            ApiError::not_found("habit resource")
        }
        HabitRepositoryError::NotHabit(_) | HabitRepositoryError::TargetUnitMismatch => {
            ApiError::validation(error.to_string())
        }
        HabitRepositoryError::RevisionConflict {
            expected,
            actual,
            current_occurrence,
            current_pause,
        } => ApiError::conflict("habit revision conflict").with_details(json!({
            "expected_revision": expected,
            "actual_revision": actual,
            "current_occurrence": current_occurrence,
            "current_pause": current_pause,
        })),
        HabitRepositoryError::OpenPauseConflict(current) => {
            ApiError::conflict("habit already has an open pause")
                .with_details(json!({"current_pause": current}))
        }
        HabitRepositoryError::PauseAlreadyClosed(current) => {
            ApiError::conflict("habit pause is already closed")
                .with_details(json!({"current_pause": current}))
        }
        HabitRepositoryError::PauseIdentityConflict(current) => {
            ApiError::conflict("habit pause id is already in use")
                .with_details(json!({"current_pause": current}))
        }
        HabitRepositoryError::InvalidPauseInterval => ApiError::validation(error.to_string()),
        HabitRepositoryError::IdempotencyConflict => {
            ApiError::conflict("Idempotency-Key or operation_id was reused for different content")
        }
        HabitRepositoryError::ReconcileReceiptCapacity => {
            ApiError::unavailable("missed-occurrence reconciliation is temporarily rate limited")
        }
        HabitRepositoryError::MissedResolutionAlreadyResolved(current) => {
            ApiError::conflict("missed occurrence decision is already resolved")
                .with_details(json!({"current_resolution": current}))
        }
        HabitRepositoryError::MissedReductionUnavailable => ApiError::conflict(
            "no authoritative future occurrence is available for frequency reduction",
        ),
        HabitRepositoryError::InvalidCursor => ApiError::bad_request("habit cursor is invalid"),
        HabitRepositoryError::EvidenceConflict | HabitRepositoryError::Internal => {
            ApiError::unavailable("habit ledger is temporarily unavailable")
        }
    }
}
