//! Inert HTTP boundary tests: no database or external service is started.
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
    scheduling::PostgresSchedulingRepository,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "synthetic-routine-occurrence-token";
const ID: &str = "00000000-0000-0000-0000-000000000001";
const MEMBER: &str = "00000000-0000-0000-0000-000000000002";
const PLANNER_OCCURRENCE: &str = "00000000-0000-5000-8000-000000000003";

struct FixedAuth(Principal);

#[async_trait::async_trait]
impl Authenticator for FixedAuth {
    async fn authenticate(&self, _: &str) -> Result<Principal, AuthenticationError> {
        Ok(self.0.clone())
    }
}

fn owner(scopes: Vec<Scope>) -> Principal {
    Principal {
        subject: "synthetic occurrence owner".to_owned(),
        scopes,
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

fn routes() -> Vec<(&'static str, String)> {
    vec![
        ("GET", "/v1/routine-occurrences".to_owned()),
        ("GET", "/v1/routine-occurrences/delta".to_owned()),
        ("GET", format!("/v1/routine-occurrences/{ID}")),
        (
            "PUT",
            format!("/v1/routine-occurrences/{ID}/members/{MEMBER}"),
        ),
        (
            "GET",
            format!(
                "/v1/routine-occurrences/lookup?series_item_id={ID}&occurrence_id={PLANNER_OCCURRENCE}"
            ),
        ),
    ]
}

fn command() -> Value {
    json!({"schema_version":1,"operation_id":Uuid::from_u128(100),
        "expected_instance_revision":1,"expected_member_revision":1,
        "expected_evidence_hash":"a".repeat(64),
        "action":{"type":"set_outcome","status":"completed"}})
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<String>,
    authenticated: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .body(body.map_or_else(Body::empty, Body::from))
                .unwrap(),
        )
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
    let value = serde_json::from_slice(&bytes).unwrap();
    (status, value)
}

#[tokio::test]
async fn owner_device_routes_fail_unavailable_without_persistence_and_never_invent_empty_authority()
{
    let app = router(state(owner(vec![Scope::ItemsRead, Scope::ItemsWrite])));
    for (method, path) in routes() {
        let (status, value) = call(&app, method, &path, Some(command().to_string()), true).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(value["error"]["code"], "service_unavailable");
        assert!(value.get("changes").is_none());
        assert!(value.get("occurrence").is_none());
    }
}

#[tokio::test]
async fn complete_private_manifests_require_bound_device_and_read_permission_even_on_put() {
    let full = vec![Scope::ItemsRead, Scope::ItemsWrite];
    for audience in [
        PrincipalAudience::Legacy,
        PrincipalAudience::Mcp,
        PrincipalAudience::McpOAuth,
    ] {
        let mut principal = owner(full.clone());
        principal.audience = audience;
        let app = router(state(principal));
        for (method, path) in routes() {
            let (status, _) = call(&app, method, &path, Some(command().to_string()), true).await;
            assert_eq!(
                status,
                if audience == PrincipalAudience::Legacy {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
        }
    }
    for missing_workspace in [true, false] {
        let mut principal = owner(full.clone());
        if missing_workspace {
            principal.workspace_id = None;
        } else {
            principal.user_id = None;
        }
        let app = router(state(principal));
        assert_eq!(
            call(&app, "GET", &routes()[0].1, None, true).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for scopes in [vec![], vec![Scope::ItemsRead], vec![Scope::ItemsWrite]] {
        let app = router(state(owner(scopes.clone())));
        for (method, path) in routes() {
            let expected = if method == "GET" && scopes.contains(&Scope::ItemsRead) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(
                call(&app, method, &path, Some(command().to_string()), true)
                    .await
                    .0,
                expected
            );
        }
    }
}

#[tokio::test]
async fn missing_authentication_and_wrong_repository_owner_never_read_storage() {
    let principal = owner(vec![Scope::ItemsRead, Scope::ItemsWrite]);
    let base = state(principal);
    let app = router(base.clone());
    for (method, path) in routes() {
        assert_eq!(
            call(&app, method, &path, Some(command().to_string()), false)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    // Lazy construction does not connect. A scope mismatch must be rejected
    // before the repository could try to use this deliberately unreachable pool.
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://synthetic@127.0.0.1:1/synthetic")
        .unwrap();
    for scope in [
        DatabaseScope {
            workspace_id: Uuid::from_u128(99),
            user_id: Uuid::from_u128(11),
        },
        DatabaseScope {
            workspace_id: Uuid::from_u128(10),
            user_id: Uuid::from_u128(99),
        },
    ] {
        let app = router(base.clone().with_postgres_scheduling(
            Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope)),
            Arc::new(Vec::new()),
        ));
        for (method, path) in routes() {
            assert_eq!(
                call(&app, method, &path, Some(command().to_string()), true)
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
        }
    }
    pool.close().await;
}

#[tokio::test]
async fn malformed_private_command_is_closed_and_parser_diagnostics_are_not_echoed() {
    let app = router(state(owner(vec![Scope::ItemsRead, Scope::ItemsWrite])));
    let path = &routes()[3].1;
    let base = command().to_string();
    let unknown = base.replacen('{', "{\"private-title-must-not-leak\":true,", 1);
    let duplicate = base.replacen(
        '{',
        "{\"operation_id\":\"00000000-0000-0000-0000-000000000101\",",
        1,
    );
    let fractional = base.replace(
        "\"expected_member_revision\":1",
        "\"expected_member_revision\":1.0",
    );
    let mut nested = command();
    nested["action"] = json!({"type":"reopen","open":{"status":"private-title-must-not-leak","blocked_reason_kind":null,"blocked_by_item_id":null,"blocked_reason":null}});
    for body in [
        unknown,
        duplicate,
        fractional,
        nested.to_string(),
        "{private-title-must-not-leak".to_owned(),
    ] {
        let (status, value) = call(&app, "PUT", path, Some(body), true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"]["code"], "invalid_json");
        assert!(!value.to_string().contains("private-title-must-not-leak"));
    }
    let (status, value) = call(&app, "PUT", path, Some("x".repeat(1024 * 1024 + 1)), true).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(value["error"]["code"], "payload_too_large");
}

#[tokio::test]
async fn strict_query_and_path_errors_remain_private_and_bounded() {
    let app = router(state(owner(vec![Scope::ItemsRead, Scope::ItemsWrite])));
    for suffix in [
        "?unknown=private-title-must-not-leak",
        "?limit=private-title-must-not-leak",
        "?limit=1&limit=2",
    ] {
        let (status, value) = call(
            &app,
            "GET",
            &format!("/v1/routine-occurrences{suffix}"),
            None,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"]["code"], "invalid_query");
        assert!(!value.to_string().contains("private-title-must-not-leak"));
    }
    for limit in [0, 101] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/v1/routine-occurrences?limit={limit}"),
                None,
                true
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (status, value) = call(
        &app,
        "GET",
        "/v1/routine-occurrences/private-title-must-not-leak",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"]["code"], "invalid_path");
    assert!(!value.to_string().contains("private-title-must-not-leak"));
}

#[tokio::test]
async fn lookup_validates_closed_selectors_before_storage_without_echoing_private_queries() {
    let app = router(state(owner(vec![Scope::ItemsRead])));
    let selectors = format!("series_item_id={ID}&occurrence_id={PLANNER_OCCURRENCE}");
    for query in [
        String::new(),
        format!("series_item_id={ID}"),
        format!("occurrence_id={PLANNER_OCCURRENCE}"),
        format!("{selectors}&unknown=private-title-must-not-leak"),
        format!("{selectors}&series_item_id={ID}"),
        format!("{selectors}&occurrence_id={PLANNER_OCCURRENCE}"),
        format!("series_item_id=private-title-must-not-leak&occurrence_id={PLANNER_OCCURRENCE}"),
        format!("series_item_id={ID}&occurrence_id=private-title-must-not-leak"),
    ] {
        let (status, value) = call(
            &app,
            "GET",
            &format!("/v1/routine-occurrences/lookup?{query}"),
            None,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(value["error"]["code"], "invalid_query");
        assert!(!value.to_string().contains("private-title-must-not-leak"));
    }
    for (series, occurrence) in [
        (Uuid::nil().to_string(), PLANNER_OCCURRENCE.to_owned()),
        (ID.to_owned(), Uuid::nil().to_string()),
        (ID.to_owned(), Uuid::new_v4().to_string()),
        (
            ID.to_owned(),
            "00000000-0000-5000-0000-000000000003".to_owned(),
        ),
    ] {
        let (status, value) = call(
            &app,
            "GET",
            &format!(
                "/v1/routine-occurrences/lookup?series_item_id={series}&occurrence_id={occurrence}"
            ),
            None,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(value["error"]["code"], "routine_occurrence_invalid");
    }
    for scopes in [vec![], vec![Scope::ItemsWrite]] {
        let forbidden = router(state(owner(scopes)));
        let (status, _) = call(
            &forbidden,
            "GET",
            "/v1/routine-occurrences/lookup?unknown=private-title-must-not-leak",
            None,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn lookup_repository_rejects_invalid_identity_without_connecting() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://synthetic@127.0.0.1:1/synthetic")
        .unwrap();
    let repository = dayweave_api::persistence::PostgresRoutineOccurrenceRepository::new(
        pool.clone(),
        DatabaseScope {
            workspace_id: Uuid::from_u128(10),
            user_id: Uuid::from_u128(11),
        },
    );
    for (series, occurrence) in [
        (Uuid::nil(), Uuid::parse_str(PLANNER_OCCURRENCE).unwrap()),
        (Uuid::from_u128(1), Uuid::nil()),
        (Uuid::from_u128(1), Uuid::new_v4()),
    ] {
        assert_eq!(
            repository.lookup(series, occurrence).await.unwrap_err(),
            dayweave_api::routine_occurrences::RoutineOccurrenceError::Invalid
        );
    }
    pool.close().await;
}

#[tokio::test]
async fn openapi_lists_all_five_owner_only_operations_and_typed_bodies() {
    let app = router(state(owner(Vec::new())));
    let (status, document) = call(&app, "GET", "/openapi.json", None, false).await;
    assert_eq!(status, StatusCode::OK);
    for (path, method, schema) in [
        ("/v1/routine-occurrences", "get", "RoutineOccurrencePage"),
        (
            "/v1/routine-occurrences/delta",
            "get",
            "RoutineOccurrencePage",
        ),
        (
            "/v1/routine-occurrences/{occurrence_id}",
            "get",
            "RoutineOccurrenceSnapshot",
        ),
        (
            "/v1/routine-occurrences/lookup",
            "get",
            "RoutineOccurrenceSnapshot",
        ),
        (
            "/v1/routine-occurrences/{occurrence_id}/members/{item_id}",
            "put",
            "RoutineOccurrenceMutation",
        ),
    ] {
        let operation = &document["paths"][path][method];
        assert!(
            operation["description"]
                .as_str()
                .unwrap()
                .contains("device")
        );
        assert!(operation["security"].is_array());
        assert!(operation["responses"]["200"].to_string().contains(schema));
        for status in ["400", "401", "403", "404", "409", "413", "422", "503"] {
            assert!(operation["responses"][status].is_object());
        }
    }
    assert!(document["components"]["schemas"]["RoutineOccurrenceCommand"].is_object());
    let parameters = document["paths"]["/v1/routine-occurrences/lookup"]["get"]["parameters"]
        .as_array()
        .unwrap();
    assert_eq!(parameters.len(), 2);
    for name in ["series_item_id", "occurrence_id"] {
        let parameter = parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap();
        assert_eq!(parameter["in"], "query");
        assert_eq!(parameter["required"], true);
    }
}
