//! Owner-device access to complete, potentially sensitive occurrence manifests.
use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    body::to_bytes,
    extract::{
        Path, Query, Request, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use super::{
    ROUTINE_PLANNING_WITNESS_BYTES, RoutineOccurrenceCommand, RoutineOccurrenceError,
    RoutineOccurrenceSnapshot, RoutinePlanningWitnessError, RoutinePlanningWitnessRequest,
    RoutinePlanningWitnessResponse, validate_occurrence_lookup,
};
use crate::{
    AppState,
    auth::{Principal, PrincipalAudience, Scope},
    error::ApiError,
    persistence::{
        PostgresRoutineOccurrenceRepository, RoutineOccurrenceMutation, RoutineOccurrencePage,
    },
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/routine-occurrences", get(list_occurrences))
        .route("/routine-occurrences/delta", get(occurrence_delta))
        .route("/routine-occurrences/lookup", get(lookup_occurrence))
        .route(
            "/routine-occurrences/planning-witness",
            post(planning_witness),
        )
        .route("/routine-occurrences/{occurrence_id}", get(get_occurrence))
        .route(
            "/routine-occurrences/{occurrence_id}/members/{item_id}",
            put(put_member),
        )
}

#[utoipa::path(post, path = "/v1/routine-occurrences/planning-witness", tag = "schedule",
    description = "Read-only owner-Device qualification of exact current-source inputs for helper v2; requires items_read and schedule_simulate. A qualified response is neither publication nor execution authority and does not advance the client's terminal checkpoint.",
    security(("bearer_token" = [])), request_body = RoutinePlanningWitnessRequest,
    responses((status = 200, body = RoutinePlanningWitnessResponse),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 401, body = crate::error::ErrorEnvelope),
        (status = 403, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 415, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn planning_witness(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> Result<Response, ApiError> {
    require_device(&principal, Scope::ItemsRead)?;
    require_device(&principal, Scope::ScheduleSimulate)?;
    if principal.credential_id.is_none_or(|id| id.is_nil()) {
        return Err(ApiError::forbidden());
    }
    if request.uri().query().is_some() {
        return Err(ApiError::routine_occurrence(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "Planning witness selectors belong in the request body",
        ));
    }
    let content_types = request.headers().get_all(header::CONTENT_TYPE);
    if content_types.iter().count() != 1
        || content_types
            .iter()
            .next()
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| {
                !value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("application/json")
            })
    {
        return Err(ApiError::routine_occurrence(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Planning witnesses require application/json",
        ));
    }
    // Explicitly bounded raw extraction preserves duplicate/numeric evidence;
    // the ordinary route-wide body limit is not widened for member mutations.
    let body = to_bytes(request.into_body(), ROUTINE_PLANNING_WITNESS_BYTES)
        .await
        .map_err(|_| map_planning_error(RoutinePlanningWitnessError::TooLarge))?;
    let value =
        dayweave_scheduler_helper::decode_bounded_json(&body).map_err(|error| match error {
            dayweave_scheduler_helper::BoundedJsonError::TooLarge => {
                map_planning_error(RoutinePlanningWitnessError::TooLarge)
            }
            dayweave_scheduler_helper::BoundedJsonError::Invalid => invalid_planning_json(),
        })?;
    let request: RoutinePlanningWitnessRequest =
        serde_json::from_value(value).map_err(|_| invalid_planning_json())?;
    request.validate().map_err(map_planning_error)?;
    // The occurrence repository checks the exact owner binding without reading
    // storage; AppState constructs it from this same scheduling repository.
    repository(&state, &principal)?;
    let scheduling = state
        .scheduling
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Routine planning authority requires PostgreSQL"))?;
    let response = scheduling
        .routine_planning_witness(&request)
        .await
        .map_err(map_planning_error)?;
    response.validate_size().map_err(map_planning_error)?;
    Ok(no_store(Json(response).into_response()))
}

fn invalid_planning_json() -> ApiError {
    ApiError::routine_occurrence(
        StatusCode::BAD_REQUEST,
        "invalid_json",
        "Invalid routine planning JSON",
    )
}

fn map_planning_error(error: RoutinePlanningWitnessError) -> ApiError {
    let (status, code, message) = match error {
        RoutinePlanningWitnessError::Invalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "routine_planning_invalid",
            "The routine planning request is invalid",
        ),
        RoutinePlanningWitnessError::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "routine_planning_too_large",
            "Routine planning evidence exceeds resource bounds",
        ),
        RoutinePlanningWitnessError::SourceChanged => (
            StatusCode::CONFLICT,
            "routine_planning_source_changed",
            "The complete canonical planning snapshot changed",
        ),
        RoutinePlanningWitnessError::CursorChanged => (
            StatusCode::CONFLICT,
            "routine_planning_cursor_changed",
            "A current terminal occurrence checkpoint is required",
        ),
        RoutinePlanningWitnessError::Unavailable => {
            return ApiError::unavailable("Routine planning authority is unavailable");
        }
    };
    ApiError::routine_occurrence(status, code, message)
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct OccurrencePageQuery {
    /// Exact opaque continuation from this same endpoint; terminal list cursors
    /// resume the ordinary delta endpoint.
    cursor: Option<String>,
    /// Whole occurrence records per page (default 50, maximum 100); byte limits
    /// can produce a smaller page without splitting one complete occurrence.
    limit: Option<u16>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(crate) struct OccurrenceLookupQuery {
    /// Canonical recurring Task/Routine root, not the selected leaf member.
    series_item_id: Uuid,
    /// Exact planner UUID-v5 occurrence identity, not the private manifest ID.
    occurrence_id: Uuid,
}

#[utoipa::path(get, path = "/v1/routine-occurrences/lookup", tag = "items",
    description = "Resolve an exact calendar identity to a current private occurrence review; requires an owner-bound device and items_read. Does not admit instances or grant planning authority.",
    security(("bearer_token" = [])), params(OccurrenceLookupQuery),
    responses((status = 200, body = RoutineOccurrenceSnapshot),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 401, body = crate::error::ErrorEnvelope),
        (status = 403, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn lookup_occurrence(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<OccurrenceLookupQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    require_device(&principal, Scope::ItemsRead)?;
    let query = query
        .map_err(|_| {
            ApiError::routine_occurrence(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "Invalid occurrence query",
            )
        })?
        .0;
    validate_occurrence_lookup(query.series_item_id, query.occurrence_id).map_err(map_error)?;
    let snapshot = repository(&state, &principal)?
        .lookup(query.series_item_id, query.occurrence_id)
        .await
        .map_err(map_error)?;
    Ok(no_store(Json(snapshot).into_response()))
}

#[utoipa::path(get, path = "/v1/routine-occurrences", tag = "items",
    description = "Complete private manifests; requires a device credential bound to the workspace owner and items_read. Install current-state pages only after the terminal page.",
    security(("bearer_token" = [])), params(OccurrencePageQuery),
    responses((status = 200, body = RoutineOccurrencePage),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 401, body = crate::error::ErrorEnvelope),
        (status = 403, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn list_occurrences(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<OccurrencePageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    require_device(&principal, Scope::ItemsRead)?;
    let query = page_query(query)?;
    let repository = repository(&state, &principal)?;
    let page = repository
        .list(query.cursor.as_deref(), query.limit.unwrap_or(50))
        .await
        .map_err(map_error)?;
    Ok(no_store(Json(page).into_response()))
}

#[utoipa::path(get, path = "/v1/routine-occurrences/delta", tag = "items",
    description = "Ordered private occurrence changes; requires an owner-bound device and items_read. Intermediate current-state list cursors cannot be used here.",
    security(("bearer_token" = [])), params(OccurrencePageQuery),
    responses((status = 200, body = RoutineOccurrencePage),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 401, body = crate::error::ErrorEnvelope),
        (status = 403, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn occurrence_delta(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<OccurrencePageQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    require_device(&principal, Scope::ItemsRead)?;
    let query = page_query(query)?;
    let repository = repository(&state, &principal)?;
    let page = repository
        .delta(query.cursor.as_deref(), query.limit.unwrap_or(50))
        .await
        .map_err(map_error)?;
    Ok(no_store(Json(page).into_response()))
}

#[utoipa::path(get, path = "/v1/routine-occurrences/{occurrence_id}", tag = "items",
    description = "Exact private occurrence review; requires an owner-bound device and items_read.",
    security(("bearer_token" = [])), params(("occurrence_id" = Uuid, Path)),
    responses((status = 200, body = RoutineOccurrenceSnapshot),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 401, body = crate::error::ErrorEnvelope),
        (status = 403, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn get_occurrence(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, ApiError> {
    require_device(&principal, Scope::ItemsRead)?;
    let id = path.map_err(|_| invalid_path())?.0;
    let snapshot = repository(&state, &principal)?
        .get(id)
        .await
        .map_err(map_error)?;
    Ok(no_store(Json(snapshot).into_response()))
}

#[utoipa::path(put, path = "/v1/routine-occurrences/{occurrence_id}/members/{item_id}", tag = "items",
    description = "Exact occurrence-local command; requires an owner-bound device and items_write plus items_read. The body operation_id owns permanent replay; success includes Idempotency-Replayed matching replayed. No canonical template or execution mutation.",
    security(("bearer_token" = [])), params(("occurrence_id" = Uuid, Path), ("item_id" = Uuid, Path)),
    request_body = RoutineOccurrenceCommand,
    responses((status = 200, body = RoutineOccurrenceMutation),
        (status = 400, body = crate::error::ErrorEnvelope),
        (status = 401, body = crate::error::ErrorEnvelope),
        (status = 403, body = crate::error::ErrorEnvelope),
        (status = 404, body = crate::error::ErrorEnvelope),
        (status = 409, body = crate::error::ErrorEnvelope),
        (status = 413, body = crate::error::ErrorEnvelope),
        (status = 422, body = crate::error::ErrorEnvelope),
        (status = 503, body = crate::error::ErrorEnvelope)))]
pub(crate) async fn put_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    request: Result<Json<RoutineOccurrenceCommand>, JsonRejection>,
) -> Result<Response, ApiError> {
    // Success/replay also returns the full private manifest, so write-only
    // credentials do not acquire read authority through this endpoint.
    require_device(&principal, Scope::ItemsWrite)?;
    require_device(&principal, Scope::ItemsRead)?;
    let (id, member_id) = path.map_err(|_| invalid_path())?.0;
    let command = request
        .map_err(|error| {
            if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::payload_too_large("Occurrence request exceeds the route limit")
            } else {
                ApiError::routine_occurrence(
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "Invalid occurrence JSON",
                )
            }
        })?
        .0;
    // Repository resolves exact receipts before fresh-command validation.
    let mutation = repository(&state, &principal)?
        .put(id, member_id, command, principal.credential_id)
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

fn require_device(principal: &Principal, scope: Scope) -> Result<(), ApiError> {
    if principal.audience != PrincipalAudience::Device
        || !principal.has_scope(scope)
        || principal.workspace_id.is_none_or(|id| id.is_nil())
        || principal.user_id.is_none_or(|id| id.is_nil())
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

fn repository<'a>(
    state: &'a AppState,
    principal: &Principal,
) -> Result<&'a Arc<PostgresRoutineOccurrenceRepository>, ApiError> {
    let repository = state
        .routine_occurrences
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Routine occurrence authority requires PostgreSQL"))?;
    let scope = repository.scope();
    if principal.workspace_id != Some(scope.workspace_id)
        || principal.user_id != Some(scope.user_id)
    {
        return Err(ApiError::not_found("routine occurrence"));
    }
    Ok(repository)
}

fn page_query(
    query: Result<Query<OccurrencePageQuery>, QueryRejection>,
) -> Result<OccurrencePageQuery, ApiError> {
    let query = query
        .map_err(|_| {
            ApiError::routine_occurrence(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "Invalid occurrence query",
            )
        })?
        .0;
    if !(1..=100).contains(&query.limit.unwrap_or(50)) {
        return Err(map_error(RoutineOccurrenceError::Invalid));
    }
    Ok(query)
}

fn invalid_path() -> ApiError {
    ApiError::routine_occurrence(
        StatusCode::BAD_REQUEST,
        "invalid_path",
        "Invalid occurrence path",
    )
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

fn map_error(error: RoutineOccurrenceError) -> ApiError {
    use RoutineOccurrenceError as Error;
    let (status, code, message) = match error {
        Error::Invalid => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "routine_occurrence_invalid",
            "The occurrence command or evidence is invalid",
        ),
        Error::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "routine_occurrence_too_large",
            "The complete occurrence exceeds resource bounds",
        ),
        Error::DefinitionChanged => (
            StatusCode::CONFLICT,
            "routine_occurrence_definition_changed",
            "The recurring definition changed",
        ),
        Error::SourceIneligible => (
            StatusCode::CONFLICT,
            "routine_occurrence_source_ineligible",
            "An occurrence source is no longer eligible",
        ),
        Error::InstanceStale => (
            StatusCode::CONFLICT,
            "routine_occurrence_instance_stale",
            "The occurrence revision changed",
        ),
        Error::MemberStale => (
            StatusCode::CONFLICT,
            "routine_occurrence_member_stale",
            "The occurrence member revision changed",
        ),
        Error::EvidenceStale => (
            StatusCode::CONFLICT,
            "routine_occurrence_evidence_stale",
            "The reviewed occurrence evidence changed",
        ),
        Error::MemberMissing => (
            StatusCode::NOT_FOUND,
            "routine_occurrence_member_missing",
            "The occurrence member was not found",
        ),
        Error::OccurrenceMissing => (
            StatusCode::NOT_FOUND,
            "routine_occurrence_missing",
            "The recurring occurrence was not found",
        ),
        Error::OperationReused => (
            StatusCode::CONFLICT,
            "routine_occurrence_operation_reused",
            "The operation identity belongs to different content",
        ),
        Error::InvalidCursor => (
            StatusCode::CONFLICT,
            "routine_occurrence_invalid_cursor",
            "The occurrence cursor is invalid",
        ),
        Error::LeafRequired => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "routine_occurrence_leaf_required",
            "This action requires an occurrence leaf",
        ),
        Error::ParentRequired => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "routine_occurrence_parent_required",
            "This action requires an occurrence parent",
        ),
        Error::OccurrenceEvidenceRequired => (
            StatusCode::CONFLICT,
            "routine_occurrence_evidence_required",
            "Qualified nested occurrence evidence is required",
        ),
        Error::ExecutionConflict => (
            StatusCode::CONFLICT,
            "routine_occurrence_execution_conflict",
            "An affected occurrence member has live execution",
        ),
        Error::Unavailable => {
            return ApiError::unavailable("Routine occurrence authority is unavailable");
        }
    };
    ApiError::routine_occurrence(status, code, message)
}
