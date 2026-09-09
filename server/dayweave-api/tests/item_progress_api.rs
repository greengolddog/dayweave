use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use dayweave_api::{
    AppState,
    auth::{AuthenticationError, Authenticator, Principal, Scope, StaticTokenAuthenticator},
    execution::ExecutionService,
    http::router,
    items::{ItemRepository, ItemService},
    persistence::{DatabaseScope, MIGRATOR, PostgresExecutionRepository, PostgresItemRepository},
    proposals::{InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, ConnectOptions as _, Executor as _, postgres::PgConnectOptions};
use std::{str::FromStr as _, sync::Arc, time::Duration};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "synthetic-item-progress-token";

fn state() -> AppState {
    AppState::new(
        Arc::new(ProposalService::new(
            Arc::new(InMemoryProposalRepository::default()),
            Arc::new(SystemClock),
            Duration::from_hours(24),
        )),
        Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
        Readiness::default(),
    )
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json");
    // Progress uses its body operation identity alone, unlike legacy item commands.
    if !path.ends_with("/progress") {
        request = request.header(
            "Idempotency-Key",
            format!("progress-test-{}", Uuid::new_v4()),
        );
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let replay = response.headers().get("idempotency-replayed").cloned();
    if path.ends_with("/progress") {
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, max-age=0"
        );
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");
    }
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    if method == "PUT" && path.ends_with("/progress") && status == StatusCode::OK {
        assert_eq!(
            replay.unwrap(),
            if value["replayed"] == true {
                "true"
            } else {
                "false"
            }
        );
    }
    (status, value)
}

async fn ok(app: &Router, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, value) = call(app, method, path, body).await;
    assert!(status.is_success(), "{method} {path}: {status} {value}");
    value
}

fn task(id: Uuid) -> Value {
    json!({"id":id,"kind":"task","status":"planned","title":"Synthetic progress item","timezone_name":"UTC",
        "duration_seconds":60,"flexible_constraints":{},"split_policy":{"type":"indivisible"},
        "importance":50,"urgency":50,"is_sensitive":false,"parent_id":null,"sibling_order":0})
}

fn command(operation: Uuid, item_revision: u64, revision: u64, components: Value) -> Value {
    let mut command = json!({"schema_version":1,"operation_id":operation,"expected_item_revision":item_revision,
        "expected_progress_revision":revision});
    command["components"] = components;
    command
}

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/item-progress/components-v1.json"
    ))
    .unwrap()
}

#[allow(clippy::too_many_lines)] // One shared ordered history proves permanent replay after later state changes.
async fn exercise_contract(app: &Router) {
    let id = Uuid::new_v4();
    let path = format!("/v1/items/{id}/progress");
    let original = ok(app, "POST", "/v1/items", Some(task(id))).await["item"].clone();
    let canonical_head = ok(app, "GET", "/v1/items/delta", None).await["next_cursor"].clone();
    assert!(canonical_head.is_string());
    let empty = ok(app, "GET", &path, None).await;
    assert_eq!(
        empty,
        json!({"schema_version":1,"item_id":id,"item_revision":1,"revision":0,"components":[],"updated_at":null})
    );
    let fixture = fixture();
    let mut revision = 0_u64;
    let mut first = None;
    for case in fixture["valid"].as_array().unwrap() {
        let input = command(Uuid::new_v4(), 1, revision, case["components"].clone());
        let result = ok(app, "PUT", &path, Some(input.clone())).await;
        revision += 1;
        assert_eq!(result["progress"]["revision"], revision);
        assert_eq!(result["progress"]["components"], case["components"]);
        assert_eq!(result["progress"]["item_revision"], 1);
        assert!(result["progress"]["updated_at"].is_string());
        assert_eq!(result["replayed"], false);
        if first.is_none() {
            first = Some((input, result));
        }
    }
    let latest = ok(app, "GET", &path, None).await;
    for case in fixture["invalid"].as_array().unwrap() {
        let (status, body) = call(
            app,
            "PUT",
            &path,
            Some(command(
                Uuid::new_v4(),
                1,
                revision,
                case["components"].clone(),
            )),
        )
        .await;
        assert!(
            matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "{} {status} {body}",
            case["name"]
        );
        assert_eq!(ok(app, "GET", &path, None).await, latest);
    }
    let (input, first_result) = first.unwrap();
    let replay = ok(app, "PUT", &path, Some(input.clone())).await;
    assert_eq!(replay["progress"], first_result["progress"]);
    assert_eq!(replay["replayed"], true);
    let mut reused = input.clone();
    reused["expected_item_revision"] = json!(2);
    for (input, code) in [
        (reused, "item_progress_operation_reused"),
        (
            command(Uuid::new_v4(), 2, revision, json!([])),
            "item_progress_item_stale",
        ),
        (
            command(Uuid::new_v4(), 1, 0, json!([])),
            "item_progress_revision_stale",
        ),
    ] {
        let (status, error) = call(app, "PUT", &path, Some(input)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], code);
    }
    assert_eq!(
        ok(app, "GET", &format!("/v1/items/{id}"), None).await["item"],
        original
    );
    assert_eq!(
        ok(app, "GET", "/v1/items/delta", None).await["next_cursor"],
        canonical_head
    );
    // A legacy full replacement must preserve sidecar content and GET joins CURRENT revision.
    let mut replacement = task(id);
    replacement.as_object_mut().unwrap().remove("id");
    replacement["title"] = json!("Updated title, unchanged independent progress");
    ok(
        app,
        "PUT",
        &format!("/v1/items/{id}"),
        Some(json!({"expected_revision":1,"item":replacement})),
    )
    .await;
    let joined = ok(app, "GET", &path, None).await;
    assert_eq!(joined["item_revision"], 2);
    assert_eq!(joined["revision"], revision);
    assert_eq!(joined["components"], latest["components"]);
    let clear = command(Uuid::new_v4(), 2, revision, json!([]));
    let cleared = ok(app, "PUT", &path, Some(clear.clone())).await;
    assert_eq!(cleared["progress"]["revision"], revision + 1);
    ok(
        app,
        "DELETE",
        &format!("/v1/items/{id}?expected_revision=2"),
        None,
    )
    .await;
    let (status, error) = call(app, "GET", &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "item_progress_item_missing");
    assert_eq!(
        ok(app, "PUT", &path, Some(clear)).await["progress"],
        cleared["progress"]
    );
    let (status, _) = call(
        app,
        "PUT",
        &path,
        Some(command(Uuid::new_v4(), 3, revision + 1, json!([]))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    ok(
        app,
        "POST",
        &format!("/v1/items/{id}/restore"),
        Some(json!({"expected_revision":3})),
    )
    .await;
    let restored = ok(app, "GET", &path, None).await;
    assert_eq!(restored["item_revision"], 4);
    assert_eq!(restored["revision"], revision + 1);
    // Operation identity spans every item, not merely this detail route.
    let other = Uuid::new_v4();
    ok(app, "POST", "/v1/items", Some(task(other))).await;
    let (status, error) = call(
        app,
        "PUT",
        &format!("/v1/items/{other}/progress"),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "item_progress_operation_reused");
}

#[tokio::test]
async fn memory_http_fixtures_cas_replay_and_legacy_preservation() {
    let app = router(state());
    exercise_contract(&app).await;
    exercise_all_kinds(&app).await;
}

async fn exercise_all_kinds(app: &Router) {
    let structural: Value = serde_json::from_str(include_str!(
        "../../../fixtures/structural-authoring/requests-v1.json"
    ))
    .unwrap();
    for kind in [
        "event", "task", "habit", "routine", "goal", "project", "break",
    ] {
        let id = Uuid::new_v4();
        let mut item = if kind == "event" {
            structural["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["create"]["kind"] == "event")
                .unwrap()["create"]
                .clone()
        } else {
            task(id)
        };
        item["id"] = json!(id);
        item["kind"] = json!(kind);
        if kind == "habit" {
            item["recurrence"] = json!({"type":"daily","times_per_day":1});
        }
        let original = ok(app, "POST", "/v1/items", Some(item)).await["item"].clone();
        ok(
            app,
            "PUT",
            &format!("/v1/items/{id}/progress"),
            Some(command(
                Uuid::new_v4(),
                1,
                0,
                fixture()["valid"][1]["components"].clone(),
            )),
        )
        .await;
        assert_eq!(
            ok(app, "GET", &format!("/v1/items/{id}"), None).await["item"],
            original,
            "{kind} lifecycle and effort are independent"
        );
    }
}

#[tokio::test]
async fn invalid_command_authority_and_private_parser_diagnostics_are_rejected() {
    let app = router(state());
    let id = Uuid::new_v4();
    ok(&app, "POST", "/v1/items", Some(task(id))).await;
    let path = format!("/v1/items/{id}/progress");
    let valid = command(Uuid::new_v4(), 1, 0, json!([]));
    let initial = ok(&app, "GET", &path, None).await;
    for (field, value) in [
        ("schema_version", json!(2)),
        ("schema_version", Value::Null),
        ("operation_id", json!(Uuid::nil())),
        ("operation_id", json!("private-label-must-not-escape")),
        ("expected_item_revision", json!(0)),
        (
            "expected_item_revision",
            json!(9_223_372_036_854_775_808_u64),
        ),
        (
            "expected_progress_revision",
            json!(9_223_372_036_854_775_808_u64),
        ),
        ("expected_item_revision", json!(1.5)),
        ("expected_item_revision", json!("1")),
        ("expected_progress_revision", json!(-1)),
        (
            "unexpected_private_field",
            json!("private-label-must-not-escape"),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        let (status, error) = call(&app, "PUT", &path, Some(invalid)).await;
        assert!(matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ));
        assert!(!error.to_string().contains("private-label-must-not-escape"));
        assert_eq!(ok(&app, "GET", &path, None).await, initial);
    }
    for field in [
        "schema_version",
        "operation_id",
        "expected_item_revision",
        "expected_progress_revision",
        "components",
    ] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove(field);
        assert_eq!(
            call(&app, "PUT", &path, Some(invalid)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/v1/items/{}/progress", Uuid::nil()),
            Some(valid)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn active_execution_parent_and_terminal_states_are_independent() {
    let app = router(state());
    exercise_active_execution(&app).await;
}

async fn exercise_active_execution(app: &Router) {
    let id = Uuid::new_v4();
    let original = ok(app, "POST", "/v1/items", Some(task(id))).await["item"].clone();
    let session = Uuid::new_v4();
    ok(
        app,
        "POST",
        "/v1/execution/commands",
        Some(json!({"expected_revision":0,"command":{
        "type":"start","session_id":session,"item_id":id,"item_revision":1,"occurrence_id":null,
        "session_index":0,"planned_block_id":null,"device_id":Uuid::new_v4()}})),
    )
    .await;
    let execution = ok(app, "GET", "/v1/execution", None).await;
    let path = format!("/v1/items/{id}/progress");
    ok(
        app,
        "PUT",
        &path,
        Some(command(
            Uuid::new_v4(),
            1,
            0,
            fixture()["valid"][1]["components"].clone(),
        )),
    )
    .await;
    assert_eq!(ok(app, "GET", "/v1/execution", None).await, execution);
    assert_eq!(
        ok(app, "GET", &format!("/v1/items/{id}"), None).await["item"],
        original
    );
    for status in [
        "inbox",
        "planned",
        "scheduled",
        "in_progress",
        "paused",
        "completed",
        "skipped",
        "cancelled",
        "blocked",
    ] {
        let id = Uuid::new_v4();
        let mut body = task(id);
        body["status"] = json!(status);
        if status == "blocked" {
            body["blocked_reason_kind"] = json!("manual");
            body["blocked_reason"] = json!("Synthetic blocker");
        }
        ok(app, "POST", "/v1/items", Some(body)).await;
        ok(
            app,
            "PUT",
            &format!("/v1/items/{id}/progress"),
            Some(command(Uuid::new_v4(), 1, 0, json!([]))),
        )
        .await;
    }
    let parent = Uuid::new_v4();
    let mut body = task(parent);
    body["kind"] = json!("project");
    ok(app, "POST", "/v1/items", Some(body)).await;
    let mut child = task(Uuid::new_v4());
    child["parent_id"] = json!(parent);
    ok(app, "POST", "/v1/items", Some(child)).await;
    ok(
        app,
        "PUT",
        &format!("/v1/items/{parent}/progress"),
        Some(command(Uuid::new_v4(), 2, 0, json!([]))),
    )
    .await;
}

#[tokio::test]
async fn competing_progress_writes_are_atomic() {
    let app = router(state());
    exercise_concurrent_writes(&app).await;
}

async fn exercise_concurrent_writes(app: &Router) {
    let id = Uuid::new_v4();
    ok(app, "POST", "/v1/items", Some(task(id))).await;
    let path = format!("/v1/items/{id}/progress");
    let (left, right) = tokio::join!(
        call(
            app,
            "PUT",
            &path,
            Some(command(Uuid::new_v4(), 1, 0, json!([])))
        ),
        call(
            app,
            "PUT",
            &path,
            Some(command(Uuid::new_v4(), 1, 0, json!([])))
        )
    );
    let mut codes = [left.0.as_u16(), right.0.as_u16()];
    codes.sort_unstable();
    assert_eq!(codes, [200, 409]);
    assert_eq!(ok(app, "GET", &path, None).await["revision"], 1);
}

struct ScopedAuth(Vec<Scope>);
#[async_trait::async_trait]
impl Authenticator for ScopedAuth {
    async fn authenticate(&self, _token: &str) -> Result<Principal, AuthenticationError> {
        let mut principal = Principal::legacy("synthetic".to_owned());
        principal.scopes.clone_from(&self.0);
        Ok(principal)
    }
}

#[tokio::test]
async fn route_scopes_and_openapi_are_explicit() {
    let mut state = state();
    let id = Uuid::new_v4();
    let app = router(state.clone());
    let document = ok(&app, "GET", "/openapi.json", None).await;
    let path_schema = &document["paths"]["/v1/items/{item_id}/progress"];
    assert!(path_schema["get"].is_object());
    assert!(path_schema["put"].is_object());
    let schemas = &document["components"]["schemas"];
    assert!(
        schemas["ItemProgressSnapshot"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("updated_at"))
    );
    for (kind, field) in [("time", "remaining_seconds"), ("quantity", "target")] {
        let variant = schemas["ItemProgressValue"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variant| variant["properties"]["type"]["enum"] == json!([kind]))
            .unwrap();
        assert!(
            variant["required"]
                .as_array()
                .unwrap()
                .contains(&json!(field))
        );
    }
    ok(&app, "POST", "/v1/items", Some(task(id))).await;
    state.authenticator = Arc::new(ScopedAuth(vec![Scope::ItemsRead]));
    let read = router(state.clone());
    let path = format!("/v1/items/{id}/progress");
    assert_eq!(call(&read, "GET", &path, None).await.0, StatusCode::OK);
    assert_eq!(
        call(
            &read,
            "PUT",
            &path,
            Some(command(Uuid::new_v4(), 1, 0, json!([])))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    state.authenticator = Arc::new(ScopedAuth(vec![Scope::ItemsWrite]));
    let write = router(state);
    assert_eq!(
        call(&write, "GET", &path, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &write,
            "PUT",
            &path,
            Some(command(Uuid::new_v4(), 1, 0, json!([])))
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn postgres_http_progress_and_atomic_audit_contract() {
    let Ok(url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL unset; progress PostgreSQL test skipped");
        return;
    };
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .disable_statement_logging();
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    let schema = format!("dayweave_progress_{}", Uuid::new_v4().simple());
    admin
        .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .await
        .unwrap();
    let connection_schema = schema.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
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
    let test_pool = pool.clone();
    let scenario=tokio::spawn(async move {
        MIGRATOR.run(&test_pool).await.unwrap();
        let scope=DatabaseScope {user_id:Uuid::new_v4(),workspace_id:Uuid::new_v4()};
        sqlx::query("INSERT INTO users(id,auth_subject,display_name,timezone_name) VALUES($1,$2,'Synthetic progress','UTC')")
            .bind(scope.user_id).bind(format!("progress-{}",scope.user_id)).execute(&test_pool).await.unwrap();
        sqlx::query("INSERT INTO workspaces(id,owner_user_id,slug,name,timezone_name) VALUES($1,$2,$3,'Synthetic progress','UTC')")
            .bind(scope.workspace_id).bind(scope.user_id).bind(format!("progress-{}",scope.workspace_id)).execute(&test_pool).await.unwrap();
        sqlx::query("INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,'owner')")
            .bind(scope.workspace_id).bind(scope.user_id).execute(&test_pool).await.unwrap();
        let repository=Arc::new(PostgresItemRepository::new(test_pool.clone(),scope));
        let items=Arc::new(ItemService::new(repository.clone(),Arc::new(SystemClock)));
        let mut state=state().with_items(items.clone());
        state.execution=Arc::new(ExecutionService::new(Arc::new(PostgresExecutionRepository::new(test_pool.clone(),scope)),items,Arc::new(SystemClock)));
        let app=router(state);
        exercise_contract(&app).await;
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM item_progress_operations").fetch_one(&test_pool).await.unwrap();
        assert_eq!(count,6);
        let audit:i64=sqlx::query_scalar("SELECT count(*) FROM audit_operations WHERE entity_type='item_progress'").fetch_one(&test_pool).await.unwrap();assert_eq!(count,audit);
        assert!(sqlx::query("UPDATE item_progress_operations SET recorded_at=clock_timestamp()").execute(&test_pool).await.is_err());
        assert!(sqlx::query("DELETE FROM item_progress_operations").execute(&test_pool).await.is_err());
        let rows:i64=sqlx::query_scalar("SELECT count(*) FROM item_progress").fetch_one(&test_pool).await.unwrap();
        assert_eq!(rows,1);
        let mut tx=test_pool.begin().await.unwrap();
        sqlx::query("UPDATE item_progress SET revision=revision+1,updated_at=clock_timestamp()").execute(&mut *tx).await.unwrap();
        assert!(tx.commit().await.is_err(),"a sidecar update cannot commit without immutable custody");
        for case in fixture()["valid"].as_array().unwrap() {
            let valid:bool=sqlx::query_scalar("SELECT valid_item_progress_components($1)").bind(&case["components"]).fetch_one(&test_pool).await.unwrap();assert!(valid,"{}",case["name"]);
        }
        for case in fixture()["invalid"].as_array().unwrap() {
            let valid=sqlx::query_scalar::<_,bool>("SELECT valid_item_progress_components($1)").bind(&case["components"]).fetch_one(&test_pool).await;
            assert!(!matches!(valid,Ok(true)),"{}",case["name"]);
        }
        assert_shared_scalar_sql(&test_pool).await;
        let foreign=PostgresItemRepository::new(test_pool.clone(),DatabaseScope {workspace_id:scope.workspace_id,user_id:Uuid::new_v4()});
        let id:Uuid=sqlx::query_scalar("SELECT item_id FROM item_progress").fetch_one(&test_pool).await.unwrap();
        assert!(foreign.get_progress(id).await.is_err());
        assert_eq!(repository.get_progress(id).await.unwrap().revision,6);
        assert_forged_receipts_rejected(&test_pool,scope,id).await;
        exercise_concurrent_writes(&app).await;
        exercise_active_execution(&app).await;
        exercise_all_kinds(&app).await;
    }).await;
    pool.close().await;
    admin
        .execute(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .await
        .unwrap();
    admin.close().await;
    scenario.expect("isolated PostgreSQL progress contract");
}

async fn assert_shared_scalar_sql(pool: &sqlx::PgPool) {
    let values: Value = serde_json::from_str(include_str!(
        "../../../fixtures/item-progress/values-v1.json"
    ))
    .unwrap();
    for case in values["decimals"].as_array().unwrap() {
        let valid: bool = sqlx::query_scalar("SELECT valid_item_progress_decimal($1)")
            .bind(case["value"].as_str().unwrap())
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            valid,
            case["valid"].as_bool().unwrap(),
            "decimal {:?}",
            case["value"]
        );
    }
    for case in values["labels"].as_array().unwrap() {
        let valid = sqlx::query_scalar::<_, bool>("SELECT valid_item_progress_label($1,$2)")
            .bind(case["value"].as_str().unwrap())
            .bind(i32::try_from(case["max_scalars"].as_i64().unwrap()).unwrap())
            .fetch_one(pool)
            .await;
        // PostgreSQL cannot transport NUL text; rejection remains invalid, never admission.
        assert_eq!(
            valid.unwrap_or(false),
            case["valid"].as_bool().unwrap(),
            "label {:?}",
            case["value"]
        );
    }
}

async fn assert_forged_receipts_rejected(pool: &sqlx::PgPool, scope: DatabaseScope, id: Uuid) {
    let current: Value = sqlx::query_scalar("SELECT result_json FROM item_progress_operations WHERE item_id=$1 ORDER BY progress_revision DESC LIMIT 1")
        .bind(id).fetch_one(pool).await.unwrap();
    let now: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .unwrap();
    let operation = Uuid::new_v4();
    let mut before = current.clone();
    before["item_revision"] = json!(4);
    let mut result = before.clone();
    result["revision"] = json!(7);
    result["updated_at"] = json!(now);
    let request = command(operation, 4, 6, json!([]));
    let insert = "INSERT INTO item_progress_operations(workspace_id,operation_id,item_id,actor_user_id,progress_revision,request_json,before_json,result_json,recorded_at) VALUES($1,$2,$3,$4,7,$5,$6,$7,$8)";
    assert!(
        sqlx::query(insert)
            .bind(scope.workspace_id)
            .bind(operation)
            .bind(id)
            .bind(scope.user_id)
            .bind(&request)
            .bind(&before)
            .bind(&result)
            .bind(now)
            .execute(pool)
            .await
            .is_err(),
        "receipt cannot exist without its exact sidecar transition"
    );
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("UPDATE item_progress SET revision=7,updated_at=$2 WHERE item_id=$1")
        .bind(id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .unwrap();
    before["components"] = fixture()["valid"][1]["components"].clone();
    sqlx::query(insert)
        .bind(scope.workspace_id)
        .bind(operation)
        .bind(id)
        .bind(scope.user_id)
        .bind(&request)
        .bind(&before)
        .bind(&result)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .unwrap();
    assert!(
        transaction.commit().await.is_err(),
        "forged preimage must roll back sidecar and receipt together"
    );
    for field in [
        "schema_version",
        "item_id",
        "item_revision",
        "revision",
        "components",
        "updated_at",
    ] {
        let mut invalid = current.clone();
        invalid[field] = Value::Null;
        assert!(
            !sqlx::query_scalar::<_, bool>("SELECT valid_item_progress_snapshot($1)")
                .bind(invalid)
                .fetch_one(pool)
                .await
                .unwrap(),
            "null {field} must be rejected"
        );
    }
}
