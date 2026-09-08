use std::{str::FromStr as _, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    items::ItemService,
    persistence::{DatabaseScope, MIGRATOR, PostgresItemRepository},
    proposals::{InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, ConnectOptions as _, Executor as _, postgres::PgConnectOptions};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "structural-item-api-test-token";

fn test_app(items: Option<Arc<ItemService>>) -> Router {
    let proposals = Arc::new(ProposalService::new(
        Arc::new(InMemoryProposalRepository::default()),
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let mut state = AppState::new(
        proposals,
        Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
        readiness,
    );
    if let Some(items) = items {
        state = state.with_items(items);
    }
    router(state)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: Option<&str>,
    expected: StatusCode,
) -> Value {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .expect("valid structural request"),
        )
        .await
        .expect("structural item response");
    let status = response.status();
    let replayed = response.headers().get("idempotency-replayed").cloned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let mut value: Value = serde_json::from_slice(&bytes).expect("JSON item response");
    assert_eq!(status, expected, "{method} {uri}: {value}");
    if let Some(replayed) = replayed {
        value["test_replayed"] = json!(replayed.to_str().unwrap() == "true");
    }
    value
}

fn project(id: Uuid, parent: Option<Uuid>) -> Value {
    json!({
        "id": id, "is_sensitive": false, "kind": "project", "status": "planned",
        "title": "Structural project", "notes": "Preserve authored structure",
        "timezone_name": "Europe/Istanbul",
        "duration_kind": "unknown", "duration_seconds": null,
        "duration_min_seconds": null, "duration_max_seconds": null, "duration_source": null,
        "deadline_kind": "none", "deadline_date": null, "deadline_at": null,
        "deadline_strength": null, "deadline_soft_weight": null, "earliest_start_at": null,
        "recurrence": null, "flexible_constraints": {}, "has_own_effort": false,
        "split_policy": {"type": "indivisible"}, "importance": 70, "urgency": 35,
        "parent_id": parent, "sibling_order": 0,
        "blocked_reason_kind": null, "blocked_by_item_id": null, "blocked_reason": null
    })
}

fn replacement(body: &Value, revision: u64) -> Value {
    let mut item = body.clone();
    item.as_object_mut().unwrap().remove("id");
    json!({"expected_revision": revision, "item": item})
}

async fn create(app: &Router, body: &Value, key: &str) -> Value {
    call(
        app,
        "POST",
        "/v1/items",
        Some(body.clone()),
        Some(key),
        StatusCode::CREATED,
    )
    .await
}

async fn get(app: &Router, id: Uuid) -> Value {
    call(
        app,
        "GET",
        &format!("/v1/items/{id}"),
        None,
        None,
        StatusCode::OK,
    )
    .await["item"]
        .clone()
}

async fn delta(app: &Router, cursor: Option<&str>, limit: u32) -> Value {
    let mut uri = format!("/v1/items/delta?limit={limit}");
    if let Some(cursor) = cursor {
        uri.push_str("&cursor=");
        uri.push_str(cursor);
    }
    call(app, "GET", &uri, None, None, StatusCode::OK).await
}

async fn head(app: &Router) -> String {
    let mut page = delta(app, None, 200).await;
    while page["has_more"] == true {
        page = delta(app, page["next_cursor"].as_str(), 200).await;
    }
    page["next_cursor"].as_str().unwrap().to_owned()
}

fn assert_authored_fields(body: &Value, item: &Value) {
    for (key, value) in body.as_object().unwrap() {
        assert_eq!(&item[key], value, "authored field {key}");
    }
}

#[tokio::test]
async fn shared_structural_authoring_fixture_matches_create_and_replace_contracts() {
    shared_structural_fixture(&test_app(None)).await;
}

async fn shared_structural_fixture(app: &Router) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../fixtures/structural-authoring/requests-v1.json"
    ))
    .expect("shared structural authoring fixture");
    assert_eq!(
        fixture["schema"],
        "dayweave.structural-authoring-fixtures/1"
    );
    let valid = fixture["cases"].as_array().unwrap();
    let invalid = fixture["invalid_cases"].as_array().unwrap();
    assert_eq!(valid.len(), 11, "all shared positive cases stay covered");
    assert_eq!(invalid.len(), 11, "all shared negative cases stay covered");
    for case in valid {
        let name = case["name"].as_str().unwrap();
        let body = &case["create"];
        let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
        let mut normalized = body.clone();
        if let Some(constraints) = case.get("normalized_flexible_constraints") {
            normalized["flexible_constraints"] = constraints.clone();
        }
        let created = create(app, body, &format!("structural-fixture-create-{name}")).await;
        assert_authored_fields(&normalized, &created["item"]);
        let replaced = call(
            app,
            "PUT",
            &format!("/v1/items/{id}"),
            Some(replacement(body, 1)),
            Some(&format!("structural-fixture-replace-{name}")),
            StatusCode::OK,
        )
        .await;
        assert_authored_fields(&normalized, &replaced["item"]);
        assert_eq!(replaced["item"]["revision"], 2, "{name}");
        assert_eq!(get(app, id).await, replaced["item"], "{name}");
        if body["kind"] == "project" || body["kind"] == "goal" {
            assert_eq!(replaced["item"]["is_executable"], body["has_own_effort"]);
        }
    }
    for case in invalid {
        reject_shared_structural_case(app, case).await;
    }
}

async fn reject_shared_structural_case(app: &Router, case: &Value) {
    let name = case["name"].as_str().unwrap();
    let body = &case["create"];
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    // An impossible civil date is rejected by strict JSON deserialization;
    // well-formed but contradictory structural shapes reach domain validation.
    let status = if name == "invalid_civil_date" {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let cursor = head(app).await;
    call(
        app,
        "POST",
        "/v1/items",
        Some(body.clone()),
        Some(&format!("structural-invalid-create-{name}")),
        status,
    )
    .await;
    assert_eq!(
        head(app).await,
        cursor,
        "invalid CREATE {name} has no delta"
    );
    call(
        app,
        "GET",
        &format!("/v1/items/{id}"),
        None,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    let original = create(
        app,
        &project(id, None),
        &format!("structural-invalid-seed-{name}"),
    )
    .await;
    let cursor = head(app).await;
    call(
        app,
        "PUT",
        &format!("/v1/items/{id}"),
        Some(replacement(body, 1)),
        Some(&format!("structural-invalid-replace-{name}")),
        status,
    )
    .await;
    assert_eq!(
        get(app, id).await,
        original["item"],
        "invalid REPLACE {name}"
    );
    assert_eq!(
        head(app).await,
        cursor,
        "invalid REPLACE {name} has no delta"
    );
}

#[tokio::test]
async fn modern_project_fields_round_trip_through_create_and_full_replace() {
    modern_project_round_trip(&test_app(None)).await;
}

async fn modern_project_round_trip(app: &Router) {
    let id = Uuid::new_v4();
    let mut body = project(id, None);
    body["deadline_kind"] = json!("date");
    body["deadline_date"] = json!("2026-10-01");
    body["deadline_strength"] = json!("soft");
    body["deadline_soft_weight"] = json!(125);
    let created = create(app, &body, "modern-project-create").await;
    assert_authored_fields(&body, &created["item"]);
    assert_eq!(created["item"]["revision"], 1);
    assert_eq!(created["item"]["is_executable"], false);

    body["title"] = json!("Project with independent leaf effort");
    body["duration_kind"] = json!("range");
    body["duration_seconds"] = json!(3_600);
    body["duration_min_seconds"] = json!(1_800);
    body["duration_max_seconds"] = json!(7_200);
    body["duration_source"] = json!("assistant");
    body["has_own_effort"] = json!(true);
    body["flexible_constraints"] = json!({"has_own_effort": true, "energy": "deep"});
    body["deadline_kind"] = json!("date_time");
    body["deadline_date"] = Value::Null;
    body["deadline_at"] = json!("2026-10-02T16:00:00Z");
    body["deadline_strength"] = json!("hard");
    body["deadline_soft_weight"] = Value::Null;
    let update = replacement(&body, 1);
    let replaced = call(
        app,
        "PUT",
        &format!("/v1/items/{id}"),
        Some(update.clone()),
        Some("modern-project-replace"),
        StatusCode::OK,
    )
    .await;
    assert_authored_fields(&body, &replaced["item"]);
    assert_eq!(replaced["item"]["revision"], 2);
    assert_eq!(replaced["item"]["is_executable"], true);
    let cursor = head(app).await;
    let replay = call(
        app,
        "PUT",
        &format!("/v1/items/{id}"),
        Some(update),
        Some("modern-project-replace"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(replay["test_replayed"], true);
    assert_eq!(replay["item"], replaced["item"]);
    assert_eq!(head(app).await, cursor);

    let cleared = project(id, None);
    let replaced = call(
        app,
        "PUT",
        &format!("/v1/items/{id}"),
        Some(replacement(&cleared, 2)),
        Some("modern-project-clear"),
        StatusCode::OK,
    )
    .await;
    assert_authored_fields(&cleared, &replaced["item"]);
    assert_eq!(replaced["item"]["is_executable"], false);
    assert_eq!(replaced["item"]["revision"], 3);
    assert_eq!(get(app, id).await, replaced["item"]);
}

#[tokio::test]
async fn project_nesting_refreshes_parents_atomically_and_replays_original_responses() {
    nested_project_lifecycle(&test_app(None)).await;
}

#[allow(clippy::too_many_lines)] // One lifecycle proves revisions, atomic groups, and replay ordering together.
async fn nested_project_lifecycle(app: &Router) {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let leaf_id = Uuid::new_v4();
    let mut root = project(root_id, None);
    root["has_own_effort"] = json!(true);
    root["flexible_constraints"] = json!({"has_own_effort": true});
    let original = create(app, &root, "nested-project-root").await;
    assert_eq!(original["item"]["is_executable"], true);
    let root_cursor = head(app).await;
    let mut child = project(child_id, Some(root_id));
    child["has_own_effort"] = json!(true);
    child["flexible_constraints"] = json!({"has_own_effort": true});
    create(app, &child, "nested-project-child").await;
    let child_group = delta(app, Some(&root_cursor), 1).await;
    assert_eq!(child_group["changes"].as_array().unwrap().len(), 2);
    assert_eq!(child_group["changes"][0]["item"]["id"], json!(child_id));
    assert_eq!(child_group["changes"][1]["item"]["id"], json!(root_id));
    assert_eq!(child_group["changes"][1]["item"]["revision"], 2);
    assert_eq!(child_group["changes"][1]["item"]["is_executable"], false);
    assert_eq!(child_group["has_more"], false);

    let mut leaf = project(leaf_id, Some(child_id));
    leaf["kind"] = json!("task");
    create(app, &leaf, "nested-project-leaf").await;
    let leaf_group = delta(app, child_group["next_cursor"].as_str(), 1).await;
    assert_eq!(leaf_group["changes"].as_array().unwrap().len(), 2);
    assert_eq!(leaf_group["changes"][0]["item"]["id"], json!(leaf_id));
    assert_eq!(leaf_group["changes"][1]["item"]["id"], json!(child_id));
    assert_eq!(leaf_group["changes"][1]["item"]["revision"], 2);
    assert_eq!(leaf_group["changes"][1]["item"]["is_executable"], false);
    assert_eq!(
        get(app, root_id).await["revision"],
        2,
        "only direct parents refresh"
    );
    assert_eq!(get(app, leaf_id).await["is_executable"], true);

    let replay = create(app, &root, "nested-project-root").await;
    assert_eq!(replay["test_replayed"], true);
    assert_eq!(
        replay["item"], original["item"],
        "replay is historical, not a new snapshot"
    );
    assert_eq!(get(app, root_id).await["is_executable"], false);
    let before_rejections = head(app).await;
    let stale = call(
        app,
        "PUT",
        &format!("/v1/items/{child_id}"),
        Some(replacement(&child, 1)),
        Some("nested-project-stale"),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(stale["error"]["details"]["actual_revision"], 2);
    let mut cycle = root.clone();
    cycle["parent_id"] = json!(leaf_id);
    call(
        app,
        "PUT",
        &format!("/v1/items/{root_id}"),
        Some(replacement(&cycle, 2)),
        Some("nested-project-cycle"),
        StatusCode::CONFLICT,
    )
    .await;
    let mut executing = child.clone();
    executing["status"] = json!("in_progress");
    call(
        app,
        "PUT",
        &format!("/v1/items/{child_id}"),
        Some(replacement(&executing, 2)),
        Some("nested-project-execute-parent"),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(head(app).await, before_rejections);

    child["title"] = json!("Renamed nested project");
    let renamed = call(
        app,
        "PUT",
        &format!("/v1/items/{child_id}"),
        Some(replacement(&child, 2)),
        Some("nested-project-rename"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(renamed["item"]["parent_id"], json!(root_id));
    assert_eq!(renamed["item"]["is_executable"], false);
    assert_eq!(renamed["item"]["revision"], 3);

    let before_move = head(app).await;
    leaf["parent_id"] = json!(root_id);
    leaf["sibling_order"] = json!(7);
    call(
        app,
        "PUT",
        &format!("/v1/items/{leaf_id}"),
        Some(replacement(&leaf, 1)),
        Some("nested-project-move"),
        StatusCode::OK,
    )
    .await;
    let moved = delta(app, Some(&before_move), 1).await;
    let mut refreshed = moved["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["item"]["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    refreshed.sort_unstable();
    let mut expected = vec![
        root_id.to_string(),
        child_id.to_string(),
        leaf_id.to_string(),
    ];
    expected.sort_unstable();
    assert_eq!(
        refreshed, expected,
        "old and new parent refreshes share one group"
    );
    assert_eq!(moved["has_more"], false);
    assert_eq!(get(app, root_id).await["revision"], 3);
    assert_eq!(get(app, child_id).await["revision"], 4);
    assert_eq!(get(app, child_id).await["is_executable"], true);

    leaf["parent_id"] = Value::Null;
    call(
        app,
        "PUT",
        &format!("/v1/items/{leaf_id}"),
        Some(replacement(&leaf, 2)),
        Some("nested-project-detach"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(get(app, leaf_id).await["parent_id"], Value::Null);
    assert_eq!(get(app, root_id).await["revision"], 4);
    assert_eq!(
        get(app, root_id).await["is_executable"],
        false,
        "nested project is still a child"
    );
}

#[tokio::test]
async fn modern_project_invalid_fields_reject_create_and_replace_without_side_effects() {
    invalid_project_fields(&test_app(None)).await;
}

async fn invalid_project_fields(app: &Router) {
    let id = Uuid::new_v4();
    let body = project(id, None);
    let original = create(app, &body, "invalid-project-baseline").await;
    let cursor = head(app).await;
    let invalid = [
        (
            "recurrence",
            json!({"recurrence": {"type": "daily", "times_per_day": 1}}),
        ),
        (
            "effort",
            json!({"has_own_effort": true, "flexible_constraints": {"has_own_effort": false}}),
        ),
        (
            "unknown-duration",
            json!({"duration_kind": "unknown", "duration_seconds": 60}),
        ),
        (
            "range-duration",
            json!({"duration_kind": "range", "duration_seconds": 60,
            "duration_min_seconds": 90, "duration_max_seconds": 120, "duration_source": "user"}),
        ),
        (
            "date-and-timestamp",
            json!({"deadline_kind": "date", "deadline_date": "2026-10-01",
            "deadline_at": "2026-10-01T12:00:00Z", "deadline_strength": "hard"}),
        ),
        (
            "soft-weight-required",
            json!({"deadline_kind": "date", "deadline_date": "2026-10-01",
            "deadline_strength": "soft"}),
        ),
        (
            "hard-weight-forbidden",
            json!({"deadline_kind": "date_time", "deadline_at": "2026-10-01T12:00:00Z",
            "deadline_strength": "hard", "deadline_soft_weight": 1}),
        ),
    ];
    for (name, fields) in invalid {
        let mut invalid = body.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        invalid["id"] = json!(Uuid::new_v4());
        let rejected = call(
            app,
            "POST",
            "/v1/items",
            Some(invalid.clone()),
            Some(&format!("invalid-project-create-{name}")),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        if name == "recurrence" {
            assert!(
                rejected["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("project does not support recurrence")
            );
        }
        call(
            app,
            "PUT",
            &format!("/v1/items/{id}"),
            Some(replacement(&invalid, 1)),
            Some(&format!("invalid-project-replace-{name}")),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(
            get(app, id).await,
            original["item"],
            "{name} cannot modify the item"
        );
        assert_eq!(head(app).await, cursor, "{name} cannot emit a delta");
    }
}

#[tokio::test]
async fn nested_creates_require_a_committed_parent_and_accept_blocked_projects() {
    parent_admission(&test_app(None)).await;
}

async fn parent_admission(app: &Router) {
    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let mut parent = project(parent_id, None);
    parent["status"] = json!("blocked");
    parent["blocked_reason_kind"] = json!("manual");
    parent["blocked_reason"] = json!("Waiting for research");
    let child = project(child_id, Some(parent_id));
    let cursor = head(app).await;
    let rejected = call(
        app,
        "POST",
        "/v1/items",
        Some(child.clone()),
        Some("ordered-project-child"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(rejected["error"]["details"]["parent_id"], json!(parent_id));
    assert_eq!(head(app).await, cursor);
    create(app, &parent, "ordered-project-parent").await;
    // A failed request cannot reserve the key or fabricate the not-yet-created parent.
    let created = create(app, &child, "ordered-project-child").await;
    assert_eq!(created["test_replayed"], false);
    assert_eq!(created["item"]["parent_id"], json!(parent_id));
    assert_eq!(get(app, parent_id).await["status"], "blocked");
    assert_eq!(get(app, parent_id).await["revision"], 2);

    for status in [
        "scheduled",
        "in_progress",
        "paused",
        "completed",
        "skipped",
        "cancelled",
    ] {
        let parent_id = Uuid::new_v4();
        let mut unavailable = project(parent_id, None);
        unavailable["status"] = json!(status);
        create(app, &unavailable, &format!("unavailable-project-{status}")).await;
        call(
            app,
            "POST",
            "/v1/items",
            Some(project(Uuid::new_v4(), Some(parent_id))),
            Some(&format!("unavailable-project-child-{status}")),
            StatusCode::CONFLICT,
        )
        .await;
        assert_eq!(get(app, parent_id).await["revision"], 1);
    }
}

#[tokio::test]
async fn postgres_structural_project_http_contract_matches_memory() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL unset; structural Project PostgreSQL test skipped");
        return;
    };
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .disable_statement_logging();
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .expect("connect test PostgreSQL");
    let schema = format!("dayweave_structural_api_{}", Uuid::new_v4().simple());
    admin
        .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .await
        .unwrap();
    let connection_schema = schema.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .after_connect(move |connection, _| {
            let statement = format!("SET search_path TO {connection_schema}");
            Box::pin(async move {
                connection
                    .execute(AssertSqlSafe(statement))
                    .await
                    .map(|_| ())
            })
        })
        .connect_with(options)
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query("INSERT INTO users (id, auth_subject, display_name, timezone_name) VALUES ($1,$2,'Synthetic structural test','UTC')")
        .bind(scope.user_id).bind(format!("structural-{}", scope.user_id)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) VALUES ($1,$2,$3,'Synthetic structural test','UTC')")
        .bind(scope.workspace_id).bind(scope.user_id).bind(format!("structural-{}", scope.workspace_id)).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1,$2,'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&pool)
    .await
    .unwrap();
    let app = test_app(Some(Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        Arc::new(SystemClock),
    ))));
    // Destroy only this test's UUID-named schema, even when an assertion fails.
    let scenario = tokio::spawn(async move {
        shared_structural_fixture(&app).await;
        modern_project_round_trip(&app).await;
        nested_project_lifecycle(&app).await;
        invalid_project_fields(&app).await;
        parent_admission(&app).await;
    })
    .await;
    pool.close().await;
    admin
        .execute(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .await
        .unwrap();
    admin.close().await;
    scenario.expect("PostgreSQL structural Project API contract");
}
