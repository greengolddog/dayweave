use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use dayweave_api::{
    AppState,
    auth::{AuthenticationError, Authenticator, Principal, Scope, StaticTokenAuthenticator},
    http::router,
    proposals::{InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "synthetic-item-completion-token";

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
    // Completion uses its body operation identity alone, unlike legacy item commands.
    if !path.ends_with("/completion") {
        request = request.header(
            "Idempotency-Key",
            format!("completion-test-{}", Uuid::new_v4()),
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
    if path.ends_with("/completion") {
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, max-age=0"
        );
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");
    }
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    if method == "PUT" && path.ends_with("/completion") && status == StatusCode::OK {
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
    json!({"id":id,"kind":"task","status":"planned","title":"Synthetic completion item","timezone_name":"UTC",
        "duration_seconds":60,"flexible_constraints":{},"split_policy":{"type":"indivisible"},
        "importance":50,"urgency":50,"is_sensitive":false,"parent_id":null,"sibling_order":0})
}

fn command(snapshot: &Value, mode: &str) -> Value {
    json!({"schema_version":1,"operation_id":Uuid::new_v4(),
        "expected_item_revision":snapshot["item_revision"],
        "expected_completion_revision":snapshot["state"]["revision"],
        "expected_evidence_hash":snapshot["evidence_hash"],"required_for_parent":true,
        "mode":mode,"reopening":null})
}

async fn replace_status(app: &Router, id: Uuid, status: &str) -> Value {
    let canonical = ok(app, "GET", &format!("/v1/items/{id}"), None).await["item"].clone();
    let mut body = task(id);
    body.as_object_mut().unwrap().remove("id");
    body["parent_id"] = canonical["parent_id"].clone();
    body["status"] = json!(status);
    ok(
        app,
        "PUT",
        &format!("/v1/items/{id}"),
        Some(json!({"expected_revision":canonical["revision"],"item":body})),
    )
    .await["item"]
        .clone()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn memory_http_completion_all_writers_exact_blocker_and_permanent_policy_receipt() {
    let app = router(state());
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let mut body = task(parent);
    body["status"] = json!("blocked");
    body["blocked_reason_kind"] = json!("manual");
    body["blocked_reason"] = json!("Synthetic reviewed blocker");
    ok(&app, "POST", "/v1/items", Some(body)).await;
    let mut body = task(child);
    body["parent_id"] = json!(parent);
    ok(&app, "POST", "/v1/items", Some(body)).await;
    let path = format!("/v1/items/{parent}/completion");
    let initial = ok(&app, "GET", &path, None).await;
    assert_eq!(initial["state"]["revision"], 0);
    assert_eq!(initial["counts"]["required_descendants"], 1);
    assert_eq!(initial["counts"]["incomplete"], 1);
    let original_child_receipt = replace_status(&app, child, "completed").await;
    assert_eq!(original_child_receipt["revision"], 2);
    let completed = ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"].clone();
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["revision"], 3);
    assert!(completed["blocked_reason"].is_null());
    let auto = ok(&app, "GET", &path, None).await;
    assert_eq!(auto["state"]["provenance"]["kind"], "automatic");
    assert_eq!(
        auto["state"]["provenance"]["reopen"]["blocked_reason"],
        "Synthetic reviewed blocker"
    );

    let added = Uuid::new_v4();
    let mut body = task(added);
    body["parent_id"] = json!(parent);
    ok(&app, "POST", "/v1/items", Some(body)).await;
    let reopened = ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"].clone();
    assert_eq!(reopened["status"], "blocked");
    assert_eq!(reopened["blocked_reason"], "Synthetic reviewed blocker");
    assert_eq!(reopened["revision"], 5);
    let snapshot = ok(&app, "GET", &path, None).await;
    let keep = command(&snapshot, "keep_open");
    let original_policy = ok(&app, "PUT", &path, Some(keep.clone())).await;
    replace_status(&app, added, "completed").await;
    assert_eq!(
        ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"]["status"],
        "blocked"
    );
    let snapshot = ok(&app, "GET", &path, None).await;
    let automatic = ok(&app, "PUT", &path, Some(command(&snapshot, "automatic"))).await;
    assert_eq!(
        automatic["completion"]["state"]["provenance"]["kind"],
        "automatic"
    );
    assert_eq!(
        ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"]["status"],
        "completed"
    );
    let head = ok(&app, "GET", "/v1/items/delta", None).await["next_cursor"].clone();
    let replay = ok(&app, "PUT", &path, Some(keep.clone())).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["completion"], original_policy["completion"]);
    assert_eq!(
        ok(&app, "GET", "/v1/items/delta", None).await["next_cursor"],
        head
    );
    let mut reused = keep;
    reused["mode"] = json!("automatic");
    let (status, error) = call(&app, "PUT", &path, Some(reused)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "item_completion_operation_reused");

    replace_status(&app, added, "planned").await;
    let latest = ok(&app, "GET", &format!("/v1/items/{added}"), None).await["item"].clone();
    let trashed = ok(
        &app,
        "DELETE",
        &format!("/v1/items/{added}?expected_revision={}", latest["revision"]),
        None,
    )
    .await;
    assert_eq!(
        ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"]["status"],
        "completed"
    );
    ok(
        &app,
        "POST",
        &format!("/v1/items/{added}/restore"),
        Some(json!({"expected_revision":trashed["item"]["revision"]})),
    )
    .await;
    assert_eq!(
        ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"]["status"],
        "blocked"
    );

    let child_path = format!("/v1/items/{added}/completion");
    let optional_snapshot = ok(&app, "GET", &child_path, None).await;
    let mut optional = command(&optional_snapshot, "automatic");
    optional["required_for_parent"] = json!(false);
    ok(&app, "PUT", &child_path, Some(optional)).await;
    assert_eq!(
        ok(&app, "GET", &format!("/v1/items/{parent}"), None).await["item"]["status"],
        "completed"
    );
    let stale = command(&optional_snapshot, "automatic");
    assert_eq!(
        call(&app, "PUT", &child_path, Some(stale)).await.1["error"]["code"],
        "item_completion_item_stale"
    );
}

#[tokio::test]
async fn completion_review_includes_global_and_execution_changes() {
    let app = router(state());
    let id = Uuid::new_v4();
    ok(&app, "POST", "/v1/items", Some(task(id))).await;
    let path = format!("/v1/items/{id}/completion");
    let before = ok(&app, "GET", &path, None).await;
    let request = command(&before, "automatic");
    let other = Uuid::new_v4();
    ok(&app, "POST", "/v1/items", Some(task(other))).await;
    let (status, error) = call(&app, "PUT", &path, Some(request)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "item_completion_evidence_stale");
    let before = ok(&app, "GET", &path, None).await;
    let request = command(&before, "automatic");
    ok(&app,"POST","/v1/execution/commands",Some(json!({"expected_revision":0,"command":{
        "type":"start","session_id":Uuid::new_v4(),"item_id":other,"item_revision":1,
        "occurrence_id":null,"session_index":0,"planned_block_id":null,"device_id":Uuid::new_v4()}}))).await;
    let (status, error) = call(&app, "PUT", &path, Some(request)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "item_completion_evidence_stale");
}

#[tokio::test]
async fn competing_policy_writes_have_one_winner_and_same_uuid_replays() {
    let app = router(state());
    let id = Uuid::new_v4();
    ok(&app, "POST", "/v1/items", Some(task(id))).await;
    let path = format!("/v1/items/{id}/completion");
    let snapshot = ok(&app, "GET", &path, None).await;
    let (left, right) = tokio::join!(
        call(&app, "PUT", &path, Some(command(&snapshot, "automatic"))),
        call(&app, "PUT", &path, Some(command(&snapshot, "automatic")))
    );
    let mut statuses = [left.0.as_u16(), right.0.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409]);
    let snapshot = ok(&app, "GET", &path, None).await;
    let request = command(&snapshot, "automatic");
    let (left, right) = tokio::join!(
        call(&app, "PUT", &path, Some(request.clone())),
        call(&app, "PUT", &path, Some(request))
    );
    assert_eq!((left.0, right.0), (StatusCode::OK, StatusCode::OK));
    assert_eq!(left.1["completion"], right.1["completion"]);
    assert_ne!(left.1["replayed"], right.1["replayed"]);
}

#[tokio::test]
async fn completion_private_parser_diagnostics_and_required_nullables_are_closed() {
    let app = router(state());
    let id = Uuid::new_v4();
    ok(&app, "POST", "/v1/items", Some(task(id))).await;
    let path = format!("/v1/items/{id}/completion");
    let snapshot = ok(&app, "GET", &path, None).await;
    let valid = command(&snapshot, "automatic");
    for field in [
        "schema_version",
        "operation_id",
        "expected_item_revision",
        "expected_completion_revision",
        "expected_evidence_hash",
        "required_for_parent",
        "mode",
        "reopening",
    ] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove(field);
        let (status, error) = call(&app, "PUT", &path, Some(invalid)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {error}");
    }
    let mut invalid = valid.clone();
    invalid["private-unknown-field"] = json!("private-blocker-must-not-escape");
    let (status, error) = call(&app, "PUT", &path, Some(invalid)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !error
            .to_string()
            .contains("private-blocker-must-not-escape")
    );
    let mut invalid = valid;
    invalid["mode"] = json!("complete");
    assert_eq!(
        call(&app, "PUT", &path, Some(invalid)).await.1["error"]["code"],
        "item_completion_parent_required"
    );
    assert_eq!(ok(&app, "GET", &path, None).await, snapshot);
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
async fn completion_route_scopes_and_documented_contract_are_explicit() {
    let mut state = state();
    let app = router(state.clone());
    let id = Uuid::new_v4();
    ok(&app, "POST", "/v1/items", Some(task(id))).await;
    let path = format!("/v1/items/{id}/completion");
    let snapshot = ok(&app, "GET", &path, None).await;
    let doc = ok(&app, "GET", "/openapi.json", None).await;
    let schema = &doc["paths"]["/v1/items/{item_id}/completion"];
    assert!(schema["get"].is_object());
    assert!(schema["put"].is_object());
    state.authenticator = Arc::new(ScopedAuth(vec![Scope::ItemsRead]));
    let read = router(state.clone());
    assert_eq!(call(&read, "GET", &path, None).await.0, StatusCode::OK);
    assert_eq!(
        call(&read, "PUT", &path, Some(command(&snapshot, "automatic")))
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
        call(&write, "PUT", &path, Some(command(&snapshot, "automatic")))
            .await
            .0,
        StatusCode::OK
    );
}
