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

const TOKEN: &str = "item-api-test-token";

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

fn item_body(id: Uuid, kind: &str, title: &str, parent_id: Option<Uuid>, order: u32) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "status": "planned",
        "title": title,
        "notes": "Created offline",
        "timezone_name": "Europe/Madrid",
        "duration_seconds": 3600,
        "deadline_at": "2026-09-04T16:00:00Z",
        "earliest_start_at": "2026-09-01T08:00:00Z",
        "recurrence": {
            "type": "weekly",
            "weekdays": ["monday", "thursday"]
        },
        "flexible_constraints": {
            "preferred_start_minute": 540,
            "energy": "deep"
        },
        "split_policy": {
            "type": "splittable",
            "minimum_chunk_seconds": 1200,
            "maximum_chunk_seconds": 2400
        },
        "importance": 80,
        "urgency": 60,
        "parent_id": parent_id,
        "sibling_order": order
    })
}

fn replacement(
    body: &Value,
    expected_revision: u64,
    status: &str,
    parent_id: Option<Uuid>,
) -> Value {
    let mut item = body.clone();
    item.as_object_mut().unwrap().remove("id");
    item["status"] = json!(status);
    item["parent_id"] = json!(parent_id);
    json!({
        "expected_revision": expected_revision,
        "item": item,
    })
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn item_contract_is_authenticated_idempotent_hierarchical_and_delta_synced() {
    let app = test_app();
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let leaf_id = Uuid::new_v4();
    let root = item_body(root_id, "goal", "Ship canonical sync", None, 0);
    let child = item_body(child_id, "routine", "Implementation", Some(root_id), 1);
    let leaf = item_body(leaf_id, "task", "Write contract tests", Some(child_id), 0);

    let unauthorized = app
        .clone()
        .oneshot(request("GET", "/v1/items", None, false, None))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    for (body, key) in [
        (&root, "create-root-001"),
        (&child, "create-child-001"),
        (&leaf, "create-leaf-001"),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(body.clone()),
                true,
                Some(key),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "{body}");
        assert_eq!(response.headers()["idempotency-replayed"], "false");
    }

    let replay = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(root.clone()),
            true,
            Some("create-root-001"),
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");

    let root_response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/items/{root_id}"),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    let root_response = body_json(root_response).await;
    assert!(!root_response["item"]["is_executable"].as_bool().unwrap());
    assert_eq!(root_response["item"]["revision"], 2);

    let execute_parent = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{root_id}"),
            Some(replacement(&root, 2, "in_progress", None)),
            true,
            Some("execute-parent-001"),
        ))
        .await
        .unwrap();
    assert_eq!(execute_parent.status(), StatusCode::CONFLICT);

    let cycle = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{root_id}"),
            Some(replacement(&root, 2, "planned", Some(leaf_id))),
            true,
            Some("create-cycle-001"),
        ))
        .await
        .unwrap();
    assert_eq!(cycle.status(), StatusCode::CONFLICT);

    let first_delta = app
        .clone()
        .oneshot(request("GET", "/v1/items/delta?limit=2", None, true, None))
        .await
        .unwrap();
    assert_eq!(first_delta.status(), StatusCode::OK);
    let first_delta = body_json(first_delta).await;
    assert_eq!(first_delta["changes"].as_array().unwrap().len(), 2);
    assert_eq!(first_delta["has_more"], true);
    let cursor = first_delta["next_cursor"].as_str().unwrap();

    let second_delta = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/items/delta?limit=200&cursor={cursor}"),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    let second_delta = body_json(second_delta).await;
    assert_eq!(second_delta["changes"].as_array().unwrap().len(), 3);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/items/{leaf_id}?expected_revision=1"),
            None,
            true,
            Some("delete-leaf-001"),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted = body_json(deleted).await;
    assert_eq!(deleted["item"]["revision"], 2);
    assert!(deleted["item"]["deleted_at"].is_string());

    let tombstones = app
        .clone()
        .oneshot(request(
            "GET",
            &format!(
                "/v1/items/delta?cursor={}",
                second_delta["next_cursor"].as_str().unwrap()
            ),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    let tombstones = body_json(tombstones).await;
    assert_eq!(tombstones["changes"][0]["type"], "tombstone");
    assert_eq!(
        tombstones["changes"][0]["tombstone"]["id"],
        leaf_id.to_string()
    );

    let restored = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/items/{leaf_id}/restore"),
            Some(json!({"expected_revision": 2})),
            true,
            Some("restore-leaf-001"),
        ))
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    assert_eq!(body_json(restored).await["item"]["revision"], 3);
}

#[tokio::test]
async fn strict_item_json_and_idempotency_conflicts_are_structured() {
    let app = test_app();
    let id = Uuid::new_v4();
    let valid = item_body(id, "event", "Fixed review", None, 0);

    let missing_key = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(valid.clone()),
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_key.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(valid.clone()),
            true,
            Some("strict-create-001"),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);

    let mut different = valid;
    different["title"] = json!("Different content");
    let conflict = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(different),
            true,
            Some("strict-create-001"),
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(conflict).await["error"]["code"], "conflict");

    let mut unknown = item_body(Uuid::new_v4(), "task", "Unknown field", None, 0);
    unknown["unexpected"] = json!(true);
    let unknown = app
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(unknown),
            true,
            Some("strict-unknown-001"),
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(unknown).await["error"]["code"], "invalid_json");

    let unknown_query = test_app()
        .oneshot(request(
            "GET",
            "/v1/items?unexpected=true",
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(unknown_query.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_json(unknown_query).await["error"]["code"],
        "validation_failed"
    );
}
