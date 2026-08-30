use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use chrono::{Duration as ChronoDuration, Utc};
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
#[allow(clippy::too_many_lines)] // Keeps the conflict, closed-lease retry, and exact replay contract together.
async fn terminal_item_projection_waits_for_the_execution_lease_to_close() {
    let app = test_app();
    let item_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let created_item = item(item_id);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(created_item.clone()),
            true,
            Some("execution-guard-create-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let started = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                0,
                json!({
                    "type": "start",
                    "session_id": session_id,
                    "item_id": item_id,
                    "item_revision": 1,
                    "occurrence_id": null,
                    "session_index": 0,
                    "planned_block_id": null,
                    "device_id": Uuid::new_v4()
                }),
            )),
            true,
            Some("execution-guard-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);

    let mut completed_fields = created_item;
    completed_fields.as_object_mut().unwrap().remove("id");
    completed_fields["status"] = json!("completed");
    let completed_request = json!({
        "expected_revision": 1,
        "item": completed_fields,
    });
    let blocked = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{item_id}"),
            Some(completed_request.clone()),
            true,
            Some("execution-guard-terminal-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    let blocked = body_json(blocked).await;
    assert_eq!(blocked["error"]["code"], "item_execution_active");
    assert_eq!(blocked["error"]["details"]["item_id"], item_id.to_string());
    assert_eq!(
        blocked["error"]["details"]["session_id"],
        session_id.to_string()
    );

    let closed = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                1,
                json!({
                    "type": "complete",
                    "session_id": session_id,
                    "actual_seconds": 0
                }),
            )),
            true,
            Some("execution-guard-close-001"),
        ))
        .await
        .unwrap();
    assert_eq!(closed.status(), StatusCode::OK);

    let reconciled = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{item_id}"),
            Some(completed_request.clone()),
            true,
            Some("execution-guard-terminal-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(reconciled.status(), StatusCode::OK);
    assert_eq!(reconciled.headers()["idempotency-replayed"], "false");
    assert_eq!(body_json(reconciled).await["item"]["revision"], 2);

    let mut reopened_request = completed_request.clone();
    reopened_request["expected_revision"] = json!(2);
    reopened_request["item"]["status"] = json!("planned");
    let reopened = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{item_id}"),
            Some(reopened_request),
            true,
            Some("execution-guard-reopen-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(reopened.status(), StatusCode::OK);

    let second_session_id = Uuid::new_v4();
    let second_started = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                2,
                json!({
                    "type": "start",
                    "session_id": second_session_id,
                    "item_id": item_id,
                    "item_revision": 3,
                    "occurrence_id": null,
                    "session_index": 1,
                    "planned_block_id": null,
                    "device_id": Uuid::new_v4()
                }),
            )),
            true,
            Some("execution-guard-second-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(second_started.status(), StatusCode::OK);

    // Exact idempotency replay is historical: it must return the stored response without
    // projecting it again or being rejected by a lease that opened later.
    let replay = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{item_id}"),
            Some(completed_request),
            true,
            Some("execution-guard-terminal-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert_eq!(body_json(replay).await["item"]["revision"], 2);

    let canonical = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/items/{item_id}"),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    let canonical = body_json(canonical).await;
    assert_eq!(canonical["item"]["status"], "planned");
    assert_eq!(canonical["item"]["revision"], 3);

    let blocked_trash = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{item_id}?expected_revision=3"),
            None,
            true,
            Some("execution-guard-trash-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(blocked_trash.status(), StatusCode::CONFLICT);
    let blocked_trash = body_json(blocked_trash).await;
    assert_eq!(blocked_trash["error"]["code"], "item_execution_active");
    assert_eq!(
        blocked_trash["error"]["details"]["session_id"],
        second_session_id.to_string()
    );

    let second_closed = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                3,
                json!({
                    "type": "complete",
                    "session_id": second_session_id,
                    "actual_seconds": 0
                }),
            )),
            true,
            Some("execution-guard-second-close-001"),
        ))
        .await
        .unwrap();
    assert_eq!(second_closed.status(), StatusCode::OK);

    let trashed = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{item_id}?expected_revision=3"),
            None,
            true,
            Some("execution-guard-trash-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(trashed.status(), StatusCode::OK);
    assert_eq!(trashed.headers()["idempotency-replayed"], "false");
    assert_eq!(body_json(trashed).await["item"]["revision"], 4);

    let restored = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/items/{item_id}/restore"),
            Some(json!({ "expected_revision": 4 })),
            true,
            Some("execution-guard-restore-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);

    let third_session_id = Uuid::new_v4();
    let third_started = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                4,
                json!({
                    "type": "start",
                    "session_id": third_session_id,
                    "item_id": item_id,
                    "item_revision": 5,
                    "occurrence_id": null,
                    "session_index": 2,
                    "planned_block_id": null,
                    "device_id": Uuid::new_v4()
                }),
            )),
            true,
            Some("execution-guard-third-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(third_started.status(), StatusCode::OK);

    let trash_replay = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{item_id}?expected_revision=3"),
            None,
            true,
            Some("execution-guard-trash-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(trash_replay.status(), StatusCode::OK);
    assert_eq!(trash_replay.headers()["idempotency-replayed"], "true");
    assert_eq!(body_json(trash_replay).await["item"]["revision"], 4);
    let canonical = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/items/{item_id}"),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    let canonical = body_json(canonical).await;
    assert_eq!(canonical["item"]["status"], "planned");
    assert_eq!(canonical["item"]["revision"], 5);
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

    let exact_now = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let nanosecond_pause = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                0,
                json!({
                    "type": "pause",
                    "session_id": Uuid::new_v4(),
                    "duration_seconds": null,
                    "pause_until": exact_now
                        + ChronoDuration::minutes(1)
                        + ChronoDuration::nanoseconds(1),
                    "reason": null
                }),
            )),
            true,
            Some("execution-nanosecond-break-001"),
        ))
        .await
        .unwrap();
    assert_eq!(nanosecond_pause.status(), StatusCode::UNPROCESSABLE_ENTITY);

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

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers response compatibility and replay as one public contract.
async fn execution_defer_is_terminal_exact_and_replayable_over_http() {
    let app = test_app();
    let item_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(item(item_id)),
            true,
            Some("execution-defer-create-item-001"),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let started = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                0,
                json!({
                    "type": "start",
                    "session_id": session_id,
                    "item_id": item_id,
                    "item_revision": 1,
                    "occurrence_id": null,
                    "session_index": 0,
                    "planned_block_id": null,
                    "device_id": Uuid::new_v4()
                }),
            )),
            true,
            Some("execution-defer-start-001"),
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);
    let started = body_json(started).await;
    assert!(
        started["mutation"]["changed_session"]
            .get("move_start")
            .is_none()
    );
    assert!(
        started["mutation"]["changed_session"]
            .get("move_end")
            .is_none()
    );

    let exact_now = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let move_start = exact_now + ChronoDuration::days(30) + ChronoDuration::microseconds(1);
    let move_end = move_start + ChronoDuration::hours(24);
    let defer = command(
        1,
        json!({
            "type": "defer",
            "session_id": session_id,
            "move_start": move_start,
            "move_end": move_end
        }),
    );
    let deferred = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(defer.clone()),
            true,
            Some("execution-defer-command-001"),
        ))
        .await
        .unwrap();
    assert_eq!(deferred.status(), StatusCode::OK);
    assert_eq!(deferred.headers()["idempotency-replayed"], "false");
    let deferred = body_json(deferred).await;
    assert_eq!(deferred["mutation"]["revision"], 2);
    assert!(deferred["mutation"]["active_session"].is_null());
    assert_eq!(
        deferred["mutation"]["changed_session"]["status"],
        "deferred"
    );
    assert_eq!(
        deferred["mutation"]["changed_session"]["move_start"],
        serde_json::to_value(move_start).unwrap()
    );
    assert_eq!(
        deferred["mutation"]["changed_session"]["move_end"],
        serde_json::to_value(move_end).unwrap()
    );
    assert_eq!(
        deferred["mutation"]["changed_session"]["ended_at"],
        deferred["mutation"]["changed_session"]["updated_at"]
    );

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(defer),
            true,
            Some("execution-defer-command-001"),
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
}

#[tokio::test]
async fn execution_defer_rejects_invalid_windows_missing_fields_and_unknown_fields() {
    let app = test_app();
    let now = Utc::now();
    let exact_now = chrono::DateTime::from_timestamp_micros(now.timestamp_micros()).unwrap();
    let invalid_windows = [
        (
            now - ChronoDuration::seconds(1),
            now + ChronoDuration::minutes(1),
        ),
        (
            now + ChronoDuration::minutes(1),
            now + ChronoDuration::minutes(1),
        ),
        (
            now + ChronoDuration::days(30),
            now + ChronoDuration::days(31) + ChronoDuration::seconds(1),
        ),
        (
            exact_now + ChronoDuration::days(30) + ChronoDuration::nanoseconds(1),
            exact_now + ChronoDuration::days(30) + ChronoDuration::hours(1),
        ),
    ];
    for (index, (move_start, move_end)) in invalid_windows.into_iter().enumerate() {
        let invalid = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/execution/commands",
                Some(command(
                    0,
                    json!({
                        "type": "defer",
                        "session_id": Uuid::new_v4(),
                        "move_start": move_start,
                        "move_end": move_end
                    }),
                )),
                true,
                Some(&format!("execution-invalid-defer-{index:03}")),
            ))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let future_start = now + ChronoDuration::hours(1);
    for (index, malformed) in [
        json!({
            "type": "defer",
            "session_id": Uuid::new_v4(),
            "move_start": future_start
        }),
        json!({
            "type": "defer",
            "session_id": Uuid::new_v4(),
            "move_start": future_start,
            "move_end": future_start + ChronoDuration::minutes(30),
            "surprise": true
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let malformed = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/execution/commands",
                Some(command(0, malformed)),
                true,
                Some(&format!("execution-malformed-defer-{index:03}")),
            ))
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    }
}
