use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Timelike as _, Utc};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    habits::{HabitOccurrenceEvidence, InMemoryHabitRepository},
    http::router,
    items::{InMemoryItemRepository, ItemService},
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "habit-api-test-token";
const PRIVATE_NOTE: &str = "SYNTHETIC-PRIVATE-HABIT-NOTE";

fn test_app() -> (Router, Arc<InMemoryHabitRepository>) {
    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let items = Arc::new(ItemService::new(
        Arc::new(InMemoryItemRepository::default()),
        Arc::new(SystemClock),
    ));
    let habits = Arc::new(InMemoryHabitRepository::default());
    let state = AppState::new(
        proposals,
        Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
        readiness,
    )
    .with_items(items)
    .with_habit_repository(habits.clone());
    (router(state), habits)
}

fn request(method: &str, uri: &str, body: Option<Value>, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("valid request")
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

fn habit_body(habit_id: Uuid) -> Value {
    json!({
        "id": habit_id,
        "is_sensitive": true,
        "kind": "habit",
        "status": "planned",
        "title": "Private reading habit",
        "notes": null,
        "timezone_name": "Europe/Paris",
        "duration_seconds": 1800,
        "deadline_at": null,
        "earliest_start_at": null,
        "recurrence": { "type": "daily", "times_per_day": 1 },
        "flexible_constraints": {
            "habit_target": { "amount": 20, "unit": "pages" },
            "preserves_streak_when_paused": true
        },
        "split_policy": { "type": "indivisible" },
        "importance": 80,
        "urgency": 60,
        "parent_id": null,
        "sibling_order": 0
    })
}

fn outcome(
    operation_id: Uuid,
    expected_revision: u64,
    status: &str,
    progress_basis_points: u16,
    now: DateTime<Utc>,
) -> Value {
    let (quantity, unit, actual_seconds, note) = if status == "unresolved" {
        (Value::Null, Value::Null, Value::Null, Value::Null)
    } else {
        (json!(10), json!("pages"), json!(900), json!(PRIVATE_NOTE))
    };
    json!({
        "operation_id": operation_id,
        "expected_revision": expected_revision,
        "outcome": {
            "status": status,
            "progress_basis_points": progress_basis_points,
            "quantity": quantity,
            "unit": unit,
            "actual_seconds": actual_seconds,
            "note": note,
            "occurred_at": now,
        }
    })
}

fn postgres_now() -> DateTime<Utc> {
    Utc::now().with_nanosecond(0).expect("whole second")
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn habit_api_is_authoritative_retryable_revisioned_private_and_analytics_safe() {
    let (app, repository) = test_app();
    let habit_id = Uuid::new_v4();
    let evidence_id = Uuid::new_v4();
    let planner_occurrence_id = Uuid::new_v4();
    let create = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(habit_body(habit_id)),
            Some("habit-create-001"),
        ))
        .await
        .expect("item response");
    assert_eq!(
        create.status(),
        StatusCode::CREATED,
        "{:?}",
        body_json(create).await
    );

    let local_date = Utc::now().date_naive();
    let nominal_start = local_date.and_hms_opt(8, 0, 0).expect("time").and_utc();
    repository
        .insert_authoritative_occurrence(HabitOccurrenceEvidence {
            id: evidence_id,
            habit_id,
            planner_occurrence_id,
            source_schedule_revision_id: Uuid::new_v4(),
            source_item_revision: 1,
            policy_fingerprint: format!("sha256:{}", "a".repeat(64)),
            identity: json!({"type":"daily","date":local_date,"ordinal":0}),
            nominal_start,
            nominal_end: nominal_start + ChronoDuration::minutes(30),
            window_start: nominal_start - ChronoDuration::hours(1),
            window_end: nominal_start + ChronoDuration::hours(1),
            local_date,
            timezone_name: "Europe/Paris".to_owned(),
            expected_duration_seconds: Some(1800),
            expected_quantity: Some(20),
            expected_unit: Some("pages".to_owned()),
        })
        .await
        .expect("trusted evidence seed");

    let unknown = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{}", Uuid::new_v4()),
            Some(outcome(Uuid::new_v4(), 0, "partial", 5_000, postgres_now())),
            Some("habit-unknown-001"),
        ))
        .await
        .expect("unknown evidence response");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    // The scheduler occurrence UUID is a join key, never an HTTP write identity.
    let planner_id_write = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{planner_occurrence_id}"),
            Some(outcome(Uuid::new_v4(), 0, "partial", 5_000, postgres_now())),
            Some("habit-planner-id-001"),
        ))
        .await
        .expect("planner identity response");
    assert_eq!(planner_id_write.status(), StatusCode::NOT_FOUND);

    let operation_id = Uuid::new_v4();
    let partial_body = outcome(operation_id, 0, "partial", 5_000, postgres_now());
    let partial = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body.clone()),
            Some("habit-outcome-001"),
        ))
        .await
        .expect("partial response");
    assert_eq!(partial.status(), StatusCode::OK);
    assert_eq!(partial.headers()["idempotency-replayed"], "false");
    assert_eq!(
        partial.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let partial_json = body_json(partial).await;
    assert_eq!(
        partial_json["occurrence"]["evidence"]["id"],
        json!(evidence_id)
    );
    assert_eq!(
        partial_json["occurrence"]["evidence"]["planner_occurrence_id"],
        json!(planner_occurrence_id)
    );
    assert_eq!(partial_json["occurrence"]["outcome"]["revision"], 1);

    let replay = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body.clone()),
            Some("habit-outcome-001"),
        ))
        .await
        .expect("replay response");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert_eq!(body_json(replay).await["replayed"], true);

    let reused_operation = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(outcome(
                operation_id,
                1,
                "completed",
                10_000,
                postgres_now(),
            )),
            Some("habit-outcome-different-key-001"),
        ))
        .await
        .expect("operation collision response");
    assert_eq!(reused_operation.status(), StatusCode::CONFLICT);

    let cross_route_operation = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(json!({
                "operation_id": operation_id,
                "pause_id": Uuid::new_v4(),
                "expected_revision": 0,
                "started_at": postgres_now() - ChronoDuration::minutes(1),
            })),
            Some("habit-cross-operation-001"),
        ))
        .await
        .expect("cross-route operation collision response");
    assert_eq!(cross_route_operation.status(), StatusCode::CONFLICT);

    let completed = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(outcome(
                Uuid::new_v4(),
                1,
                "completed",
                10_000,
                postgres_now(),
            )),
            Some("habit-outcome-002"),
        ))
        .await
        .expect("correction response");
    assert_eq!(completed.status(), StatusCode::OK);
    assert_eq!(
        body_json(completed).await["occurrence"]["outcome"]["revision"],
        2
    );

    let stale = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(outcome(Uuid::new_v4(), 1, "skipped", 5_000, postgres_now())),
            Some("habit-outcome-stale-001"),
        ))
        .await
        .expect("stale response");
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale_json = body_json(stale).await;
    assert_eq!(stale_json["error"]["details"]["actual_revision"], 2);
    assert_eq!(
        stale_json["error"]["details"]["current_occurrence"]["outcome"]["note"],
        PRIVATE_NOTE
    );

    let list = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/occurrences?start_date={local_date}&end_date={local_date}&limit=1"
            ),
            None,
            None,
        ))
        .await
        .expect("list response");
    assert_eq!(list.status(), StatusCode::OK);
    let list_json = body_json(list).await;
    assert_eq!(list_json["occurrences"].as_array().map(Vec::len), Some(1));
    assert_eq!(list_json["has_more"], false);

    let analytics = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/analytics?start_date={local_date}&end_date={local_date}&bucket=day"
            ),
            None,
            None,
        ))
        .await
        .expect("analytics response");
    assert_eq!(analytics.status(), StatusCode::OK);
    let analytics_text = body_json(analytics).await;
    assert_eq!(analytics_text["analytics"]["completed"], 1);
    assert_eq!(
        analytics_text["analytics"]["adherence_basis_points"],
        10_000
    );
    assert!(!analytics_text.to_string().contains(PRIVATE_NOTE));

    let invalid_range = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/habits/{habit_id}/occurrences?start_date=1899-12-31&end_date=1899-12-31"),
            None,
            None,
        ))
        .await
        .expect("invalid date response");
    assert_eq!(invalid_range.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_json(invalid_range).await["error"]["code"],
        "validation_failed"
    );

    let delta = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/habits/occurrences/delta?limit=200",
            None,
            None,
        ))
        .await
        .expect("delta response");
    assert_eq!(delta.status(), StatusCode::OK);
    let delta_json = body_json(delta).await;
    assert_eq!(delta_json["changes"].as_array().map(Vec::len), Some(3));
    let cursor = delta_json["next_cursor"]
        .as_str()
        .expect("opaque cursor")
        .to_owned();
    assert!(delta_json.to_string().contains(PRIVATE_NOTE));

    let mut stream_request = request("GET", "/v1/habits/stream", None, None);
    stream_request.headers_mut().insert(
        header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );
    let mut stream = app
        .clone()
        .oneshot(stream_request)
        .await
        .expect("stream response");
    assert_eq!(stream.status(), StatusCode::OK);
    let frame = timeout(Duration::from_secs(1), stream.body_mut().frame())
        .await
        .expect("immediate durable invalidation")
        .expect("stream remains open")
        .expect("valid stream frame")
        .into_data()
        .expect("data frame");
    let frame = String::from_utf8(frame.to_vec()).expect("utf8 sse");
    assert!(frame.contains("event: habit-invalidation"));
    assert!(frame.contains("\"cursor\""));
    assert!(!frame.contains(PRIVATE_NOTE));
    assert!(!frame.contains(&habit_id.to_string()));
    assert!(!frame.contains(&evidence_id.to_string()));

    let resumed_stream = {
        let mut request = request("GET", "/v1/habits/stream", None, None);
        request.headers_mut().insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        request.headers_mut().insert(
            "last-event-id",
            HeaderValue::from_str(&cursor).expect("cursor header"),
        );
        app.clone().oneshot(request).await.expect("resumed stream")
    };
    assert_eq!(resumed_stream.status(), StatusCode::OK);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{habit_id}?expected_revision=1"),
            None,
            Some("habit-delete-001"),
        ))
        .await
        .expect("habit delete response");
    assert_eq!(deleted.status(), StatusCode::OK);
    let historical_replay = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body),
            Some("habit-outcome-001"),
        ))
        .await
        .expect("historical replay response");
    assert_eq!(historical_replay.status(), StatusCode::OK);
    assert_eq!(historical_replay.headers()["idempotency-replayed"], "true");
    assert_eq!(
        body_json(historical_replay).await["occurrence"]["outcome"]["revision"],
        1
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn habit_pause_is_single_open_revisioned_and_validated() {
    let (app, _) = test_app();
    let habit_id = Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(habit_body(habit_id)),
            Some("habit-create-pause-001"),
        ))
        .await
        .expect("item response");
    assert_eq!(created.status(), StatusCode::CREATED);

    let pause_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let started_at = postgres_now() - ChronoDuration::minutes(5);
    let body = json!({
        "operation_id": operation_id,
        "pause_id": pause_id,
        "expected_revision": 0,
        "started_at": started_at,
    });
    let started = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(body.clone()),
            Some("habit-pause-001"),
        ))
        .await
        .expect("pause response");
    assert_eq!(started.status(), StatusCode::OK);
    let started_json = body_json(started).await;
    assert_eq!(started_json["pause"]["revision"], 1);
    assert_eq!(started_json["pause"]["preserves_streak"], true);

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(body),
            Some("habit-pause-001"),
        ))
        .await
        .expect("pause replay");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");

    let second = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "pause_id": Uuid::new_v4(),
                "expected_revision": 0,
                "started_at": postgres_now(),
            })),
            Some("habit-pause-002"),
        ))
        .await
        .expect("second pause response");
    assert_eq!(second.status(), StatusCode::CONFLICT);

    let invalid_resume = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(json!({
                "operation_id": Uuid::new_v4(),
                "expected_revision": 1,
                "ended_at": started_at,
            })),
            Some("habit-pause-invalid-resume-001"),
        ))
        .await
        .expect("invalid resume response");
    assert_eq!(invalid_resume.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let resume_body = json!({
        "operation_id": Uuid::new_v4(),
        "expected_revision": 1,
        "ended_at": postgres_now(),
    });
    let resumed = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(resume_body.clone()),
            Some("habit-pause-resume-001"),
        ))
        .await
        .expect("resume response");
    assert_eq!(resumed.status(), StatusCode::OK);
    assert_eq!(body_json(resumed).await["pause"]["revision"], 2);

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(resume_body),
            Some("habit-pause-resume-001"),
        ))
        .await
        .expect("resume replay");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
}

#[test]
fn evidence_identity_contract_is_distinct_and_strict() {
    let local_date = NaiveDate::from_ymd_opt(2026, 10, 25).expect("DST date");
    let value = json!({
        "id": Uuid::new_v4(),
        "habit_id": Uuid::new_v4(),
        "planner_occurrence_id": Uuid::new_v4(),
        "source_schedule_revision_id": Uuid::new_v4(),
        "source_item_revision": 7,
        "policy_fingerprint": format!("sha256:{}", "0".repeat(64)),
        "identity": {"type":"daily","date":local_date,"ordinal":0},
        "nominal_start": "2026-10-25T07:00:00Z",
        "nominal_end": "2026-10-25T07:30:00Z",
        "window_start": "2026-10-25T06:00:00Z",
        "window_end": "2026-10-25T08:00:00Z",
        "local_date": local_date,
        "timezone_name": "Europe/Paris",
        "expected_duration_seconds": 1800,
        "expected_quantity": null,
        "expected_unit": null,
    });
    let parsed: HabitOccurrenceEvidence = serde_json::from_value(value.clone()).expect("evidence");
    assert_ne!(parsed.id, parsed.planner_occurrence_id);
    let mut unknown = value;
    unknown["invented"] = json!(true);
    assert!(serde_json::from_value::<HabitOccurrenceEvidence>(unknown).is_err());
}
