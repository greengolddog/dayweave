use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
};
use chrono::{DateTime, Utc};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    items::{
        IdempotencyContext, IdempotencyKey, InMemoryItemRepository, Item, ItemDeltaPage,
        ItemInvalidationConfig, ItemMutation, ItemQuery, ItemRepository, ItemRepositoryError,
        ItemService, NewItem, ReplaceItem,
    },
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tokio::{sync::Notify, time::timeout};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "item-stream-api-token";
const NORMAL_HEAD: u8 = 0;
const FORCED_HEAD: u8 = 1;
const FAILED_HEAD: u8 = 2;

fn stream_config(
    probe: Duration,
    heartbeat: Duration,
    lifetime: Duration,
    max_connections: usize,
) -> ItemInvalidationConfig {
    ItemInvalidationConfig::new(probe, heartbeat, lifetime, max_connections)
        .expect("bounded stream test configuration")
}

fn test_state() -> AppState {
    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    AppState::new(
        proposals,
        Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
        readiness,
    )
}

fn app_with_repository(
    repository: Arc<dyn ItemRepository>,
    config: ItemInvalidationConfig,
) -> Router {
    let mut state = test_state();
    state.items = Arc::new(
        ItemService::new(repository, Arc::new(SystemClock)).with_invalidation_config(config),
    );
    router(state)
}

fn standard_app() -> Router {
    app_with_repository(
        Arc::new(InMemoryItemRepository::default()),
        stream_config(
            Duration::from_millis(200),
            Duration::from_millis(200),
            Duration::from_secs(2),
            8,
        ),
    )
}

fn request(method: &str, uri: &str, body: Option<Value>, authenticated: bool) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("valid request")
}

fn stream_request(
    authenticated: bool,
    accept: Option<&str>,
    cursor: Option<&str>,
) -> Request<Body> {
    let mut request = request("GET", "/v1/items/stream", None, authenticated);
    if let Some(accept) = accept {
        request.headers_mut().insert(
            header::ACCEPT,
            HeaderValue::from_str(accept).expect("valid Accept test value"),
        );
    }
    if let Some(cursor) = cursor {
        request.headers_mut().insert(
            "last-event-id",
            HeaderValue::from_str(cursor).expect("valid cursor test value"),
        );
    }
    request
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn next_stream_chunk(response: &mut Response<Body>, wait: Duration) -> Option<String> {
    let frame = timeout(wait, response.body_mut().frame())
        .await
        .expect("stream produced or ended before timeout")?;
    let frame = frame.expect("valid stream frame");
    let data = frame.into_data().expect("stream data frame");
    Some(String::from_utf8(data.to_vec()).expect("UTF-8 SSE frame"))
}

fn item_body(id: Uuid, parent_id: Option<Uuid>, title: &str) -> Value {
    json!({
        "id": id,
        "is_sensitive": true,
        "kind": "task",
        "status": "planned",
        "title": title,
        "notes": "SYNTHETIC-PRIVATE-ITEM-STREAM-NOTES",
        "timezone_name": "Europe/Madrid",
        "duration_seconds": 1800,
        "deadline_at": null,
        "earliest_start_at": null,
        "recurrence": null,
        "flexible_constraints": {},
        "split_policy": { "type": "indivisible" },
        "importance": 80,
        "urgency": 70,
        "parent_id": parent_id,
        "sibling_order": 0
    })
}

fn replacement_body(body: &Value, expected_revision: u64) -> Value {
    let mut item = body.clone();
    item.as_object_mut().unwrap().remove("id");
    item["title"] = json!("SYNTHETIC-PRIVATE-ITEM-STREAM-TITLE-REPLACED");
    json!({
        "expected_revision": expected_revision,
        "item": item,
    })
}

async fn create_item(app: &Router, body: Value, key: &'static str) -> Response<Body> {
    app.clone()
        .oneshot({
            let mut request = request("POST", "/v1/items", Some(body), true);
            request
                .headers_mut()
                .insert("idempotency-key", HeaderValue::from_static(key));
            request
        })
        .await
        .expect("item response")
}

async fn delta_cursor(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(request("GET", "/v1/items/delta?limit=200", None, true))
        .await
        .expect("delta response");
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["next_cursor"]
        .as_str()
        .expect("opaque delta cursor")
        .to_owned()
}

fn assert_cursor_frame_is_content_free(frame: &str, cursor: &str, item_id: Uuid) {
    assert_eq!(
        frame,
        format!("id: {cursor}\nevent: item-invalidation\ndata: {{\"cursor\":\"{cursor}\"}}\n\n")
    );
    for forbidden in [
        item_id.to_string(),
        "SYNTHETIC-PRIVATE-ITEM-STREAM-TITLE".to_owned(),
        "SYNTHETIC-PRIVATE-ITEM-STREAM-NOTES".to_owned(),
        "is_sensitive".to_owned(),
        "revision".to_owned(),
    ] {
        assert!(!frame.contains(&forbidden), "SSE leaked {forbidden}");
    }
}

#[tokio::test]
async fn stream_requires_authentication_exact_accept_and_exact_opaque_cursor() {
    let app = standard_app();

    let unauthorized = app
        .clone()
        .oneshot(stream_request(false, Some("text/event-stream"), None))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    for accept in [None, Some("*/*"), Some("text/event-stream, */*")] {
        let response = app
            .clone()
            .oneshot(stream_request(true, accept, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(body_json(response).await["error"]["code"], "not_acceptable");
    }

    let mut duplicate_accept = stream_request(true, Some("text/event-stream"), None);
    duplicate_accept.headers_mut().append(
        header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );
    let duplicate_accept = app.clone().oneshot(duplicate_accept).await.unwrap();
    assert_eq!(duplicate_accept.status(), StatusCode::NOT_ACCEPTABLE);

    let oversized_cursor = "a".repeat(257);
    for cursor in [
        "",
        "not-a-cursor",
        "DWI1",
        "abc=",
        " abc",
        &oversized_cursor,
    ] {
        let response = app
            .clone()
            .oneshot(stream_request(
                true,
                Some("text/event-stream"),
                Some(cursor),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "cursor {cursor:?}"
        );
        assert_eq!(body_json(response).await["error"]["code"], "bad_request");
    }

    let zero_cursor = delta_cursor(&app).await;
    assert!(zero_cursor.is_ascii());
    assert!(zero_cursor.len() <= 256);

    let other_app = standard_app();
    let wrong_scope = delta_cursor(&other_app).await;
    let wrong_scope = app
        .clone()
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&wrong_scope),
        ))
        .await
        .unwrap();
    assert_eq!(wrong_scope.status(), StatusCode::BAD_REQUEST);

    let mut duplicate = stream_request(true, Some("text/event-stream"), Some(&zero_cursor));
    duplicate.headers_mut().append(
        "last-event-id",
        HeaderValue::from_str(&zero_cursor).unwrap(),
    );
    let duplicate = app.clone().oneshot(duplicate).await.unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);

    let accepted = app
        .oneshot(stream_request(
            true,
            Some("TEXT/EVENT-STREAM"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
}

#[tokio::test]
async fn valid_ahead_cursor_conflicts_and_unavailable_head_returns_503() {
    let ahead_repository = Arc::new(ControlledHeadRepository::forced(0));
    let ahead_app = app_with_repository(
        ahead_repository,
        stream_config(
            Duration::from_millis(200),
            Duration::from_millis(200),
            Duration::from_secs(1),
            2,
        ),
    );
    let created = create_item(
        &ahead_app,
        item_body(Uuid::new_v4(), None, "Ahead cursor source"),
        "item-stream-ahead-001",
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let ahead_cursor = delta_cursor(&ahead_app).await;
    let ahead = ahead_app
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&ahead_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(ahead.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(ahead).await["error"]["code"], "conflict");

    let unavailable = app_with_repository(
        Arc::new(ControlledHeadRepository::failed()),
        stream_config(
            Duration::from_millis(200),
            Duration::from_millis(200),
            Duration::from_secs(1),
            2,
        ),
    );
    let unavailable = unavailable
        .oneshot(stream_request(true, Some("text/event-stream"), None))
        .await
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(unavailable).await["error"]["code"],
        "service_unavailable"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live connection proves every direct item command path.
async fn every_direct_mutation_wakes_immediately_while_failure_and_replay_are_silent() {
    let app = standard_app();
    let zero_cursor = delta_cursor(&app).await;
    let mut live = app
        .clone()
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(live.status(), StatusCode::OK);
    assert_eq!(live.headers()[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(live.headers()[header::CACHE_CONTROL], "no-store, no-cache");
    assert_eq!(live.headers()[header::PRAGMA], "no-cache");
    assert_eq!(live.headers()["x-accel-buffering"], "no");

    let failed = create_item(
        &app,
        item_body(
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
            "Failed item must not wake stream",
        ),
        "item-stream-failed-001",
    )
    .await;
    assert_eq!(failed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        timeout(Duration::from_millis(40), live.body_mut().frame())
            .await
            .is_err(),
        "a failed item command must not publish an invalidation"
    );

    let item_id = Uuid::new_v4();
    let body = item_body(item_id, None, "SYNTHETIC-PRIVATE-ITEM-STREAM-TITLE");
    let created = create_item(&app, body.clone(), "item-stream-live-001").await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let head_cursor = delta_cursor(&app).await;
    let frame = next_stream_chunk(&mut live, Duration::from_secs(1))
        .await
        .expect("live invalidation");
    assert_cursor_frame_is_content_free(&frame, &head_cursor, item_id);

    let replay = create_item(&app, body.clone(), "item-stream-live-001").await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert!(
        timeout(Duration::from_millis(40), live.body_mut().frame())
            .await
            .is_err(),
        "a replay with no new durable head must remain silent"
    );

    let replaced = app
        .clone()
        .oneshot({
            let mut request = request(
                "PUT",
                &format!("/v1/items/{item_id}"),
                Some(replacement_body(&body, 1)),
                true,
            );
            request.headers_mut().insert(
                "idempotency-key",
                HeaderValue::from_static("item-stream-replace-001"),
            );
            request
        })
        .await
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::OK);
    let replace_cursor = delta_cursor(&app).await;
    let replace_frame = next_stream_chunk(&mut live, Duration::from_secs(1))
        .await
        .expect("replace invalidation");
    assert_cursor_frame_is_content_free(&replace_frame, &replace_cursor, item_id);

    let trashed = app
        .clone()
        .oneshot({
            let mut request = request(
                "DELETE",
                &format!("/v1/items/{item_id}?expected_revision=2"),
                None,
                true,
            );
            request.headers_mut().insert(
                "idempotency-key",
                HeaderValue::from_static("item-stream-trash-001"),
            );
            request
        })
        .await
        .unwrap();
    assert_eq!(trashed.status(), StatusCode::OK);
    let trash_cursor = delta_cursor(&app).await;
    let trash_frame = next_stream_chunk(&mut live, Duration::from_secs(1))
        .await
        .expect("trash invalidation");
    assert_cursor_frame_is_content_free(&trash_frame, &trash_cursor, item_id);

    let restored = app
        .clone()
        .oneshot({
            let mut request = request(
                "POST",
                &format!("/v1/items/{item_id}/restore"),
                Some(json!({ "expected_revision": 3 })),
                true,
            );
            request.headers_mut().insert(
                "idempotency-key",
                HeaderValue::from_static("item-stream-restore-001"),
            );
            request
        })
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    let restore_cursor = delta_cursor(&app).await;
    let restore_frame = next_stream_chunk(&mut live, Duration::from_secs(1))
        .await
        .expect("restore invalidation");
    assert_cursor_frame_is_content_free(&restore_frame, &restore_cursor, item_id);
}

#[tokio::test]
async fn stream_catches_up_immediately_from_opaque_cursor_or_omitted_cursor() {
    let app = standard_app();
    let zero_cursor = delta_cursor(&app).await;
    let item_id = Uuid::new_v4();
    let created = create_item(
        &app,
        item_body(item_id, None, "SYNTHETIC-PRIVATE-ITEM-STREAM-TITLE"),
        "item-stream-catchup-001",
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let head_cursor = delta_cursor(&app).await;

    let mut resumed = app
        .clone()
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    let resumed_frame = next_stream_chunk(&mut resumed, Duration::from_secs(1))
        .await
        .expect("immediate resumed catch-up");
    assert_cursor_frame_is_content_free(&resumed_frame, &head_cursor, item_id);

    let mut fresh = app
        .oneshot(stream_request(true, Some("text/event-stream"), None))
        .await
        .unwrap();
    let fresh_frame = next_stream_chunk(&mut fresh, Duration::from_secs(1))
        .await
        .expect("immediate fresh-client catch-up");
    assert_cursor_frame_is_content_free(&fresh_frame, &head_cursor, item_id);
}

#[tokio::test]
async fn authoritative_probe_recovers_cross_instance_or_direct_repository_changes() {
    let repository = Arc::new(InMemoryItemRepository::default());
    let app = app_with_repository(
        repository.clone(),
        stream_config(
            Duration::from_millis(10),
            Duration::from_millis(200),
            Duration::from_millis(500),
            2,
        ),
    );
    let zero_cursor = delta_cursor(&app).await;
    let mut response = app
        .clone()
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // A separate service instance shares durable state but has a different
    // process-local wake hub, modeling another server process or direct writer.
    let external = ItemService::new(repository, Arc::new(SystemClock));
    let item_id = Uuid::new_v4();
    external
        .create(
            new_item(item_id, "SYNTHETIC-PRIVATE-ITEM-STREAM-TITLE", None),
            idempotency("item-stream-external-001"),
        )
        .await
        .unwrap();

    let head_cursor = delta_cursor(&app).await;
    let frame = next_stream_chunk(&mut response, Duration::from_millis(200))
        .await
        .expect("probe invalidation");
    assert_cursor_frame_is_content_free(&frame, &head_cursor, item_id);
}

#[tokio::test]
async fn subscribing_before_the_head_read_closes_the_opening_race() {
    let repository = Arc::new(ControlledHeadRepository::blocking_once());
    let app = app_with_repository(
        repository.clone(),
        stream_config(
            Duration::from_millis(200),
            Duration::from_millis(200),
            Duration::from_secs(1),
            2,
        ),
    );
    let zero_cursor = delta_cursor(&app).await;

    let stream_app = app.clone();
    let opening = tokio::spawn(async move {
        stream_app
            .oneshot(stream_request(
                true,
                Some("text/event-stream"),
                Some(&zero_cursor),
            ))
            .await
            .expect("stream response")
    });
    timeout(Duration::from_secs(1), repository.entered.notified())
        .await
        .expect("stream reached authoritative head read");

    let item_id = Uuid::new_v4();
    let created = create_item(
        &app,
        item_body(item_id, None, "SYNTHETIC-PRIVATE-ITEM-STREAM-TITLE"),
        "item-stream-opening-race-001",
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    repository.release.notify_one();

    let mut response = opening.await.expect("opening task");
    assert_eq!(response.status(), StatusCode::OK);
    let head_cursor = delta_cursor(&app).await;
    let frame = next_stream_chunk(&mut response, Duration::from_secs(1))
        .await
        .expect("race-safe invalidation");
    assert_cursor_frame_is_content_free(&frame, &head_cursor, item_id);
}

#[tokio::test]
async fn stream_capacity_heartbeat_and_expiry_are_bounded() {
    let app = app_with_repository(
        Arc::new(InMemoryItemRepository::default()),
        stream_config(
            Duration::from_millis(80),
            Duration::from_millis(10),
            Duration::from_millis(60),
            1,
        ),
    );
    let zero_cursor = delta_cursor(&app).await;
    let mut first = app
        .clone()
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let saturated = app
        .clone()
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(saturated.status(), StatusCode::SERVICE_UNAVAILABLE);

    let heartbeat = next_stream_chunk(&mut first, Duration::from_millis(40))
        .await
        .expect("heartbeat comment");
    assert_eq!(heartbeat, ": heartbeat\n\n");
    timeout(Duration::from_millis(150), async {
        while let Some(frame) = first.body_mut().frame().await {
            let frame = frame.expect("valid heartbeat frame");
            let data = frame.into_data().expect("heartbeat data frame");
            assert_eq!(data, ": heartbeat\n\n");
        }
    })
    .await
    .expect("stream expired cleanly");
    drop(first);

    let reopened = app
        .oneshot(stream_request(
            true,
            Some("text/event-stream"),
            Some(&zero_cursor),
        ))
        .await
        .unwrap();
    assert_eq!(reopened.status(), StatusCode::OK);
}

struct ControlledHeadRepository {
    fallback: InMemoryItemRepository,
    head_mode: AtomicU8,
    forced_head: AtomicU64,
    block_once: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl ControlledHeadRepository {
    fn forced(head: u64) -> Self {
        Self {
            fallback: InMemoryItemRepository::default(),
            head_mode: AtomicU8::new(FORCED_HEAD),
            forced_head: AtomicU64::new(head),
            block_once: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }

    fn failed() -> Self {
        Self {
            head_mode: AtomicU8::new(FAILED_HEAD),
            ..Self::forced(0)
        }
    }

    fn blocking_once() -> Self {
        Self {
            fallback: InMemoryItemRepository::default(),
            head_mode: AtomicU8::new(NORMAL_HEAD),
            forced_head: AtomicU64::new(0),
            block_once: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }
}

#[async_trait]
impl ItemRepository for ControlledHeadRepository {
    fn cursor_scope(&self) -> Uuid {
        self.fallback.cursor_scope()
    }

    async fn create(
        &self,
        item: Item,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        self.fallback.create(item, idempotency).await
    }

    async fn get(&self, id: Uuid, include_deleted: bool) -> Result<Item, ItemRepositoryError> {
        self.fallback.get(id, include_deleted).await
    }

    async fn list(&self, query: ItemQuery) -> Result<Vec<Item>, ItemRepositoryError> {
        self.fallback.list(query).await
    }

    async fn replace(
        &self,
        id: Uuid,
        expected_revision: u64,
        replacement: ReplaceItem,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        self.fallback
            .replace(id, expected_revision, replacement, now, idempotency)
            .await
    }

    async fn trash(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        self.fallback
            .trash(id, expected_revision, now, idempotency)
            .await
    }

    async fn restore(
        &self,
        id: Uuid,
        expected_revision: u64,
        now: DateTime<Utc>,
        idempotency: IdempotencyContext,
    ) -> Result<ItemMutation, ItemRepositoryError> {
        self.fallback
            .restore(id, expected_revision, now, idempotency)
            .await
    }

    async fn delta_head(&self) -> Result<u64, ItemRepositoryError> {
        let head = match self.head_mode.load(Ordering::SeqCst) {
            NORMAL_HEAD => self.fallback.delta_head().await?,
            FORCED_HEAD => self.forced_head.load(Ordering::SeqCst),
            _ => return Err(ItemRepositoryError::Internal),
        };
        if self.block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(head)
    }

    async fn delta(&self, after: u64, limit: usize) -> Result<ItemDeltaPage, ItemRepositoryError> {
        self.fallback.delta(after, limit).await
    }
}

fn new_item(id: Uuid, title: &str, parent_id: Option<Uuid>) -> NewItem {
    serde_json::from_value(item_body(id, parent_id, title)).expect("valid item fixture")
}

fn idempotency(key: &str) -> IdempotencyKey {
    IdempotencyKey {
        key: key.to_owned(),
        fingerprint: [7; 32],
    }
}
