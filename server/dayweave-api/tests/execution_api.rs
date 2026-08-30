use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    execution::{
        DeferAssessment, DeferAssessmentRequest, ExecutionCommand, ExecutionIdempotency,
        ExecutionMutation, ExecutionRepository, ExecutionRepositoryError, ExecutionService,
        ExecutionSnapshot, InMemoryExecutionRepository,
    },
    http::router,
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "execution-api-test-token";

fn test_state() -> AppState {
    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let authenticator = Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN]));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    AppState::new(proposals, authenticator, readiness)
}

fn test_app() -> Router {
    router(test_state())
}

#[derive(Clone)]
struct AssessmentRepository {
    assessment: DeferAssessment,
    requests: Arc<Mutex<Vec<DeferAssessmentRequest>>>,
    fallback: InMemoryExecutionRepository,
}

#[async_trait]
impl ExecutionRepository for AssessmentRepository {
    async fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionRepositoryError> {
        self.fallback.snapshot().await
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
        _now: DateTime<Utc>,
    ) -> Result<DeferAssessment, ExecutionRepositoryError> {
        self.requests
            .lock()
            .expect("assessment request lock")
            .push(request);
        Ok(self.assessment.clone())
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

fn test_app_with_assessment(
    assessment: DeferAssessment,
) -> (Router, Arc<Mutex<Vec<DeferAssessmentRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let repository: Arc<dyn ExecutionRepository> = Arc::new(AssessmentRepository {
        assessment,
        requests: requests.clone(),
        fallback: InMemoryExecutionRepository::default(),
    });
    let mut state = test_state();
    state.execution = Arc::new(ExecutionService::new(
        repository,
        state.items.clone(),
        Arc::new(SystemClock),
    ));
    (router(state), requests)
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
async fn defer_assessment_route_returns_the_exact_envelope_and_forwards_the_closed_body() {
    let session_id = Uuid::new_v4();
    let item_id = Uuid::new_v4();
    let occurrence_id = Uuid::new_v4();
    let source_schedule_revision_id = Uuid::new_v4();
    let source_block_id = Uuid::new_v4();
    let move_start = "2026-10-02T09:30:00Z".parse().unwrap();
    let move_end = "2026-10-02T10:15:00Z".parse().unwrap();
    let expires_at = "2026-09-01T10:05:00Z".parse().unwrap();
    let assessment = DeferAssessment {
        session_id,
        execution_revision: 7,
        session_revision: 3,
        item_id,
        item_revision: 11,
        occurrence_id: Some(occurrence_id),
        source_session_index: 2,
        replacement_session_index: 4,
        source_schedule_revision_id,
        source_block_id,
        actual_seconds: 601,
        credited_source_seconds: 660,
        planned_duration_seconds: 3_600,
        remaining_duration_seconds: 2_940,
        move_start,
        move_end,
        environment_digest: format!("sha256:{}", "a".repeat(64)),
        assessment_digest: format!("sha256:{}", "b".repeat(64)),
        approval_required: false,
        violations: Vec::new(),
        expires_at,
    };
    let assessment_request = DeferAssessmentRequest {
        expected_revision: 7,
        session_id,
        move_start,
        actual_seconds: Some(601),
    };
    let (app, captured) = test_app_with_assessment(assessment.clone());

    let response = app
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(serde_json::to_value(&assessment_request).unwrap()),
            true,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "assessment": assessment })
    );
    assert_eq!(
        *captured.lock().expect("assessment request lock"),
        vec![assessment_request]
    );
}

#[tokio::test]
async fn defer_assessment_body_is_closed_and_memory_mode_fails_without_mutation() {
    let app = test_app();
    let session_id = Uuid::new_v4();
    let move_start = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
        .unwrap()
        + ChronoDuration::hours(1);
    let valid = json!({
        "expected_revision": 0,
        "session_id": session_id,
        "move_start": move_start,
        "actual_seconds": null
    });

    let mut unknown = valid.clone();
    unknown["client_assessment"] = json!("must not be trusted");
    let unknown = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(unknown),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(json!({
                "expected_revision": 0,
                "session_id": session_id
            })),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);

    let unsupported_revision = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(json!({
                "expected_revision": (i64::MAX as u64) + 1,
                "session_id": session_id,
                "move_start": move_start,
                "actual_seconds": null
            })),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        unsupported_revision.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let unavailable = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(valid),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    let unavailable = body_json(unavailable).await;
    assert_eq!(unavailable["error"]["code"], "service_unavailable");
    assert!(unavailable["error"].get("details").is_none());

    let snapshot = app
        .oneshot(request("GET", "/v1/execution", None, true, None))
        .await
        .unwrap();
    let snapshot = body_json(snapshot).await;
    assert_eq!(snapshot["execution"]["revision"], 0);
    assert!(snapshot["execution"]["active_session"].is_null());
}

#[tokio::test]
async fn defer_command_requires_canonical_matching_assessment_digests_at_the_http_boundary() {
    let app = test_app();
    let move_start = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
        .unwrap()
        + ChronoDuration::days(30);
    let move_end = move_start + ChronoDuration::minutes(30);
    let canonical = format!("sha256:{}", "a".repeat(64));
    let other = format!("sha256:{}", "b".repeat(64));
    let invalid_digests = [
        (Some("sha256:abc".to_owned()), None),
        (Some(format!("sha256:{}", "A".repeat(64))), None),
        (Some(format!("sha256:{}", "g".repeat(64))), None),
        (Some(format!("SHA256:{}", "a".repeat(64))), None),
        (None, Some(canonical.clone())),
        (Some(canonical.clone()), Some(other)),
    ];

    for (index, (assessment_digest, approved_assessment_digest)) in
        invalid_digests.into_iter().enumerate()
    {
        let response = app
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
                        "move_end": move_end,
                        "actual_seconds": 0,
                        "assessment_digest": assessment_digest,
                        "approved_assessment_digest": approved_assessment_digest
                    }),
                )),
                true,
                Some(&format!("execution-invalid-assessment-digest-{index:03}")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "validation_failed"
        );
    }

    let accepted_at_boundary = app
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                0,
                json!({
                    "type": "defer",
                    "session_id": Uuid::new_v4(),
                    "move_start": move_start,
                    "move_end": move_end,
                    "actual_seconds": 0,
                    "assessment_digest": canonical,
                    "approved_assessment_digest": canonical
                }),
            )),
            true,
            Some("execution-valid-assessment-digest-001"),
        ))
        .await
        .unwrap();
    assert_eq!(accepted_at_boundary.status(), StatusCode::NOT_FOUND);
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

    let active_defer = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/execution/commands",
            Some(command(
                1,
                json!({
                    "type": "defer",
                    "session_id": session_id,
                    "move_start": move_start,
                    "move_end": move_end
                }),
            )),
            true,
            Some("execution-active-defer-rejected-001"),
        ))
        .await
        .unwrap();
    assert_eq!(active_defer.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(active_defer).await["error"]["code"], "conflict");

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
                    "duration_seconds": null,
                    "pause_until": null,
                    "reason": "Choose a later time"
                }),
            )),
            true,
            Some("execution-defer-pause-001"),
        ))
        .await
        .unwrap();
    assert_eq!(paused.status(), StatusCode::OK);
    let paused = body_json(paused).await;
    assert_eq!(paused["mutation"]["revision"], 2);
    assert_eq!(paused["mutation"]["active_session"]["status"], "paused");

    let defer = command(
        2,
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
    assert_eq!(deferred["mutation"]["revision"], 3);
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
#[allow(clippy::too_many_lines)] // Covers all terminal states and exact replay as one contract.
async fn terminal_semantic_slots_fail_closed_without_schedule_attestation() {
    for terminal_type in ["completed", "skipped", "deferred"] {
        let app = test_app();
        let item_id = Uuid::new_v4();
        let first_session_id = Uuid::new_v4();
        let start_key = format!("execution-terminal-{terminal_type}-start-001");
        let terminal_key = format!("execution-terminal-{terminal_type}-close-001");
        let retry_key = format!("execution-terminal-{terminal_type}-retry-001");
        let created = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(item(item_id)),
                true,
                Some(&format!("execution-terminal-{terminal_type}-item-001")),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let first_start = command(
            0,
            json!({
                "type": "start",
                "session_id": first_session_id,
                "item_id": item_id,
                "item_revision": 1,
                "occurrence_id": null,
                "session_index": 0,
                "planned_block_id": null,
                "device_id": Uuid::new_v4()
            }),
        );
        let started = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/execution/commands",
                Some(first_start.clone()),
                true,
                Some(&start_key),
            ))
            .await
            .unwrap();
        assert_eq!(started.status(), StatusCode::OK);

        let terminal_revision = if terminal_type == "deferred" {
            let paused = app
                .clone()
                .oneshot(request(
                    "POST",
                    "/v1/execution/commands",
                    Some(command(
                        1,
                        json!({
                            "type": "pause",
                            "session_id": first_session_id,
                            "duration_seconds": null,
                            "pause_until": null,
                            "reason": null
                        }),
                    )),
                    true,
                    Some(&format!("execution-terminal-{terminal_type}-pause-001")),
                ))
                .await
                .unwrap();
            assert_eq!(paused.status(), StatusCode::OK);
            2
        } else {
            1
        };
        let terminal = if terminal_type == "deferred" {
            let exact_now =
                chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
            let move_start = exact_now + ChronoDuration::days(30);
            command(
                terminal_revision,
                json!({
                    "type": "defer",
                    "session_id": first_session_id,
                    "move_start": move_start,
                    "move_end": move_start + ChronoDuration::minutes(30)
                }),
            )
        } else {
            let command_type = match terminal_type {
                "completed" => "complete",
                "skipped" => "skip",
                _ => unreachable!(),
            };
            command(
                terminal_revision,
                json!({
                    "type": command_type,
                    "session_id": first_session_id,
                    "actual_seconds": 0
                }),
            )
        };
        let terminal = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/execution/commands",
                Some(terminal),
                true,
                Some(&terminal_key),
            ))
            .await
            .unwrap();
        assert_eq!(terminal.status(), StatusCode::OK, "{terminal_type}");
        let final_revision = terminal_revision + 1;

        let replacement = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/execution/commands",
                Some(command(
                    final_revision,
                    json!({
                        "type": "start",
                        "session_id": Uuid::new_v4(),
                        "item_id": item_id,
                        "item_revision": 1,
                        "occurrence_id": null,
                        "session_index": 0,
                        "planned_block_id": Uuid::new_v4(),
                        "device_id": Uuid::new_v4()
                    }),
                )),
                true,
                Some(&retry_key),
            ))
            .await
            .unwrap();
        assert_eq!(
            replacement.status(),
            StatusCode::CONFLICT,
            "{terminal_type}"
        );
        let replacement = body_json(replacement).await;
        assert_eq!(
            replacement["error"]["code"], "execution_schedule_stale",
            "{terminal_type}"
        );
        assert!(replacement["error"].get("details").is_none());

        // Repository replay remains ahead of the semantic guard: an exact historical retry
        // returns the original success even though that semantic slot is now terminal.
        let replay = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/execution/commands",
                Some(first_start),
                true,
                Some(&start_key),
            ))
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(replay.headers()["idempotency-replayed"], "true");

        let snapshot = app
            .clone()
            .oneshot(request("GET", "/v1/execution", None, true, None))
            .await
            .unwrap();
        let snapshot = body_json(snapshot).await;
        assert_eq!(snapshot["execution"]["revision"], final_revision);
        assert!(snapshot["execution"]["active_session"].is_null());
    }
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
