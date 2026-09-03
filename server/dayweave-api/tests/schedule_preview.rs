use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
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

const TOKEN: &str = "schedule-preview-test-token";

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

#[tokio::test]
async fn schedule_routes_have_a_bounded_16_mib_override_without_widening_other_routes() {
    let app = test_app();
    let mut over_global_limit = preview(Uuid::new_v4(), 1);
    over_global_limit["availability"][0]["contexts"] = json!(["x".repeat(1024 * 1024 + 64 * 1024)]);
    let schedule = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(over_global_limit.clone()),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_ne!(schedule.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let publish = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/publish",
            Some(json!({
                "idempotency_key": Uuid::new_v4(),
                "expected_input_digest": format!("sha256:{}", "0".repeat(64)),
                "schedule": over_global_limit,
            })),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(publish.status(), StatusCode::SERVICE_UNAVAILABLE);

    let unrelated = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/suggestions",
            Some(json!({"padding": "x".repeat(1024 * 1024 + 64 * 1024)})),
            true,
            Some("unrelated-large-body"),
        ))
        .await
        .unwrap();
    assert_eq!(unrelated.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let mut over_schedule_limit = preview(Uuid::new_v4(), 1);
    over_schedule_limit["availability"][0]["contexts"] = json!(["x".repeat(16 * 1024 * 1024 + 1)]);
    let oversized_schedule = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(over_schedule_limit.clone()),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(oversized_schedule.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let oversized_publish = app
        .oneshot(request(
            "POST",
            "/v1/schedule/publish",
            Some(json!({
                "idempotency_key": Uuid::new_v4(),
                "expected_input_digest": format!("sha256:{}", "0".repeat(64)),
                "schedule": over_schedule_limit,
            })),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(oversized_publish.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

fn task(id: Uuid, constraints: &Value) -> Value {
    json!({
        "id": id,
        "is_sensitive": false,
        "kind": "task",
        "status": "planned",
        "title": "Compose the day",
        "notes": null,
        "timezone_name": "Europe/Madrid",
        "duration_seconds": 3600,
        "deadline_at": "2026-09-01T12:00:00Z",
        "earliest_start_at": null,
        "recurrence": null,
        "flexible_constraints": constraints,
        "split_policy": {"type": "indivisible"},
        "importance": 80,
        "urgency": 60,
        "parent_id": null,
        "sibling_order": 0
    })
}

fn preview(item_id: Uuid, item_revision: u64) -> Value {
    json!({
        "as_of": "2026-09-01T07:00:00Z",
        "horizon_start": "2026-09-01T00:00:00Z",
        "horizon_end": "2026-09-02T00:00:00Z",
        "timezone_name": "Europe/Madrid",
        "availability": [{
            "start": "2026-09-01T07:00:00Z",
            "end": "2026-09-01T16:00:00Z",
            "contexts": ["computer"],
            "location": "home",
            "energy": "deep"
        }],
        "previous_assignments": [{
            "item_id": item_id,
            "item_revision": item_revision,
            "occurrence_id": null,
            "blocks": [],
            "pinned": false
        }]
    })
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn preview_is_authenticated_deterministic_and_does_not_mutate_items() {
    let app = test_app();
    let valid_id = Uuid::new_v4();
    let incomplete_id = Uuid::new_v4();
    let mut unschedulable = task(incomplete_id, &json!({}));
    unschedulable["duration_seconds"] = Value::Null;
    for (item, key) in [
        (
            task(
                valid_id,
                &json!({"energy": "deep", "preferred_start_minute": 540}),
            ),
            "preview-valid-item",
        ),
        (unschedulable, "preview-invalid-item"),
    ] {
        let response = app
            .clone()
            .oneshot(request("POST", "/v1/items", Some(item), true, Some(key)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let unauthorized = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(preview(valid_id, 0)),
            false,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let first = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(preview(valid_id, 0)),
            true,
            None,
        ))
        .await
        .unwrap();
    let first_status = first.status();
    let first = body_json(first).await;
    assert_eq!(
        first_status,
        StatusCode::OK,
        "preview response body: {first}"
    );
    assert_eq!(first["source_item_count"], 2);
    assert_eq!(first["source_item_revisions"][valid_id.to_string()], 1);
    assert_eq!(first["source_item_revisions"][incomplete_id.to_string()], 1);
    assert!(first.get("source_item_sensitivity").is_none());
    assert_eq!(first["accepted_item_count"], 2);
    assert!(first["rejected_items"].as_array().unwrap().is_empty());
    assert_eq!(
        first["ignored_previous_assignments"][0]["item_id"],
        valid_id.to_string()
    );
    assert_eq!(first["plan"]["blocks"].as_array().unwrap().len(), 1);
    assert_eq!(
        first["plan"]["blocks"][0]["start"],
        "2026-09-01T09:00:00+02:00"
    );
    assert!(
        first["input_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );

    let second = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(preview(valid_id, 0)),
            true,
            None,
        ))
        .await
        .unwrap();
    let second = body_json(second).await;
    assert_eq!(first["input_digest"], second["input_digest"]);
    assert_eq!(first["plan"], second["plan"]);

    let item = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/items/{valid_id}"),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(body_json(item).await["item"]["revision"], 1);

    let mut unknown = preview(valid_id, 1);
    unknown["unexpected"] = json!(true);
    let response = app
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(unknown),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sensitive_ancestor_marks_child_preview_blocks_without_moving_them() {
    let app = test_app();
    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let mut parent = task(parent_id, &json!({}));
    parent["kind"] = json!("goal");
    parent["title"] = json!("SYNTHETIC-SENSITIVE-PARENT-CANARY");
    parent["duration_seconds"] = Value::Null;
    parent["deadline_at"] = Value::Null;
    parent["is_sensitive"] = json!(true);
    let mut child = task(
        child_id,
        &json!({"energy": "deep", "preferred_start_minute": 540}),
    );
    child["title"] = json!("SYNTHETIC-SENSITIVE-CHILD-CANARY");
    child["parent_id"] = json!(parent_id);

    for (body, key) in [
        (parent, "sensitive-preview-parent"),
        (child, "sensitive-preview-child"),
    ] {
        let response = app
            .clone()
            .oneshot(request("POST", "/v1/items", Some(body), true, Some(key)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let response = app
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(preview(child_id, 1)),
            true,
            None,
        ))
        .await
        .unwrap();
    let response_status = response.status();
    let response = body_json(response).await;
    assert_eq!(
        response_status,
        StatusCode::OK,
        "preview response body: {response}"
    );
    let block = &response["plan"]["blocks"][0];
    assert_eq!(block["item_id"], child_id.to_string());
    assert_eq!(block["is_sensitive"], true);
    assert_eq!(block["start"], "2026-09-01T09:00:00+02:00");
}

#[tokio::test]
async fn sensitive_fixed_block_is_required_and_preserved_in_preview() {
    let app = test_app();
    let canary_id = Uuid::new_v4();
    let mut body = preview(Uuid::new_v4(), 1);
    body["previous_assignments"] = json!([]);
    body["fixed_blocks"] = json!([{
        "id": canary_id,
        "is_sensitive": true,
        "title": "SYNTHETIC-SENSITIVE-FIXED-PREVIEW-CANARY",
        "start": "2026-09-01T08:00:00Z",
        "end": "2026-09-01T09:00:00Z",
        "source": "google_calendar"
    }]);

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(body.clone()),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = body_json(response).await;
    let block = &response["plan"]["blocks"][0];
    assert_eq!(block["external_block_id"], canary_id.to_string());
    assert_eq!(block["is_sensitive"], true);

    body["fixed_blocks"][0]
        .as_object_mut()
        .expect("fixed block object")
        .remove("is_sensitive");
    let missing = app
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(body),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One contract test covers every fixed-block persistence fence.
async fn fixed_block_publishability_is_rejected_before_a_client_can_journal() {
    let app = test_app();
    let item_id = Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(task(item_id, &json!({"energy": "deep"}))),
            true,
            Some("publishability-item"),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let mut body = preview(item_id, 1);
    body["previous_assignments"] = json!([]);
    let fixed_id = Uuid::new_v4();
    body["fixed_blocks"] = json!([{
        "id": fixed_id,
        "is_sensitive": false,
        "title": "é".repeat(500),
        "start": "2026-09-01T08:00:00Z",
        "end": "2026-09-01T09:00:00Z",
        "source": "manual"
    }]);
    let maximum = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(body.clone()),
            true,
            None,
        ))
        .await
        .unwrap();
    let maximum_status = maximum.status();
    let maximum = body_json(maximum).await;
    assert_eq!(
        maximum_status,
        StatusCode::OK,
        "preview response body: {maximum}"
    );

    for invalid_title in ["é".repeat(501), "contains\0nul".to_owned(), "\n".to_owned()] {
        let mut invalid = body.clone();
        invalid["fixed_blocks"][0]["title"] = json!(invalid_title);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/schedule/preview",
                Some(invalid),
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let mut duplicate = body.clone();
    duplicate["fixed_blocks"] = json!([
        duplicate["fixed_blocks"][0].clone(),
        duplicate["fixed_blocks"][0].clone()
    ]);
    let duplicate = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(duplicate),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let generated_id = maximum["plan"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["item_id"] == item_id.to_string())
        .and_then(|block| block["id"].as_str())
        .expect("preview contains a generated canonical block");
    let mut generated_collision = body.clone();
    generated_collision["fixed_blocks"][0]["id"] = json!(generated_id);
    let generated_collision = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(generated_collision),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        generated_collision.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let mut nanosecond = body;
    nanosecond["fixed_blocks"][0]["start"] = json!("2026-09-01T08:00:00.000000001Z");
    let nanosecond = app
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(nanosecond),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(nanosecond.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn durable_snapshot_limit_rejects_before_publication_journaling() {
    fn request_with_blocks(block_count: usize) -> Value {
        let base: DateTime<Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
        let title = "🧶".repeat(500);
        let fixed_blocks = (0..block_count)
            .map(|index| {
                let start = base + ChronoDuration::minutes(i64::try_from(index).unwrap());
                let end = start + ChronoDuration::minutes(1);
                json!({
                    "id": Uuid::new_v4(),
                    "is_sensitive": false,
                    "title": title,
                    "start": start.to_rfc3339_opts(SecondsFormat::Secs, true),
                    "end": end.to_rfc3339_opts(SecondsFormat::Secs, true),
                    "source": "manual"
                })
            })
            .collect::<Vec<_>>();
        json!({
            "as_of": "2026-09-01T00:00:00Z",
            "horizon_start": "2026-09-01T00:00:00Z",
            "horizon_end": "2026-11-30T00:00:00Z",
            "timezone_name": "Europe/Madrid",
            "availability": [],
            "fixed_blocks": fixed_blocks,
            "previous_assignments": [],
            "config": {
                "slot_granularity_minutes": 5,
                "stability_weight": 4,
                "default_soft_weight": 100
            },
            "recurrence_context": {}
        })
    }

    let app = test_app();
    let near_limit = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(request_with_blocks(1_500)),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(near_limit.status(), StatusCode::OK);

    let over_limit = app
        .oneshot(request(
            "POST",
            "/v1/schedule/preview",
            Some(request_with_blocks(2_500)),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(over_limit.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
