use std::convert::Infallible;

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
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
use uuid::Uuid;

use crate::{AppState, error::ApiError};

use super::{
    DeltaChange, IdempotencyKey, Item, ItemQuery, ItemRepositoryError, ItemServiceError, NewItem,
    ReplaceItem,
    invalidation::{ItemInvalidationOpenError, ItemInvalidationSignal},
};

const DEFAULT_ITEM_LIMIT: usize = 100;
const MAX_ITEM_LIMIT: usize = 200;
const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const REPLAY_HEADER: &str = "idempotency-replayed";
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
const EVENT_STREAM_MEDIA_TYPE: &str = "text/event-stream";
const INVALIDATION_EVENT: &str = "item-invalidation";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/items", get(list_items).post(create_item))
        .route("/items/delta", get(item_delta))
        .route("/items/stream", get(item_stream))
        .route(
            "/items/{id}",
            get(get_item).put(replace_item).delete(delete_item),
        )
        .route("/items/{id}/restore", post(restore_item))
}

#[utoipa::path(
    get,
    path = "/v1/items/stream",
    tag = "items",
    security(("bearer_token" = [])),
    params(
        ("Accept" = String, Header, description = "Must be exactly text/event-stream"),
        ("Last-Event-ID" = Option<String>, Header, description = "Exact opaque cursor from the last durably applied item delta page; omitted means the initial cursor")
    ),
    responses(
        (status = 200, description = "Content-free opaque item cursor invalidations", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Malformed or wrong-workspace Last-Event-ID", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 403, description = "Credential lacks items_read", body = crate::error::ErrorEnvelope),
        (status = 406, description = "Accept is not exactly text/event-stream", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Last-Event-ID is ahead of the authoritative item change head", body = crate::error::ErrorEnvelope),
        (status = 503, description = "Stream capacity or durable item change state is unavailable", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn item_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_event_stream_accept(&headers)?;
    let cursor = parse_last_event_id(&headers)?;
    let stream = state
        .items
        .invalidation_stream(cursor.as_deref())
        .await
        .map_err(|error| map_invalidation_open_error(&error))?
        .into_stream()
        .map(|signal| {
            let event = match signal {
                ItemInvalidationSignal::Cursor(cursor) => Event::default()
                    .id(cursor.clone())
                    .event(INVALIDATION_EVENT)
                    .data(format!(r#"{{"cursor":"{cursor}"}}"#)),
                ItemInvalidationSignal::Heartbeat => Event::default().comment("heartbeat"),
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

fn parse_last_event_id(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let name = HeaderName::from_static(LAST_EVENT_ID_HEADER);
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid_last_event_id());
    }
    value
        .to_str()
        .map(str::to_owned)
        .map(Some)
        .map_err(|_| invalid_last_event_id())
}

fn invalid_last_event_id() -> ApiError {
    ApiError::bad_request("Last-Event-ID must be an exact opaque item delta cursor")
}

fn map_invalidation_open_error(error: &ItemInvalidationOpenError) -> ApiError {
    match error {
        ItemInvalidationOpenError::InvalidCursor => invalid_last_event_id(),
        ItemInvalidationOpenError::Capacity => {
            ApiError::unavailable("item stream capacity is temporarily exhausted")
        }
        ItemInvalidationOpenError::CursorAhead => {
            ApiError::conflict("item stream cursor is ahead of authoritative state")
        }
        ItemInvalidationOpenError::Repository(_) => {
            ApiError::unavailable("item stream cannot read authoritative state")
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct ItemListQuery {
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub include_deleted: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub(crate) struct ItemDeltaQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaceItemRequest {
    pub expected_revision: u64,
    pub item: ReplaceItem,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionRequest {
    pub expected_revision: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ItemEnvelope {
    pub item: Item,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ItemListEnvelope {
    pub items: Vec<Item>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ItemDeltaEnvelope {
    pub changes: Vec<DeltaChange>,
    pub next_cursor: String,
    pub has_more: bool,
}

#[utoipa::path(
    post,
    path = "/v1/items",
    tag = "items",
    security(("bearer_token" = [])),
    request_body = NewItem,
    responses(
        (status = 201, description = "Canonical item created", body = ItemEnvelope),
        (status = 400, description = "Malformed item JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Duplicate item, hierarchy, or idempotency conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid item contract", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn create_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<NewItem>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let idempotency = idempotency(&headers, "items.create", None, &request)?;
    let mutation = state
        .items
        .create(request, idempotency)
        .await
        .map_err(map_item_error)?;
    Ok(mutation_response(
        StatusCode::CREATED,
        mutation.item,
        mutation.replayed,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/items",
    tag = "items",
    security(("bearer_token" = [])),
    params(ItemListQuery),
    responses(
        (status = 200, description = "Canonical items", body = ItemListEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid list query", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn list_items(
    State(state): State<AppState>,
    query: Result<Query<ItemListQuery>, QueryRejection>,
) -> Result<Json<ItemListEnvelope>, ApiError> {
    let query = strict_query(query)?;
    let limit = bounded_limit(query.limit)?;
    let items = state
        .items
        .list(ItemQuery {
            parent_id: query.parent_id,
            include_deleted: query.include_deleted,
            limit,
        })
        .await
        .map_err(map_item_error)?;
    Ok(Json(ItemListEnvelope { items }))
}

#[utoipa::path(
    get,
    path = "/v1/items/delta",
    tag = "items",
    security(("bearer_token" = [])),
    params(ItemDeltaQuery),
    responses(
        (status = 200, description = "Ordered item upserts and tombstones", body = ItemDeltaEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Malformed or unsupported cursor", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn item_delta(
    State(state): State<AppState>,
    query: Result<Query<ItemDeltaQuery>, QueryRejection>,
) -> Result<Json<ItemDeltaEnvelope>, ApiError> {
    let query = strict_query(query)?;
    let limit = bounded_limit(query.limit)?;
    let page = state
        .items
        .delta(query.cursor.as_deref(), limit)
        .await
        .map_err(map_item_error)?;
    Ok(Json(ItemDeltaEnvelope {
        changes: page.changes,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/items/{id}",
    tag = "items",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Item identifier")),
    responses(
        (status = 200, description = "Canonical item", body = ItemEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 404, description = "Item not found", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn get_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ItemEnvelope>, ApiError> {
    let item = state.items.get(id).await.map_err(map_item_error)?;
    Ok(Json(ItemEnvelope { item }))
}

#[utoipa::path(
    put,
    path = "/v1/items/{id}",
    tag = "items",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Item identifier")),
    request_body = ReplaceItemRequest,
    responses(
        (status = 200, description = "Item replaced", body = ItemEnvelope),
        (status = 400, description = "Malformed replacement JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision, hierarchy, or active execution conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid item contract", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn replace_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    request: Result<Json<ReplaceItemRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let idempotency = idempotency(&headers, "items.replace", Some(id), &request)?;
    let mutation = state
        .items
        .replace(id, request.expected_revision, request.item, idempotency)
        .await
        .map_err(map_item_error)?;
    Ok(mutation_response(
        StatusCode::OK,
        mutation.item,
        mutation.replayed,
    ))
}

#[utoipa::path(
    delete,
    path = "/v1/items/{id}",
    tag = "items",
    security(("bearer_token" = [])),
    params(
        ("id" = Uuid, Path, description = "Item identifier"),
        ("expected_revision" = u64, Query, description = "Optimistic revision")
    ),
    responses(
        (status = 200, description = "Item soft-deleted", body = ItemEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision, active children, or active execution conflict", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid revision or idempotency key", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    query: Result<Query<RevisionRequest>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let query = strict_query(query)?;
    let idempotency = idempotency(&headers, "items.delete", Some(id), &query)?;
    let mutation = state
        .items
        .trash(id, query.expected_revision, idempotency)
        .await
        .map_err(map_item_error)?;
    Ok(mutation_response(
        StatusCode::OK,
        mutation.item,
        mutation.replayed,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/items/{id}/restore",
    tag = "items",
    security(("bearer_token" = [])),
    params(("id" = Uuid, Path, description = "Item identifier")),
    request_body = RevisionRequest,
    responses(
        (status = 200, description = "Item restored", body = ItemEnvelope),
        (status = 400, description = "Malformed revision JSON", body = crate::error::ErrorEnvelope),
        (status = 401, description = "Missing or invalid token", body = crate::error::ErrorEnvelope),
        (status = 409, description = "Stale revision or deleted parent", body = crate::error::ErrorEnvelope),
        (status = 422, description = "Invalid revision or idempotency key", body = crate::error::ErrorEnvelope)
    )
)]
pub(crate) async fn restore_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    request: Result<Json<RevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = request
        .map_err(|error| ApiError::from_json_rejection(&error))?
        .0;
    let idempotency = idempotency(&headers, "items.restore", Some(id), &request)?;
    let mutation = state
        .items
        .restore(id, request.expected_revision, idempotency)
        .await
        .map_err(map_item_error)?;
    Ok(mutation_response(
        StatusCode::OK,
        mutation.item,
        mutation.replayed,
    ))
}

fn idempotency<T: Serialize>(
    headers: &HeaderMap,
    operation: &str,
    item_id: Option<Uuid>,
    request: &T,
) -> Result<IdempotencyKey, ApiError> {
    let key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::validation("Idempotency-Key header is required"))?;
    let mut digest = Sha256::new();
    digest.update(operation.as_bytes());
    if let Some(item_id) = item_id {
        digest.update(item_id.as_bytes());
    }
    digest.update(serde_json::to_vec(request).map_err(|_| ApiError::internal())?);
    Ok(IdempotencyKey {
        key: key.to_owned(),
        fingerprint: digest.finalize().into(),
    })
}

fn bounded_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    let limit = limit.unwrap_or(DEFAULT_ITEM_LIMIT);
    if (1..=MAX_ITEM_LIMIT).contains(&limit) {
        Ok(limit)
    } else {
        Err(ApiError::validation(format!(
            "limit must be between 1 and {MAX_ITEM_LIMIT}",
        )))
    }
}

fn strict_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(query)| query)
        .map_err(|_| ApiError::validation("query parameters are invalid"))
}

fn mutation_response(status: StatusCode, item: Item, replayed: bool) -> Response {
    let mut response = (status, Json(ItemEnvelope { item })).into_response();
    response.headers_mut().insert(
        REPLAY_HEADER,
        HeaderValue::from_static(if replayed { "true" } else { "false" }),
    );
    response
}

fn map_item_error(error: ItemServiceError) -> ApiError {
    match error {
        ItemServiceError::Domain(error)
        | ItemServiceError::Repository(ItemRepositoryError::InvalidItem(error)) => {
            ApiError::validation(error.to_string())
        }
        ItemServiceError::Repository(ItemRepositoryError::NotFound(_)) => {
            ApiError::not_found("item")
        }
        ItemServiceError::Repository(ItemRepositoryError::ParentNotFound(id)) => {
            ApiError::validation("parent item was not found")
                .with_details(json!({ "parent_id": id }))
        }
        ItemServiceError::Repository(ItemRepositoryError::BlockedByItemNotFound(id)) => {
            ApiError::validation("blocking item was not found")
                .with_details(json!({ "blocked_by_item_id": id }))
        }
        ItemServiceError::Repository(ItemRepositoryError::RevisionConflict {
            expected,
            actual,
        }) => ApiError::conflict("item was changed by another request").with_details(json!({
            "expected_revision": expected,
            "actual_revision": actual,
        })),
        ItemServiceError::Repository(ItemRepositoryError::Duplicate(_)) => {
            ApiError::conflict("item already exists")
        }
        ItemServiceError::Repository(ItemRepositoryError::IdempotencyConflict) => {
            ApiError::conflict("Idempotency-Key was already used for different content")
        }
        ItemServiceError::Repository(ItemRepositoryError::IdempotencyInProgress) => {
            ApiError::conflict("matching idempotent request is still in progress")
        }
        ItemServiceError::Repository(ItemRepositoryError::ActiveExecutionConflict {
            item_id,
            session_id,
        }) => ApiError::item_execution_active(
            "an active execution session must close before the item can become terminal or trashed",
        )
        .with_details(json!({ "item_id": item_id, "session_id": session_id })),
        ItemServiceError::Repository(ItemRepositoryError::InvalidCursor) => {
            ApiError::validation("delta cursor is invalid")
        }
        ItemServiceError::Repository(
            error @ (ItemRepositoryError::SelfParent
            | ItemRepositoryError::HierarchyCycle
            | ItemRepositoryError::InvalidParentState
            | ItemRepositoryError::NonLeafExecutable
            | ItemRepositoryError::HasChildren
            | ItemRepositoryError::DeletedParent),
        ) => ApiError::conflict(error.to_string()),
        ItemServiceError::InvalidIdempotencyKey => {
            ApiError::validation("Idempotency-Key must be 8-128 URL-safe ASCII characters")
        }
        ItemServiceError::InvalidRevision => {
            ApiError::validation("expected_revision must be positive")
        }
        ItemServiceError::InvalidCursor => ApiError::validation("delta cursor is invalid"),
        ItemServiceError::Repository(ItemRepositoryError::Internal)
        | ItemServiceError::Internal => ApiError::internal(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const LEGACY_CREATE_JSON: &str = r#"{"id":"00000000-0000-0000-0000-000000000001","is_sensitive":false,"kind":"task","status":"planned","title":"Legacy","notes":null,"timezone_name":"UTC","duration_seconds":3600,"deadline_at":"2026-09-03T12:00:00Z","earliest_start_at":null,"recurrence":null,"flexible_constraints":{},"split_policy":{"type":"indivisible"},"importance":1,"urgency":2,"parent_id":null,"sibling_order":0}"#;
    const LEGACY_REPLACE_JSON: &str = r#"{"expected_revision":7,"item":{"is_sensitive":false,"kind":"task","status":"planned","title":"Legacy","notes":null,"timezone_name":"UTC","duration_seconds":3600,"deadline_at":"2026-09-03T12:00:00Z","earliest_start_at":null,"recurrence":null,"flexible_constraints":{},"split_policy":{"type":"indivisible"},"importance":1,"urgency":2,"parent_id":null,"sibling_order":0}}"#;

    #[test]
    fn legacy_item_request_keeps_its_pre_structural_fingerprint() {
        let request: NewItem =
            serde_json::from_str(LEGACY_CREATE_JSON).expect("frozen legacy request");
        let serialized = serde_json::to_vec(&request).expect("serialize request");
        assert_eq!(serialized, LEGACY_CREATE_JSON.as_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            IDEMPOTENCY_HEADER,
            HeaderValue::from_static("legacy-create-key"),
        );
        let actual = idempotency(&headers, "items.create", None, &request)
            .expect("legacy idempotency fingerprint");
        let mut expected = Sha256::new();
        expected.update(b"items.create");
        expected.update(LEGACY_CREATE_JSON.as_bytes());
        assert_eq!(actual.fingerprint, <[u8; 32]>::from(expected.finalize()));
    }

    #[test]
    fn legacy_replace_request_keeps_its_pre_structural_fingerprint() {
        let request: ReplaceItemRequest =
            serde_json::from_str(LEGACY_REPLACE_JSON).expect("frozen legacy replacement");
        let serialized = serde_json::to_vec(&request).expect("serialize replacement");
        assert_eq!(serialized, LEGACY_REPLACE_JSON.as_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            IDEMPOTENCY_HEADER,
            HeaderValue::from_static("legacy-replace-key"),
        );
        let item_id = Uuid::from_u128(1);
        let actual = idempotency(&headers, "items.replace", Some(item_id), &request)
            .expect("legacy replacement fingerprint");
        let mut expected = Sha256::new();
        expected.update(b"items.replace");
        expected.update(item_id.as_bytes());
        expected.update(LEGACY_REPLACE_JSON.as_bytes());
        assert_eq!(actual.fingerprint, <[u8; 32]>::from(expected.finalize()));
    }
}
