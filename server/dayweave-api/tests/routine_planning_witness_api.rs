//! Inert HTTP admission checks: no database, helper process or external service.
use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use dayweave_api::{
    AppState,
    auth::{AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope},
    http::router,
    persistence::DatabaseScope,
    proposals::{InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
    routine_occurrences::{
        ROUTINE_PLANNING_WITNESS_BYTES, RoutinePlanningWitnessError, RoutinePlanningWitnessRequest,
    },
    scheduling::PostgresSchedulingRepository,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

const PATH: &str = "/v1/routine-occurrences/planning-witness";
const PRIVATE: &str = "synthetic-private-value-must-not-echo";

struct FixedAuth(Principal);
#[async_trait::async_trait]
impl Authenticator for FixedAuth {
    async fn authenticate(&self, _: &str) -> Result<Principal, AuthenticationError> {
        Ok(self.0.clone())
    }
}

fn owner() -> Principal {
    Principal {
        subject: "synthetic planning owner".to_owned(),
        scopes: vec![Scope::ItemsRead, Scope::ScheduleSimulate],
        audience: PrincipalAudience::Device,
        workspace_id: Some(Uuid::from_u128(10)),
        user_id: Some(Uuid::from_u128(11)),
        credential_id: Some(Uuid::from_u128(12)),
        allowed_origins: Vec::new(),
    }
}

fn state(principal: Principal) -> AppState {
    AppState::new(
        Arc::new(ProposalService::new(
            Arc::new(InMemoryProposalRepository::default()),
            Arc::new(SystemClock),
            Duration::from_hours(24),
        )),
        Arc::new(FixedAuth(principal)),
        Readiness::default(),
    )
}

fn lazy_repository() -> PostgresSchedulingRepository {
    PostgresSchedulingRepository::new(
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://synthetic@127.0.0.1:1/synthetic")
            .unwrap(),
        DatabaseScope {
            workspace_id: Uuid::from_u128(10),
            user_id: Uuid::from_u128(11),
        },
    )
}

fn request() -> Value {
    json!({"schema_version":1,"schedule":{"as_of":"2026-09-10T09:00:00Z",
        "horizon_start":"2026-09-10T09:00:00Z","horizon_end":"2026-09-11T09:00:00Z","timezone_name":"UTC"},
        "expected_source_item_revisions":{Uuid::from_u128(1).to_string():1},
        "terminal_cursor":"synthetic-terminal"})
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    body: Vec<u8>,
    authenticated: bool,
    content_types: &[&str],
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, "Bearer synthetic-planning-token");
    }
    for content_type in content_types {
        builder = builder.header(header::CONTENT_TYPE, *content_type);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    if path != "/openapi.json" {
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, max-age=0"
        );
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");
        assert!(!response.headers().contains_key("idempotency-replayed"));
    }
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&bytes).contains(PRIVATE));
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn post(app: &Router, value: Value) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        PATH,
        serde_json::to_vec(&value).unwrap(),
        true,
        &["application/json"],
    )
    .await
}

#[tokio::test]
async fn planning_witness_requires_device_owner_binding_and_both_scopes_before_body() {
    for audience in [
        PrincipalAudience::Legacy,
        PrincipalAudience::Mcp,
        PrincipalAudience::McpOAuth,
    ] {
        let mut principal = owner();
        principal.audience = audience;
        let (status, _) = post(&router(state(principal)), json!({"private":PRIVATE})).await;
        assert_eq!(
            status,
            if audience == PrincipalAudience::Legacy {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
    }
    for scopes in [
        vec![],
        vec![Scope::ItemsRead],
        vec![Scope::ScheduleSimulate],
        vec![Scope::ItemsWrite, Scope::ScheduleRead],
    ] {
        let mut principal = owner();
        principal.scopes = scopes;
        assert_eq!(
            post(&router(state(principal)), json!({"private":PRIVATE}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    for field in ["workspace", "user", "credential"] {
        for value in [None, Some(Uuid::nil())] {
            let mut principal = owner();
            match field {
                "workspace" => principal.workspace_id = value,
                "user" => principal.user_id = value,
                _ => principal.credential_id = value,
            }
            assert_eq!(
                post(&router(state(principal)), json!({"private":PRIVATE}))
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
    }
}

#[tokio::test]
async fn planning_witness_never_invents_authority_without_persistence_or_authentication() {
    let app = router(state(owner()));
    let (status, value) = post(&app, request()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(value["error"]["code"], "service_unavailable");
    assert!(value.get("result").is_none());
    let (status, _) = send(
        &app,
        "POST",
        PATH,
        serde_json::to_vec(&request()).unwrap(),
        false,
        &["application/json"],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn planning_witness_rejects_foreign_workspace_and_user_without_connecting() {
    for foreign_workspace in [true, false] {
        let mut principal = owner();
        if foreign_workspace {
            principal.workspace_id = Some(Uuid::from_u128(90));
        } else {
            principal.user_id = Some(Uuid::from_u128(91));
        }
        let app = router(
            state(principal)
                .with_postgres_scheduling(Arc::new(lazy_repository()), Arc::new(Vec::new())),
        );
        assert_eq!(post(&app, request()).await.0, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn planning_witness_rejects_duplicate_nested_keys_escaped_keys_and_noninteger_sources() {
    let app = router(state(owner()));
    let raw = request().to_string();
    let malformed = vec![
        format!("{raw}{raw}"),
        raw.replace(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
        ),
        raw.replace(
            "\"timezone_name\":\"UTC\"",
            "\"timezone_name\":\"UTC\",\"\\u0074imezone_name\":\"UTC\"",
        ),
        raw.replace(
            "00000000-0000-0000-0000-000000000001\":1",
            "00000000-0000-0000-0000-000000000001\":1,\"00000000-0000-0000-0000-000000000001\":2",
        ),
        raw.replace(
            "00000000-0000-0000-0000-000000000001\":1",
            "00000000-0000-0000-0000-000000000001\":1.0",
        ),
        raw.replace(
            "00000000-0000-0000-0000-000000000001\":1",
            "00000000-0000-0000-0000-000000000001\":true",
        ),
        format!(r#"{{"private":"{PRIVATE}"}}"#),
        format!("{}null{}", "[".repeat(65), "]".repeat(65)),
    ];
    for body in malformed {
        let (status, value) = send(
            &app,
            "POST",
            PATH,
            body.into_bytes(),
            true,
            &["application/json"],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"]["code"], "invalid_json");
    }
    assert_eq!(
        send(&app, "POST", PATH, vec![0xff], true, &["application/json"])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn planning_witness_requires_all_fields_and_rejects_unknown_input() {
    let app = router(state(owner()));
    for field in [
        "schema_version",
        "schedule",
        "expected_source_item_revisions",
        "terminal_cursor",
    ] {
        let mut invalid = request();
        invalid.as_object_mut().unwrap().remove(field);
        assert_eq!(post(&app, invalid).await.0, StatusCode::BAD_REQUEST);
    }
    for nested in [false, true] {
        let mut invalid = request();
        if nested {
            invalid["schedule"]["private"] = json!(PRIVATE);
        } else {
            invalid["private"] = json!(PRIVATE);
        }
        assert_eq!(post(&app, invalid).await.0, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn planning_witness_validates_semantics_before_storage_without_echoes() {
    let app = router(state(owner()));
    for defect in [
        "version",
        "nil",
        "zero",
        "overflow",
        "cursor",
        "precision",
        "horizon",
        "timezone",
    ] {
        let mut invalid = request();
        match defect {
            "version" => invalid["schema_version"] = json!(2),
            "nil" => invalid["expected_source_item_revisions"] = json!({Uuid::nil().to_string():1}),
            "zero" => {
                invalid["expected_source_item_revisions"] =
                    json!({Uuid::from_u128(1).to_string():0});
            }
            "overflow" => {
                invalid["expected_source_item_revisions"] =
                    json!({Uuid::from_u128(1).to_string():i64::MAX as u64+1});
            }
            "cursor" => invalid["terminal_cursor"] = json!(""),
            "precision" => invalid["schedule"]["as_of"] = json!("2026-09-10T09:00:00.000000001Z"),
            "horizon" => {
                invalid["schedule"]["horizon_end"] = invalid["schedule"]["horizon_start"].clone();
            }
            _ => invalid["schedule"]["timezone_name"] = json!(PRIVATE),
        }
        let (status, value) = post(&app, invalid).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{defect}");
        assert_eq!(value["error"]["code"], "routine_planning_invalid");
    }
}

#[tokio::test]
async fn planning_witness_has_an_explicit_bounded_body_without_widening_member_mutation() {
    let mut principal = owner();
    principal.scopes.push(Scope::ItemsWrite);
    let app = router(state(principal));
    let mut body = serde_json::to_vec(&request()).unwrap();
    body.resize(1024 * 1024 + 1, b' ');
    assert_eq!(
        send(
            &app,
            "POST",
            PATH,
            body.clone(),
            true,
            &["application/json"]
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let path = format!(
        "/v1/routine-occurrences/{}/members/{}",
        Uuid::from_u128(1),
        Uuid::from_u128(2)
    );
    assert_eq!(
        send(&app, "PUT", &path, body, true, &["application/json"])
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let (status, value) = send(
        &app,
        "POST",
        PATH,
        vec![b' '; ROUTINE_PLANNING_WITNESS_BYTES + 1],
        true,
        &["application/json"],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(value["error"]["code"], "routine_planning_too_large");
    let mut oversized_cursor = request();
    oversized_cursor["terminal_cursor"] = json!("x".repeat(4097));
    assert_eq!(
        post(&app, oversized_cursor).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn planning_witness_requires_one_json_media_type_and_no_query_selectors() {
    let app = router(state(owner()));
    for content_types in [
        vec![],
        vec!["text/plain"],
        vec!["application/json", "application/json"],
        vec!["application/json,application/json"],
    ] {
        assert_eq!(
            send(
                &app,
                "POST",
                PATH,
                serde_json::to_vec(&request()).unwrap(),
                true,
                &content_types
            )
            .await
            .0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }
    assert_eq!(
        send(
            &app,
            "POST",
            PATH,
            serde_json::to_vec(&request()).unwrap(),
            true,
            &["application/json; charset=utf-8"]
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (status, value) = send(
        &app,
        "POST",
        &format!("{PATH}?private={PRIVATE}"),
        serde_json::to_vec(&request()).unwrap(),
        true,
        &["application/json"],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], "invalid_query");
}

#[tokio::test]
async fn planning_witness_repository_validates_direct_typed_requests_without_connecting() {
    let mut request: RoutinePlanningWitnessRequest = serde_json::from_value(request()).unwrap();
    request.schema_version = 99;
    assert_eq!(
        lazy_repository()
            .routine_planning_witness(&request)
            .await
            .unwrap_err(),
        RoutinePlanningWitnessError::Invalid
    );
}

#[tokio::test]
async fn planning_witness_openapi_exposes_versioned_body_and_typed_remote_outcome() {
    let app = router(state(owner()));
    let (status, value) = send(&app, "GET", "/openapi.json", Vec::new(), false, &[]).await;
    assert_eq!(status, StatusCode::OK);
    let operation = &value["paths"][PATH]["post"];
    assert_eq!(
        operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/RoutinePlanningWitnessRequest"
    );
    assert_eq!(
        operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/RoutinePlanningWitnessResponse"
    );
    for status in [
        "400", "401", "403", "404", "409", "413", "415", "422", "503",
    ] {
        assert!(operation["responses"][status].is_object());
    }
    let schema = &value["components"]["schemas"]["RoutinePlanningWitnessRequest"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"].as_array().unwrap().len(), 4);
    assert!(value["components"]["schemas"]["RoutinePlanningRemoteReason"].is_object());
}
