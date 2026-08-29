use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "execution-api-test-token";

fn test_app() -> Router {
    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let authenticator = Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN]));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    router(AppState::new(proposals, authenticator, readiness))
}

fn request(
    method: &str,
    uri: &str,
    body: Option<Value>,
    authenticated: bool,
    idempotency_key: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    if let Some(key) = idempotency_key {
        builder = builder.header("Idempotency-Key", key);
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn item(id: Uuid) -> Value {
    json!({
        "id": id,
        "is_sensitive": false,
        "kind": "task",
        "status": "planned",
        "title": "Canonical timer task",
        "notes": null,
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

fn command(expected_revision: u64, command: Value) -> Value {
    let mut request = serde_json::Map::new();
    request.insert("expected_revision".to_owned(), json!(expected_revision));
    request.insert("command".to_owned(), command);
    Value::Object(request)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn execution_is_authenticated_cross_device_revisioned_and_idempotent() {
    let app = test_app();
    let item_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();

    let unauthorized = app
        .clone()
        .oneshot(request("GET", "/v1/execution", None, false, None))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let initial = app
        .clone()
        .oneshot(request("GET", "/v1/execution", None, true, None))
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    let initial = body_json(initial).await;
    assert_eq!(initial["execution"]["revision"], 0);
    assert!(initial["execution"]["active_session"].is_null());

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(item(item_id)),
            true,
            Some("execution-create-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let start = command(
        0,
        json!({
            "type": "start",
            "session_id": session_id,
            "item_id": item_id,
            "item_revision": 1,
            "occurrence_id": null,
            "session_index": 0,
            "planned_block_id": null,
            "device_id": device_id
        }),
    );
    let started = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(start.clone()),
            true,
            Some("execution-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);
    assert_eq!(started.headers()["idempotency-replayed"], "false");
    let started = body_json(started).await;
    assert_eq!(started["mutation"]["revision"], 1);
    assert_eq!(started["mutation"]["active_session"]["status"], "active");

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(start),
            true,
            Some("execution-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");

    let changed_replay = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                1,
                json!({ "type": "resume", "session_id": session_id }),
            )),
            true,
            Some("execution-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(changed_replay.status(), StatusCode::CONFLICT);

    let stale = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                0,
                json!({
                    "type": "pause",
                    "session_id": session_id,
                    "duration_seconds": 60,
                    "pause_until": null,
                    "reason": "Tea"
                }),
            )),
            true,
            Some("execution-stale-pause-001"),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale = body_json(stale).await;
    assert_eq!(stale["error"]["details"]["actual_revision"], 1);

    let paused = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                1,
                json!({
                    "type": "pause",
                    "session_id": session_id,
                    "duration_seconds": 60,
                    "pause_until": null,
                    "reason": "Tea"
                }),
            )),
            true,
            Some("execution-pause-001"),
        ))
        .await
        .unwrap();
    assert_eq!(paused.status(), StatusCode::OK);
    let paused = body_json(paused).await;
    assert_eq!(paused["mutation"]["revision"], 2);
    assert_eq!(paused["mutation"]["active_session"]["status"], "paused");

    let extended = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                2,
                json!({
                    "type": "pause",
                    "session_id": session_id,
                    "duration_seconds": 600,
                    "pause_until": null,
                    "reason": null
                }),
            )),
            true,
            Some("execution-extend-001"),
        ))
        .await
        .unwrap();
    assert_eq!(extended.status(), StatusCode::OK);
    let extended = body_json(extended).await;
    assert_eq!(extended["mutation"]["revision"], 3);
    assert_eq!(
        extended["mutation"]["changed_session"]["pause_reason"],
        "Tea"
    );

    let resumed = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                3,
                json!({ "type": "resume", "session_id": session_id }),
            )),
            true,
            Some("execution-resume-001"),
        ))
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);

    let completed = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                4,
                json!({
                    "type": "complete",
                    "session_id": session_id,
                    "actual_seconds": 42
                }),
            )),
            true,
            Some("execution-complete-001"),
        ))
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);
    let completed = body_json(completed).await;
    assert_eq!(completed["mutation"]["revision"], 5);
    assert!(completed["mutation"]["active_session"].is_null());
    assert_eq!(
        completed["mutation"]["changed_session"]["actual_seconds"],
        42
    );

    let history = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/execution/history?limit=1",
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(history.status(), StatusCode::OK);
    let history = body_json(history).await;
    assert_eq!(history["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(history["sessions"][0]["status"], "completed");
    assert!(history["next_offset"].is_null());
}

#[tokio::test]
async fn execution_rejects_malformed_breaks_and_unknown_fields() {
    let app = test_app();
    let invalid = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                0,
                json!({
                    "type": "pause",
                    "session_id": Uuid::new_v4(),
                    "duration_seconds": 60,
                    "pause_until": "2026-09-01T12:00:00Z",
                    "reason": null
                }),
            )),
            true,
            Some("execution-invalid-break-001"),
        ))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let unknown = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(json!({
                "expected_revision": 0,
                "command": {
                    "type": "resume",
                    "session_id": Uuid::new_v4(),
                    "surprise": true
                }
            })),
            true,
            Some("execution-unknown-field-001"),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
}
