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
    let invalid_id = Uuid::new_v4();
    for (item, key) in [
        (
            task(
                valid_id,
                &json!({"energy": "deep", "preferred_start_minute": 540}),
            ),
            "preview-valid-item",
        ),
        (
            task(invalid_id, &json!({"unknown_constraint": true})),
            "preview-invalid-item",
        ),
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
    assert_eq!(first.status(), StatusCode::OK);
    let first = body_json(first).await;
    assert_eq!(first["source_item_count"], 2);
    assert_eq!(first["source_item_revisions"][valid_id.to_string()], 1);
    assert_eq!(first["source_item_revisions"][invalid_id.to_string()], 1);
    assert_eq!(first["accepted_item_count"], 1);
    assert_eq!(
        first["rejected_items"][0]["item_id"],
        invalid_id.to_string()
    );
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
    assert_eq!(response.status(), StatusCode::OK);
    let response = body_json(response).await;
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
