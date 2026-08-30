use axum::{
    Json, Router,
    extract::{
        Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
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
    ExecutionCommand, ExecutionDomainError, ExecutionIdempotencyKey, ExecutionMutation,
    ExecutionRepositoryError, ExecutionServiceError, ExecutionSession, ExecutionSnapshot,
};

const DEFAULT_HISTORY_LIMIT: usize = 50;
const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const REPLAY_HEADER: &str = "idempotency-replayed";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/execution", get(get_execution))
        .route("/execution/commands", post(apply_execution_command))
        .route("/execution/history", get(execution_history))
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
        (status = 409, description = "Stale revision, active lease, or idempotency conflict", body = crate::error::ErrorEnvelope),
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
