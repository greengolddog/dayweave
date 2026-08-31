use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    execution::{
        DeferAssessment, DeferAssessmentRequest, ExecutionCommand, ExecutionIdempotency,
        ExecutionInvalidationConfig, ExecutionMutation, ExecutionRepository,
        ExecutionRepositoryError, ExecutionService, ExecutionSnapshot, InMemoryExecutionRepository,
    },
    http::router,
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tokio::{sync::Notify, time::timeout};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "execution-stream-api-token";

fn stream_config(
    probe: Duration,
    heartbeat: Duration,
    lifetime: Duration,
    max_connections: usize,
) -> ExecutionInvalidationConfig {
    ExecutionInvalidationConfig::new(probe, heartbeat, lifetime, max_connections)
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
    repository: Arc<dyn ExecutionRepository>,
    config: ExecutionInvalidationConfig,
) -> Router {
    let mut state = test_state();
    state.execution = Arc::new(
        ExecutionService::new(repository, state.items.clone(), Arc::new(SystemClock))
            .with_invalidation_config(config),
    );
    router(state)
}

fn standard_app() -> Router {
    app_with_repository(
        Arc::new(InMemoryExecutionRepository::default()),
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
    let mut request = request("GET", "/v1/execution/stream", None, authenticated);
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

fn item(id: Uuid) -> Value {
    json!({
        "id": id,
        "is_sensitive": false,
        "kind": "task",
        "status": "planned",
        "title": "Private execution title must never enter SSE",
        "notes": "Private execution notes must never enter SSE",
        "timezone_name": "Europe/Madrid",
        "duration_seconds": 1800,
        "deadline_at": null,
        "earliest_start_at": null,
        "recurrence": null,
        "flexible_constraints": {},
        "split_policy": { "type": "indivisible" },
        "importance": 80,
        "urgency": 70,
        "parent_id": null,
        "sibling_order": 0
    })
}

fn start_command(item_id: Uuid, session_id: Uuid) -> Value {
    json!({
        "expected_revision": 0,
        "command": {
            "type": "start",
            "session_id": session_id,
            "item_id": item_id,
            "item_revision": 1,
            "occurrence_id": null,
            "session_index": 0,
            "planned_block_id": null,
            "device_id": Uuid::new_v4()
        }
    })
}

async fn create_item(app: &Router, item_id: Uuid) {
    let response = app
        .clone()
        .oneshot({
            let mut request = request("POST", "/v1/items", Some(item(item_id)), true);
            request.headers_mut().insert(
                "idempotency-key",
                HeaderValue::from_static("stream-create-item-001"),
            );
            request
        })
        .await
        .expect("item response");
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn start_item(
    app: &Router,
    item_id: Uuid,
    session_id: Uuid,
    key: &'static str,
) -> Response<Body> {
    app.clone()
        .oneshot({
            let mut request = request(
                "POST",
                "/v1/execution/commands",
                Some(start_command(item_id, session_id)),
                true,
            );
            request
                .headers_mut()
                .insert("idempotency-key", HeaderValue::from_static(key));
            request
        })
        .await
        .expect("execution response")
}

fn assert_revision_frame_is_content_free(frame: &str, revision: u64) {
    let expected_id = format!("id: {revision}");
    let expected_data = format!(r#"data: {{"revision":{revision}}}"#);
    assert_eq!(
        frame,
        format!("{expected_id}\nevent: execution-invalidation\n{expected_data}\n\n")
    );
    for forbidden in [
        "session",
        "item",
        "device",
        "title",
        "status",
        "Private execution",
    ] {
        assert!(!frame.contains(forbidden), "SSE leaked {forbidden}");
    }
}

#[tokio::test]
async fn stream_requires_authentication_accept_and_canonical_cursor() {
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

    for cursor in ["", "00", "01", "+1", "-1", "1.0", "18446744073709551616"] {
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

    let mut duplicate = stream_request(true, Some("text/event-stream"), Some("0"));
    duplicate
        .headers_mut()
        .append("last-event-id", HeaderValue::from_static("0"));
    let duplicate = app.clone().oneshot(duplicate).await.unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);

    let ahead = app
        .clone()
        .oneshot(stream_request(true, Some("text/event-stream"), Some("1")))
        .await
        .unwrap();
    assert_eq!(ahead.status(), StatusCode::CONFLICT);
    let ahead = body_json(ahead).await;
    assert_eq!(ahead["error"]["details"]["cursor_revision"], 1);
    assert_eq!(ahead["error"]["details"]["head_revision"], 0);

    let accepted = app
        .oneshot(stream_request(true, Some("TEXT/EVENT-STREAM"), Some("0")))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
}

#[tokio::test]
async fn stream_catches_up_immediately_and_wakes_only_after_successful_mutation() {
    let app = standard_app();
    let item_id = Uuid::new_v4();
    create_item(&app, item_id).await;

    let mut live = app
        .clone()
        .oneshot(stream_request(true, Some("text/event-stream"), None))
        .await
        .unwrap();
    assert_eq!(live.status(), StatusCode::OK);
    assert_eq!(live.headers()[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(live.headers()[header::CACHE_CONTROL], "no-store, no-cache");
    assert_eq!(live.headers()[header::PRAGMA], "no-cache");
    assert_eq!(live.headers()["x-accel-buffering"], "no");

    let failed = start_item(
        &app,
        Uuid::new_v4(),
        Uuid::new_v4(),
        "stream-failed-start-001",
    )
    .await;
    assert_eq!(failed.status(), StatusCode::NOT_FOUND);
    assert!(
        timeout(Duration::from_millis(40), live.body_mut().frame())
            .await
            .is_err(),
        "a failed command must not publish an execution invalidation"
    );

    let session_id = Uuid::new_v4();
    let started = start_item(&app, item_id, session_id, "stream-live-start-001").await;
    assert_eq!(started.status(), StatusCode::OK);
    let live_frame = next_stream_chunk(&mut live, Duration::from_secs(1))
        .await
        .expect("live invalidation");
    assert_revision_frame_is_content_free(&live_frame, 1);

    let mut catch_up = app
        .clone()
        .oneshot(stream_request(true, Some("text/event-stream"), Some("0")))
        .await
        .unwrap();
    let catch_up_frame = next_stream_chunk(&mut catch_up, Duration::from_secs(1))
        .await
        .expect("immediate catch-up invalidation");
    assert_revision_frame_is_content_free(&catch_up_frame, 1);

    // Omitting Last-Event-ID is explicitly the fresh-client cursor 0.
    let mut fresh = app
        .oneshot(stream_request(true, Some("text/event-stream"), None))
        .await
        .unwrap();
    let fresh_frame = next_stream_chunk(&mut fresh, Duration::from_secs(1))
        .await
        .expect("fresh-client catch-up invalidation");
    assert_revision_frame_is_content_free(&fresh_frame, 1);
}

#[derive(Default)]
struct ExternalHeadRepository {
    revision: AtomicU64,
    fallback: InMemoryExecutionRepository,
}

#[async_trait]
impl ExecutionRepository for ExternalHeadRepository {
    async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionRepositoryError> {
        Ok(ExecutionSnapshot {
            revision: self.revision.load(Ordering::SeqCst),
            active_session: None,
        })
    }

    async fn replay(
        &self,
        now: DateTime<Utc>,
        idempotency: &ExecutionIdempotency,
    ) -> Result<Option<ExecutionMutation>, ExecutionRepositoryError> {
        self.fallback.replay(now, idempotency).await
    }

    async fn assess_defer(
        &self,
        request: DeferAssessmentRequest,
        now: DateTime<Utc>,
    ) -> Result<DeferAssessment, ExecutionRepositoryError> {
        self.fallback.assess_defer(request, now).await
    }

    async fn apply(
        &self,
        expected_revision: u64,
        command: ExecutionCommand,
        now: DateTime<Utc>,
        idempotency: ExecutionIdempotency,
    ) -> Result<ExecutionMutation, ExecutionRepositoryError> {
        self.fallback
            .apply(expected_revision, command, now, idempotency)
            .await
    }

    async fn history(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<dayweave_api::execution::ExecutionSession>, ExecutionRepositoryError> {
        self.fallback.history(limit, offset).await
    }
}

#[tokio::test]
async fn authoritative_probe_recovers_a_missed_or_cross_instance_wakeup() {
    let repository = Arc::new(ExternalHeadRepository::default());
    let app = app_with_repository(
        repository.clone(),
        stream_config(
            Duration::from_millis(10),
            Duration::from_millis(200),
            Duration::from_millis(500),
            2,
        ),
    );
    let mut response = app
        .oneshot(stream_request(true, Some("text/event-stream"), Some("0")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // This bypasses the service publisher, as a different process or a
    // canceled post-commit request would.
    repository.revision.store(7, Ordering::SeqCst);
    let frame = next_stream_chunk(&mut response, Duration::from_millis(200))
        .await
        .expect("probe invalidation");
    assert_revision_frame_is_content_free(&frame, 7);
}

struct BlockingSnapshotRepository {
    fallback: InMemoryExecutionRepository,
    block_once: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Default for BlockingSnapshotRepository {
    fn default() -> Self {
        Self {
            fallback: InMemoryExecutionRepository::default(),
            block_once: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }
}

#[async_trait]
impl ExecutionRepository for BlockingSnapshotRepository {
    async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionRepositoryError> {
        // Capture the old authoritative head before allowing a command to
        // commit. Returning that captured value makes this test distinguish
        // subscribe-before-read from subscribing only after the read returns.
        let snapshot = self.fallback.snapshot().await?;
        if self.block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(snapshot)
    }

    async fn replay(
        &self,
        now: DateTime<Utc>,
        idempotency: &ExecutionIdempotency,
    ) -> Result<Option<ExecutionMutation>, ExecutionRepositoryError> {
        self.fallback.replay(now, idempotency).await
    }

    async fn assess_defer(
        &self,
        request: DeferAssessmentRequest,
        now: DateTime<Utc>,
    ) -> Result<DeferAssessment, ExecutionRepositoryError> {
        self.fallback.assess_defer(request, now).await
    }

    async fn apply(
        &self,
        expected_revision: u64,
        command: ExecutionCommand,
        now: DateTime<Utc>,
        idempotency: ExecutionIdempotency,
    ) -> Result<ExecutionMutation, ExecutionRepositoryError> {
        self.fallback
            .apply(expected_revision, command, now, idempotency)
            .await
    }

    async fn history(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<dayweave_api::execution::ExecutionSession>, ExecutionRepositoryError> {
        self.fallback.history(limit, offset).await
    }
}

#[tokio::test]
async fn subscribing_before_the_head_read_closes_the_opening_race() {
    let repository = Arc::new(BlockingSnapshotRepository::default());
    let app = app_with_repository(
        repository.clone(),
        stream_config(
            Duration::from_millis(200),
            Duration::from_millis(200),
            Duration::from_secs(1),
            2,
        ),
    );
    let item_id = Uuid::new_v4();
    create_item(&app, item_id).await;

    let stream_app = app.clone();
    let opening = tokio::spawn(async move {
        stream_app
            .oneshot(stream_request(true, Some("text/event-stream"), Some("0")))
            .await
            .expect("stream response")
    });
    timeout(Duration::from_secs(1), repository.entered.notified())
        .await
        .expect("stream reached authoritative head read");

    let started = start_item(&app, item_id, Uuid::new_v4(), "stream-opening-race-001").await;
    assert_eq!(started.status(), StatusCode::OK);
    repository.release.notify_one();

    let mut response = opening.await.expect("opening task");
    assert_eq!(response.status(), StatusCode::OK);
    let frame = next_stream_chunk(&mut response, Duration::from_secs(1))
        .await
        .expect("race-safe invalidation");
    assert_revision_frame_is_content_free(&frame, 1);
}

#[tokio::test]
async fn stream_capacity_heartbeat_and_expiry_are_bounded() {
    let app = app_with_repository(
        Arc::new(InMemoryExecutionRepository::default()),
        stream_config(
            Duration::from_millis(80),
            Duration::from_millis(10),
            Duration::from_millis(60),
            1,
        ),
    );
    let mut first = app
        .clone()
        .oneshot(stream_request(true, Some("text/event-stream"), Some("0")))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let saturated = app
        .clone()
        .oneshot(stream_request(true, Some("text/event-stream"), Some("0")))
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
        .oneshot(stream_request(true, Some("text/event-stream"), Some("0")))
        .await
        .unwrap();
    assert_eq!(reopened.status(), StatusCode::OK);
}
