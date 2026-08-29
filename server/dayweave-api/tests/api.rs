use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use chrono::{DateTime, Utc};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    proposals::{Clock, InMemoryProposalRepository, ProposalRepository, ProposalService},
    readiness::Readiness,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "integration-test-token";

#[derive(Clone)]
struct TestClock(Arc<RwLock<DateTime<Utc>>>);

impl TestClock {
    fn new(now: DateTime<Utc>) -> Self {
        Self(Arc::new(RwLock::new(now)))
    }

    fn set(&self, now: DateTime<Utc>) {
        *self.0.write().expect("clock lock") = now;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.read().expect("clock lock")
    }
}

fn test_app(ready: bool) -> (Router, TestClock) {
    let clock = TestClock::new("2026-08-29T09:00:00Z".parse().unwrap());
    let repository: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let service = Arc::new(ProposalService::new(
        repository,
        Arc::new(clock.clone()),
        Duration::from_hours(7 * 24),
    ));
    let authenticator = Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN]));
    let readiness = Readiness::default();
    readiness.set_ready(ready);
    let state = AppState::new(service, authenticator, readiness);
    (router(state), clock)
}

fn request(method: &str, uri: &str, body: Option<Value>, authenticated: bool) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
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

fn new_suggestion() -> Value {
    json!({
        "source": "codex",
        "source_reference": "conversation-42",
        "kind": "create_item",
        "title": "Prepare weekly review",
        "explanation": "A review helps keep the plan realistic",
        "payload": {
            "item_type": "task",
            "duration_minutes": 45
        }
    })
}

#[tokio::test]
async fn system_endpoints_are_public_and_readiness_is_honest() {
    let (app, _) = test_app(false);

    let health = app
        .clone()
        .oneshot(request("GET", "/health", None, false))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(health.headers().contains_key("x-request-id"));

    let not_ready = app
        .clone()
        .oneshot(request("GET", "/readyz", None, false))
        .await
        .unwrap();
    assert_eq!(not_ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(not_ready).await["error"]["code"],
        "service_unavailable"
    );

    let openapi = app
        .oneshot(request("GET", "/openapi.json", None, false))
        .await
        .unwrap();
    let document = body_json(openapi).await;
    assert!(document["paths"]["/v1/suggestions"].is_object());
    assert!(document["paths"]["/v1/schedule/preview"].is_object());
    assert!(document["paths"]["/v1/integrations/google/oauth/start"].is_object());
    assert!(document["paths"]["/v1/integrations/google/oauth/callback"].is_object());
    assert!(
        document["paths"]["/v1/integrations/google/accounts/{account_id}/collections"].is_object()
    );
    assert!(
        document["paths"]
            ["/v1/integrations/google/accounts/{account_id}/collections/discover"]["post"]
            .is_object()
    );
    assert!(
        document["paths"]
            ["/v1/integrations/google/accounts/{account_id}/collections/{collection_id}"]["put"]
            .is_object()
    );
    assert!(
        document["paths"]["/v1/integrations/google/accounts/{account_id}/sync"]["get"].is_object()
    );
    assert!(
        document["paths"]["/v1/integrations/google/accounts/{account_id}/sync/refresh"]["post"]
            .is_object()
    );
    assert!(
        document["paths"]["/v1/integrations/google/accounts/{account_id}/outbound"]["post"]
            .is_object()
    );
    assert!(
        document["paths"]["/v1/integrations/google/accounts/{account_id}/outbound"]["post"]
            ["responses"]["202"]
            .is_null()
    );
    assert!(
        document["paths"]["/v1/integrations/google/accounts/{account_id}/outbound"]["post"]
            ["responses"]["409"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("server-minted audited approval"))
    );
    assert!(
        document["components"]["schemas"]["GoogleSyncCollection"]["properties"]["sync_role"]
            .is_object()
    );
    assert_eq!(
        document["components"]["schemas"]["GoogleSyncRole"]["enum"],
        json!(["read_only", "blocking", "writable"])
    );
    assert!(
        document["components"]["schemas"]["GoogleSyncStatus"]["properties"]["pending_outbound"]
            .is_object()
    );
    assert!(
        document["paths"]["/v1/integrations/google/oauth/recovery/acknowledge"]["post"].is_object()
    );
    assert!(
        document["components"]["schemas"]["GoogleOAuthCleanupStatus"]["properties"]
            ["operator_recovery_required"]
            .is_object()
    );
    assert!(
        document["components"]["schemas"]["GoogleOAuthCleanupStatus"]["properties"]
            ["legacy_recovery_required"]
            .is_object()
    );
    assert!(document["components"]["securitySchemes"].is_object());
}

#[tokio::test]
async fn protected_routes_require_a_valid_bearer_token() {
    let (app, _) = test_app(true);

    let missing = app
        .clone()
        .oneshot(request("GET", "/v1/suggestions", None, false))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(missing).await["error"]["code"], "unauthorized");

    let wrong = Request::builder()
        .uri("/v1/suggestions")
        .header(header::AUTHORIZATION, "Bearer wrong-token")
        .body(Body::empty())
        .unwrap();
    let wrong = app.oneshot(wrong).await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn google_oauth_is_fail_closed_when_disabled_and_callback_is_non_cacheable() {
    let (app, _) = test_app(true);
    let unauthenticated = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/integrations/google/oauth/start",
            Some(json!({"services": ["calendar", "tasks"]})),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let disabled = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/integrations/google/oauth/start",
            Some(json!({"services": ["calendar", "tasks"]})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::SERVICE_UNAVAILABLE);

    let account_id = "00000000-0000-4000-8000-000000000099";
    let sync_unauthenticated = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/integrations/google/accounts/{account_id}/collections"),
            None,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(sync_unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let sync_disabled = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/v1/integrations/google/accounts/{account_id}/collections"),
            None,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(sync_disabled.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(sync_disabled.headers()[header::CACHE_CONTROL], "no-store");

    let callback = app
        .oneshot(request(
            "GET",
            "/v1/integrations/google/oauth/callback?state=do-not-reflect-this-state&code=do-not-reflect-this-code",
            None,
            false,
        ))
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(callback.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(callback.headers()[header::REFERRER_POLICY], "no-referrer");
    assert!(
        callback
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    );
    let body = callback.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(!body.contains("do-not-reflect-this-state"));
    assert!(!body.contains("do-not-reflect-this-code"));
}

#[tokio::test]
async fn suggestion_lifecycle_supports_review_edit_decision_and_delete() {
    let (app, _) = test_app(true);

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/suggestions",
            Some(new_suggestion()),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = body_json(created).await;
    let id = created["suggestion"]["id"].as_str().unwrap();
    assert_eq!(created["suggestion"]["status"], "pending");
    assert_eq!(created["suggestion"]["revision"], 1);

    let edited = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/v1/suggestions/{id}"),
            Some(json!({
                "expected_revision": 1,
                "title": "Prepare a concise weekly review"
            })),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(edited.status(), StatusCode::OK);
    let edited = body_json(edited).await;
    assert_eq!(edited["suggestion"]["revision"], 2);

    let accepted = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/suggestions/{id}/accept"),
            Some(json!({
                "expected_revision": 2,
                "note": "Apply after showing the external-effects preview"
            })),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    let accepted = body_json(accepted).await;
    assert_eq!(accepted["suggestion"]["status"], "accepted");
    assert_eq!(accepted["suggestion"]["revision"], 3);

    let second_decision = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/suggestions/{id}/reject"),
            Some(json!({"expected_revision": 3})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(second_decision.status(), StatusCode::CONFLICT);

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/suggestions?status=accepted&limit=10",
            None,
            true,
        ))
        .await
        .unwrap();
    let listed = body_json(listed).await;
    assert_eq!(listed["suggestions"].as_array().unwrap().len(), 1);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/v1/suggestions/{id}?expected_revision=3"),
            None,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let missing = app
        .oneshot(request("GET", &format!("/v1/suggestions/{id}"), None, true))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_invalid_payloads_and_stale_writes_with_structured_errors() {
    let (app, _) = test_app(true);
    let mut invalid = new_suggestion();
    invalid["payload"] = json!(["not", "an", "object"]);

    let invalid = app
        .clone()
        .oneshot(request("POST", "/v1/suggestions", Some(invalid), true))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_json(invalid).await["error"]["code"],
        "validation_failed"
    );

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/suggestions",
            Some(new_suggestion()),
            true,
        ))
        .await
        .unwrap();
    let created = body_json(created).await;
    let id = created["suggestion"]["id"].as_str().unwrap();

    let stale = app
        .oneshot(request(
            "PATCH",
            &format!("/v1/suggestions/{id}"),
            Some(json!({"expected_revision": 99, "title": "Stale edit"})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale = body_json(stale).await;
    assert_eq!(stale["error"]["details"]["actual_revision"], 1);
}

#[tokio::test]
async fn due_suggestions_expire_when_read() {
    let (app, clock) = test_app(true);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/suggestions",
            Some(new_suggestion()),
            true,
        ))
        .await
        .unwrap();
    let created = body_json(created).await;
    let id = created["suggestion"]["id"].as_str().unwrap();

    clock.set("2026-09-06T09:00:00Z".parse().unwrap());
    let listed = app
        .clone()
        .oneshot(request("GET", "/v1/suggestions?status=expired", None, true))
        .await
        .unwrap();
    let listed = body_json(listed).await;
    assert_eq!(listed["suggestions"].as_array().unwrap().len(), 1);

    let expired = app
        .oneshot(request("GET", &format!("/v1/suggestions/{id}"), None, true))
        .await
        .unwrap();
    let expired = body_json(expired).await;
    assert_eq!(expired["suggestion"]["status"], "expired");
    assert_eq!(expired["suggestion"]["revision"], 2);
}
