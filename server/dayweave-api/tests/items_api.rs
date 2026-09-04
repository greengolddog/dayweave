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

fn scheduling_fixture(name: &str) -> Value {
    let source = match name {
        "valid-rich-items.json" => {
            include_str!("../../../fixtures/scheduling-metadata/valid-rich-items.json")
        }
        "invalid-items.json" => {
            include_str!("../../../fixtures/scheduling-metadata/invalid-items.json")
        }
        _ => panic!("unknown fixture {name}"),
    };
    serde_json::from_str(source).expect("shared scheduling fixture must be JSON")
}

fn new_item_from_fixture_fields(mut fields: Value, title: &str) -> Value {
    let object = fields.as_object_mut().expect("fixture fields object");
    let id = object.remove("item_id").expect("fixture item_id");
    object.insert("id".to_owned(), id);
    object.insert("is_sensitive".to_owned(), json!(false));
    object.insert("title".to_owned(), json!(title));
    object.insert("notes".to_owned(), Value::Null);
    object.insert("importance".to_owned(), json!(50));
    object.insert("urgency".to_owned(), json!(50));
    object.insert("sibling_order".to_owned(), json!(0));
    fields
}

fn item_body(id: Uuid, kind: &str, title: &str, parent_id: Option<Uuid>, order: u32) -> Value {
    let mut item = json!({
        "id": id,
        "is_sensitive": false,
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
    });
    match kind {
        "goal" => item["recurrence"] = Value::Null,
        "event" => {
            item["deadline_at"] = json!("2026-09-01T09:00:00Z");
            item["recurrence"] = Value::Null;
            item["flexible_constraints"] = json!({
                "calendar_event": {
                    "start": "2026-09-01T10:00:00+02:00",
                    "end": "2026-09-01T11:00:00+02:00",
                    "immutable": true,
                    "all_day": false,
                    "source_calendar_id": null
                }
            });
            item["split_policy"] = json!({"type": "indivisible"});
        }
        _ => {}
    }
    item
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
    let mut leaf = item_body(leaf_id, "task", "Write contract tests", Some(child_id), 0);
    leaf["is_sensitive"] = json!(true);

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
    // The requested count is a target: the child create and its root refresh
    // are one committed change group, so the page expands instead of exposing
    // a cursor between those two rows.
    assert_eq!(first_delta["changes"].as_array().unwrap().len(), 3);
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
    assert_eq!(second_delta["changes"].as_array().unwrap().len(), 2);
    assert!(
        second_delta["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| {
                change["type"] == "upsert"
                    && change["item"]["id"] == leaf_id.to_string()
                    && change["item"]["is_sensitive"] == true
            })
    );

    let mut stale_clear = replacement(&leaf, 2, "planned", Some(child_id));
    stale_clear["item"]["is_sensitive"] = json!(false);
    let stale_clear = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{leaf_id}"),
            Some(stale_clear),
            true,
            Some("stale-sensitive-clear-001"),
        ))
        .await
        .unwrap();
    assert_eq!(stale_clear.status(), StatusCode::CONFLICT);
    let retained = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/items/{leaf_id}"),
            None,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(body_json(retained).await["item"]["is_sensitive"], true);

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
async fn custom_rrule_writes_are_canonical_and_anchor_errors_are_structured() {
    let app = test_app();
    let valid_id = Uuid::new_v4();
    let mut valid = item_body(valid_id, "task", "Canonical custom recurrence", None, 0);
    valid["recurrence"] = json!({
        "type": "custom",
        "rrule": "rrule:count=5;byday=fr,mo;freq=weekly"
    });
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(valid),
            true,
            Some("custom-valid-001"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert_eq!(
        created["item"]["recurrence"],
        json!({
            "type": "custom",
            "rrule": "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,FR;COUNT=5"
        })
    );

    let mut expired = item_body(Uuid::new_v4(), "task", "Expired custom recurrence", None, 0);
    expired["recurrence"] = json!({
        "type": "custom",
        "rrule": "FREQ=DAILY;UNTIL=00010101"
    });
    let response = app
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(expired),
            true,
            Some("custom-expired-001"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = body_json(response).await;
    assert_eq!(error["error"]["code"], "validation_failed");
    assert_eq!(
        error["error"]["message"],
        "custom recurrence is invalid for its item creation anchor"
    );
    assert_eq!(error["error"]["details"]["field"], "recurrence.rrule");
    assert!(error["error"]["details"]["anchor_date"].is_string());
    assert_eq!(error["error"]["details"]["week_starts_on"], "monday");
    assert_eq!(
        error["error"]["details"]["validation_scope"],
        "all_supported_week_starts"
    );
    assert!(
        error["error"]["details"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("UNTIL precedes"))
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end strict JSON/idempotency boundary scenario.
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

    let mut missing_sensitive = item_body(Uuid::new_v4(), "task", "Missing privacy flag", None, 0);
    missing_sensitive
        .as_object_mut()
        .unwrap()
        .remove("is_sensitive");
    let missing_sensitive = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(missing_sensitive),
            true,
            Some("strict-sensitive-required-001"),
        ))
        .await
        .unwrap();
    assert_eq!(missing_sensitive.status(), StatusCode::BAD_REQUEST);

    let mut missing_replacement = replacement(&valid, 1, "scheduled", None);
    missing_replacement["item"]
        .as_object_mut()
        .unwrap()
        .remove("is_sensitive");
    let missing_replacement = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{id}"),
            Some(missing_replacement),
            true,
            Some("strict-replace-sensitive-required-001"),
        ))
        .await
        .unwrap();
    assert_eq!(missing_replacement.status(), StatusCode::BAD_REQUEST);

    let mut noncanonical_create =
        item_body(Uuid::new_v4(), "task", "Noncanonical timestamp", None, 0);
    noncanonical_create["deadline_at"] = json!("2026-09-04 16:00:00Z");
    let noncanonical_create = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(noncanonical_create),
            true,
            Some("strict-timestamp-create-001"),
        ))
        .await
        .unwrap();
    assert_eq!(noncanonical_create.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(noncanonical_create).await["error"]["code"],
        "invalid_json"
    );

    let mut noncanonical_replace = replacement(&valid, 1, "scheduled", None);
    noncanonical_replace["item"]["earliest_start_at"] = json!("2026-09-01 08:00:00Z");
    let noncanonical_replace = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/v1/items/{id}"),
            Some(noncanonical_replace),
            true,
            Some("strict-timestamp-replace-001"),
        ))
        .await
        .unwrap();
    assert_eq!(noncanonical_replace.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(noncanonical_replace).await["error"]["code"],
        "invalid_json"
    );

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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn shared_scheduling_metadata_contract_is_enforced_on_create_and_replace() {
    let app = test_app();
    // The shared metadata fixture deliberately exercises a typed dependency.
    // API-level graph validation additionally requires that its predecessor is
    // a real workspace identity, so establish that identity before replaying
    // the otherwise portable fixture cases.
    let mut dependency_predecessor = item_body(
        Uuid::parse_str("00000000-0000-0000-0000-000000000199")
            .expect("fixture dependency predecessor UUID"),
        "task",
        "Fixture dependency predecessor",
        None,
        0,
    );
    dependency_predecessor["recurrence"] = Value::Null;
    let predecessor_response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/items",
            Some(dependency_predecessor),
            true,
            Some("valid-fixture-dependency-predecessor"),
        ))
        .await
        .unwrap();
    assert_eq!(predecessor_response.status(), StatusCode::CREATED);
    let valid = scheduling_fixture("valid-rich-items.json");
    assert_eq!(valid["schema"], "dayweave.scheduling-metadata-fixtures/1");
    let mut saw_explicit_split_defaults = false;
    for case in valid["cases"].as_array().expect("valid cases") {
        let name = case["name"].as_str().expect("valid case name");
        let body = new_item_from_fixture_fields(case["fields"].clone(), name);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(body),
                true,
                Some(&format!("valid-{name}")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "{name}");
        if name == "indivisible_explicit_default_split_extensions" {
            saw_explicit_split_defaults = true;
            let id = case["fields"]["item_id"]
                .as_str()
                .expect("fixture item UUID");
            let mut item = new_item_from_fixture_fields(case["fields"].clone(), name);
            item.as_object_mut().expect("item object").remove("id");
            let response = app
                .clone()
                .oneshot(request(
                    "PUT",
                    &format!("/v1/items/{id}"),
                    Some(json!({"expected_revision": 1, "item": item})),
                    true,
                    Some("valid-indivisible-explicit-default-split-extensions-replace"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "replace {name}");
        }
    }
    assert!(
        saw_explicit_split_defaults,
        "shared fixtures must exercise semantic split defaults through create and replace"
    );

    let invalid = scheduling_fixture("invalid-items.json");
    assert_eq!(invalid["schema"], "dayweave.scheduling-metadata-fixtures/1");
    for case in invalid["cases"].as_array().expect("invalid cases") {
        let name = case["name"].as_str().expect("invalid case name");
        let expected = case["expected_error_contains"]
            .as_str()
            .expect("expected error fragment");
        let invalid_body = new_item_from_fixture_fields(case["fields"].clone(), name);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(invalid_body.clone()),
                true,
                Some(&format!("invalid-create-{name}")),
            ))
            .await
            .unwrap();
        let status = response.status();
        let error = body_json(response).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "create {name}: {error}"
        );
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "create {name}: expected {expected:?}, got {error}"
        );

        let id = invalid_body["id"].clone();
        let seed = json!({
            "id": id,
            "is_sensitive": false,
            "kind": "task",
            "status": "inbox",
            "title": format!("seed-{name}"),
            "notes": null,
            "timezone_name": "UTC",
            "duration_seconds": null,
            "deadline_at": null,
            "earliest_start_at": null,
            "recurrence": null,
            "flexible_constraints": {},
            "split_policy": {"type": "indivisible"},
            "importance": 0,
            "urgency": 0,
            "parent_id": null,
            "sibling_order": 0
        });
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/items",
                Some(seed),
                true,
                Some(&format!("invalid-seed-{name}")),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "seed {name}");

        let mut replacement = invalid_body;
        replacement.as_object_mut().unwrap().remove("id");
        let id = id.as_str().expect("fixture UUID");
        let response = app
            .clone()
            .oneshot(request(
                "PUT",
                &format!("/v1/items/{id}"),
                Some(json!({"expected_revision": 1, "item": replacement})),
                true,
                Some(&format!("invalid-replace-{name}")),
            ))
            .await
            .unwrap();
        let status = response.status();
        let error = body_json(response).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "replace {name}: {error}"
        );
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "replace {name}: expected {expected:?}, got {error}"
        );

        // Check the public error remains useful without reflecting item content.
        let response = app
            .clone()
            .oneshot(request("GET", &format!("/v1/items/{id}"), None, true, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
