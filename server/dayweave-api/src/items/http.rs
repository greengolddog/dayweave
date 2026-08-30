use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
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
use uuid::Uuid;

use crate::{AppState, error::ApiError};

use super::{
    DeltaChange, IdempotencyKey, Item, ItemQuery, ItemRepositoryError, ItemServiceError, NewItem,
    ReplaceItem,
};

const DEFAULT_ITEM_LIMIT: usize = 100;
const MAX_ITEM_LIMIT: usize = 200;
const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const REPLAY_HEADER: &str = "idempotency-replayed";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/items", get(list_items).post(create_item))
        .route("/items/delta", get(item_delta))
        .route(
            "/items/{id}",
            get(get_item).put(replace_item).delete(delete_item),
        )
        .route("/items/{id}/restore", post(restore_item))
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
