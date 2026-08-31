use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use chrono::{DateTime, Utc};
use dayweave_api::{
    AppState,
    auth::{
        AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope,
        StaticTokenAuthenticator,
    },
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

#[derive(Clone)]
struct PrincipalAuthenticator(Principal);

#[async_trait]
impl Authenticator for PrincipalAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        if token == TOKEN {
            Ok(self.0.clone())
        } else {
            Err(AuthenticationError::InvalidCredentials)
        }
    }
}

fn app_with_principal(principal: Principal) -> Router {
    let repository: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let service = Arc::new(ProposalService::new(
        repository,
        Arc::new(TestClock::new(Utc::now())),
        Duration::from_hours(7 * 24),
    ));
    router(AppState::new(
        service,
        Arc::new(PrincipalAuthenticator(principal)),
        Readiness::default(),
    ))
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
#[allow(clippy::too_many_lines)] // Keeps the public system and OpenAPI surface in one contract test.
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
    assert!(document["paths"]["/v1/suggestions/application-previews"].is_object());
    assert!(document["paths"]["/v1/suggestions/application-previews/{id}/apply"].is_object());
    assert!(document["paths"]["/v1/suggestions/applications/{id}"].is_object());
    assert!(document["paths"]["/v1/suggestions/applications/{id}/undo"].is_object());
    assert!(document["paths"]["/v1/suggestions/{id}/application"].is_object());
    for schema in [
        "ProposalChangeSet",
        "ProposalChangeSetSchema",
        "ProposalCommand",
        "ProposalImplicitChangeReason",
        "ProposalImplicitItemDiff",
        "ProposalOperation",
    ] {
        assert!(
            document["components"]["schemas"][schema].is_object(),
            "OpenAPI must declare the {schema} schema"
        );
    }
    for schema in [
        "DeferAssessment",
        "DeferAssessmentEnvelope",
        "DeferAssessmentRequest",
        "DeferExecution",
        "ExecutionCommand",
        "ExecutionSession",
        "ExecutionStatus",
    ] {
        assert!(
            document["components"]["schemas"][schema].is_object(),
            "OpenAPI must declare the {schema} schema"
        );
    }
    let defer_required = document["components"]["schemas"]["DeferExecution"]["required"]
        .as_array()
        .expect("defer required fields");
    for field in [
        "session_id",
        "move_start",
        "move_end",
        "actual_seconds",
        "assessment_digest",
    ] {
        assert!(
            defer_required.iter().any(|required| required == field),
            "OpenAPI must require defer {field}"
        );
    }
    assert!(
        !defer_required
            .iter()
            .any(|required| required == "approved_assessment_digest"),
        "approval is present only for a conflicting assessment"
    );
    let assessment_path = &document["paths"]["/v1/execution/defer-assessments"]["post"];
    assert_eq!(
        assessment_path["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/DeferAssessmentRequest"
    );
    assert_eq!(
        assessment_path["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/DeferAssessmentEnvelope"
    );
    let assessment_request = &document["components"]["schemas"]["DeferAssessmentRequest"];
    assert_eq!(assessment_request["additionalProperties"], false);
    let assessment_required = assessment_request["required"]
        .as_array()
        .expect("defer assessment request required fields");
    for field in ["expected_revision", "session_id", "move_start"] {
        assert!(
            assessment_required.iter().any(|required| required == field),
            "OpenAPI must require defer assessment {field}"
        );
    }
    assert!(
        !assessment_required
            .iter()
            .any(|required| required == "actual_seconds")
    );
    let stream_path = &document["paths"]["/v1/execution/stream"]["get"];
    assert_eq!(
        stream_path["responses"]["200"]["content"]["text/event-stream"]["schema"]["type"],
        "string"
    );
    for status in ["400", "401", "403", "406", "409", "503"] {
        assert!(
            stream_path["responses"][status].is_object(),
            "OpenAPI must declare execution stream response {status}"
        );
    }
    let stream_parameters = stream_path["parameters"]
        .as_array()
        .expect("execution stream header parameters");
    for name in ["Accept", "Last-Event-ID"] {
        assert!(
            stream_parameters
                .iter()
                .any(|parameter| parameter["name"] == name && parameter["in"] == "header"),
            "OpenAPI must declare execution stream header {name}"
        );
    }
    for field in [
        "execution_revision",
        "session_revision",
        "replacement_session_index",
        "remaining_duration_seconds",
        "environment_digest",
        "assessment_digest",
        "approval_required",
        "violations",
        "expires_at",
    ] {
        assert!(
            document["components"]["schemas"]["DeferAssessment"]["properties"][field].is_object(),
            "OpenAPI must expose defer assessment {field}"
        );
    }
    assert!(
        document["components"]["schemas"]["DeferAssessmentEnvelope"]["properties"]["assessment"]
            .is_object()
    );
    assert!(
        document["components"]["schemas"]["ExecutionStatus"]["enum"]
            .as_array()
            .expect("execution status values")
            .iter()
            .any(|status| status == "deferred")
    );
    for path in [
        "/v1/suggestions/application-previews/{id}/apply",
        "/v1/suggestions/applications/{id}/undo",
    ] {
        let parameters = document["paths"][path]["post"]["parameters"]
            .as_array()
            .expect("mutating proposal application route parameters");
        let idempotency_key = parameters
            .iter()
            .find(|parameter| parameter["name"] == "Idempotency-Key")
            .expect("Idempotency-Key header parameter");
        assert_eq!(idempotency_key["in"], "header");
        assert_eq!(idempotency_key["required"], true);
    }
    assert!(document["paths"]["/v1/schedule/preview"].is_object());
    assert!(document["paths"]["/v1/schedule/publish"].is_object());
    assert!(document["paths"]["/v1/schedule/manual-placements"]["get"].is_object());
    for schema_name in [
        "ManualPlacementInput",
        "ManualPlacementAssignmentInput",
        "ManualPlacementReleaseInput",
        "ManualPlacementApproval",
        "ManualPlacementAssessmentOutput",
        "ManualPlacementViolationOutput",
        "ManualPlacementConflictOutput",
        "RetainedManualPlacementCatalog",
        "RetainedManualPlacementSummary",
        "RetainedManualPlacementAssignmentSummary",
    ] {
        assert!(
            document["components"]["schemas"][schema_name].is_object(),
            "OpenAPI must register {schema_name}"
        );
        assert_eq!(
            document["components"]["schemas"][schema_name]["additionalProperties"], false,
            "{schema_name} must remain a closed object contract"
        );
    }
    assert!(document["paths"]["/v1/schedule/publish"]["post"]["responses"]["200"].is_object());
    assert!(document["paths"]["/v1/schedule/publish"]["post"]["responses"]["201"].is_null());
    assert!(document["paths"]["/v1/schedule/preview"]["post"]["responses"]["413"].is_object());
    assert!(document["paths"]["/v1/schedule/publish"]["post"]["responses"]["413"].is_object());
    let discover = &document["paths"]["/v1/integrations/google/accounts/{account_id}/collections/discover"]
        ["post"];
    assert!(discover["requestBody"].is_null());
    let refresh =
        &document["paths"]["/v1/integrations/google/accounts/{account_id}/sync/refresh"]["post"];
    assert_eq!(
        refresh["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/GoogleSyncRefreshRequest"
    );
    assert!(
        document["components"]["schemas"]["ComposeScheduleResult"]["properties"]
            .get("source_item_sensitivity")
            .is_none()
    );
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
    assert!(document["paths"]["/v1/auth/device-enrollments"]["post"].is_object());
    assert!(
        document["paths"]["/v1/auth/device-enrollments"]["post"]["responses"]["200"].is_object()
    );
    assert!(
        document["paths"]["/v1/auth/device-enrollments"]["post"]["responses"]["201"].is_object()
    );
    assert!(document["paths"]["/v1/auth/device-enrollments/consume"]["post"].is_object());
    assert!(document["paths"]["/v1/auth/sessions/refresh"]["post"].is_object());
    assert!(document["paths"]["/v1/auth/sessions"]["get"].is_object());
    assert!(document["paths"]["/v1/auth/sessions/{id}"]["delete"].is_object());
    assert!(document["paths"]["/v1/auth/mcp-clients"]["post"].is_object());
    assert!(document["paths"]["/v1/auth/mcp-clients"]["get"].is_object());
    assert!(document["paths"]["/v1/auth/mcp-clients/{id}"]["delete"].is_object());
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
async fn rest_scopes_and_credential_audiences_are_enforced_before_handlers() {
    let device = Principal {
        subject: "device-session:synthetic".to_owned(),
        scopes: vec![Scope::ItemsRead],
        audience: PrincipalAudience::Device,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    };
    let app = app_with_principal(device);
    let read = app
        .clone()
        .oneshot(request("GET", "/v1/items", None, true))
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let write = app
        .oneshot(request("POST", "/v1/items", Some(json!({})), true))
        .await
        .unwrap();
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(write).await["error"]["code"], "forbidden");

    let assessment_body = json!({
        "expected_revision": 0,
        "session_id": "00000000-0000-4000-8000-000000000201",
        "move_start": "2026-10-02T09:30:00Z",
        "actual_seconds": null
    });
    let execution_reader = Principal {
        subject: "device-session:execution-reader".to_owned(),
        scopes: vec![Scope::ExecutionRead],
        audience: PrincipalAudience::Device,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    };
    let forbidden_assessment = app_with_principal(execution_reader)
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(assessment_body.clone()),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(forbidden_assessment.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(forbidden_assessment).await["error"]["code"],
        "forbidden"
    );

    let execution_writer = Principal {
        subject: "device-session:execution-writer".to_owned(),
        scopes: vec![Scope::ExecutionWrite],
        audience: PrincipalAudience::Device,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    };
    let allowed_assessment = app_with_principal(execution_writer)
        .oneshot(request(
            "POST",
            "/v1/execution/defer-assessments",
            Some(assessment_body),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(allowed_assessment.status(), StatusCode::SERVICE_UNAVAILABLE);

    let mcp = Principal {
        subject: "mcp-client:synthetic".to_owned(),
        scopes: vec![Scope::ScheduleRead],
        audience: PrincipalAudience::Mcp,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    };
    let response = app_with_principal(mcp)
        .oneshot(request("GET", "/v1/items", None, true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn execution_stream_requires_read_scope_and_native_rest_audience() {
    async fn stream_response(principal: Principal) -> Response<Body> {
        let mut stream = request("GET", "/v1/execution/stream", None, true);
        stream.headers_mut().insert(
            header::ACCEPT,
            "text/event-stream".parse().expect("valid Accept"),
        );
        app_with_principal(principal).oneshot(stream).await.unwrap()
    }

    let principal = |subject: &str, scopes, audience| Principal {
        subject: subject.to_owned(),
        scopes,
        audience,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    };

    let allowed = stream_response(principal(
        "device-session:execution-stream-reader",
        vec![Scope::ExecutionRead],
        PrincipalAudience::Device,
    ))
    .await;
    assert_eq!(allowed.status(), StatusCode::OK);
    drop(allowed);

    let forbidden = stream_response(principal(
        "device-session:execution-stream-writer",
        vec![Scope::ExecutionWrite],
        PrincipalAudience::Device,
    ))
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(forbidden).await["error"]["code"], "forbidden");

    let wrong_audience = stream_response(principal(
        "mcp-client:execution-stream-reader",
        vec![Scope::ExecutionRead],
        PrincipalAudience::Mcp,
    ))
    .await;
    assert_eq!(wrong_audience.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        body_json(wrong_audience).await["error"]["code"],
        "unauthorized"
    );
}

#[tokio::test]
async fn device_rest_derives_first_party_suggestion_provenance() {
    let subject = "device-session:synthetic";
    let device = Principal {
        subject: subject.to_owned(),
        scopes: vec![Scope::SuggestionsWrite],
        audience: PrincipalAudience::Device,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    };
    let created = app_with_principal(device)
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
    assert_eq!(created["suggestion"]["source"], "app_assistant");
    assert_eq!(created["suggestion"]["source_reference"], Value::Null);
    assert_eq!(created["suggestion"]["submitted_by"], subject);
}

#[tokio::test]
async fn legacy_mode_does_not_activate_durable_issuance_and_auth_errors_are_no_store() {
    let (app, _) = test_app(true);
    let protected = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/auth/device-enrollments",
            Some(json!({
                "id": "00000000-0000-4000-8000-000000000122",
                "enrollment_token": "synthetic-enrollment-token",
                "client_instance_id": "00000000-0000-4000-8000-000000000123",
                "client_kind": "macos",
                "device_label": "Synthetic Mac",
                "client_version": "test-1"
            })),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(protected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        protected.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );

    let public = app
        .oneshot(request(
            "POST",
            "/v1/auth/sessions/refresh",
            Some(json!({
                "next_access_token": "never-reflect-access",
                "next_refresh_token": "never-reflect-refresh"
            })),
            false,
        ))
        .await
        .unwrap();
    assert_eq!(public.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(public.headers()[header::PRAGMA], "no-cache");
    let body = String::from_utf8(
        public
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("never-reflect"));
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
async fn actionable_suggestions_cannot_bypass_atomic_application_or_owner_credentials() {
    let (app, _) = test_app(true);
    let proposal_id = "00000000-0000-4000-8000-000000000101";
    let command_id = "00000000-0000-4000-8000-000000000102";
    let item_id = "00000000-0000-4000-8000-000000000103";
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/suggestions",
            Some(json!({
                "source": "codex",
                "kind": "create_item",
                "title": "Create a review task",
                "payload": {
                    "schema": "dayweave.proposal-change-set/1",
                    "commands": [{
                        "operation": "create_item",
                        "command_id": command_id,
                        "item": {
                            "id": item_id,
                            "is_sensitive": false,
                            "kind": "task",
                            "status": "inbox",
                            "title": "Review the plan",
                            "notes": null,
                            "timezone_name": "Europe/Madrid",
                            "duration_seconds": 1800,
                            "deadline_at": null,
                            "earliest_start_at": null,
                            "recurrence": null,
                            "flexible_constraints": {},
                            "split_policy": {"type": "indivisible"},
                            "importance": 50,
                            "urgency": 40,
                            "parent_id": null,
                            "sibling_order": 0
                        }
                    }]
                }
            })),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = body_json(created).await;
    let created_id = created["suggestion"]["id"].as_str().unwrap();
    assert_ne!(created_id, proposal_id);

    let bypass = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/suggestions/{created_id}/accept"),
            Some(json!({"expected_revision": 1})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(bypass.status(), StatusCode::CONFLICT);

    let legacy_preview = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/suggestions/application-previews",
            Some(json!({
                "proposals": [{
                    "proposal_id": created_id,
                    "expected_revision": 1
                }]
            })),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(legacy_preview.status(), StatusCode::FORBIDDEN);

    let unchanged = app
        .oneshot(request(
            "GET",
            &format!("/v1/suggestions/{created_id}"),
            None,
            true,
        ))
        .await
        .unwrap();
    let unchanged = body_json(unchanged).await;
    assert_eq!(unchanged["suggestion"]["status"], "pending");
    assert_eq!(unchanged["suggestion"]["revision"], 1);
}

#[tokio::test]
async fn malformed_and_future_change_set_namespaces_cannot_use_legacy_accept() {
    let (app, _) = test_app(true);
    let reserved_payloads = [
        json!({
            "schema": "dayweave.proposal-change-set/1",
            "commands": "malformed"
        }),
        json!({
            "schema": "dayweave.proposal-change-set/1",
            "commands": [],
            "future_field": true
        }),
        json!({
            "schema": "dayweave.proposal-change-set/2",
            "commands": []
        }),
        json!({
            "schema": "dayweave.proposal-change-set/",
            "commands": []
        }),
    ];

    for (index, payload) in reserved_payloads.into_iter().enumerate() {
        let created = app
            .clone()
            .oneshot(request(
                "POST",
                "/v1/suggestions",
                Some(json!({
                    "source": "codex",
                    "kind": "recommendation",
                    "title": format!("Reserved proposal {index}"),
                    "payload": payload
                })),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created = body_json(created).await;
        let id = created["suggestion"]["id"].as_str().unwrap();

        let bypass = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/v1/suggestions/{id}/accept"),
                Some(json!({"expected_revision": 1})),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(bypass.status(), StatusCode::CONFLICT);

        let unchanged = app
            .clone()
            .oneshot(request("GET", &format!("/v1/suggestions/{id}"), None, true))
            .await
            .unwrap();
        let unchanged = body_json(unchanged).await;
        assert_eq!(unchanged["suggestion"]["status"], "pending");
        assert_eq!(unchanged["suggestion"]["revision"], 1);
    }
}

#[tokio::test]
async fn legacy_accept_is_revision_bound_to_the_payload_it_classifies() {
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
    let created = body_json(created).await;
    let id = created["suggestion"]["id"].as_str().unwrap();

    let edited = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/v1/suggestions/{id}"),
            Some(json!({
                "expected_revision": 1,
                "payload": {
                    "schema": "dayweave.proposal-change-set/2",
                    "commands": []
                }
            })),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(edited.status(), StatusCode::OK);

    let stale_accept = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/suggestions/{id}/accept"),
            Some(json!({"expected_revision": 1})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(stale_accept.status(), StatusCode::CONFLICT);
    let stale_accept = body_json(stale_accept).await;
    assert_eq!(stale_accept["error"]["details"]["actual_revision"], 2);

    let current_accept = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/suggestions/{id}/accept"),
            Some(json!({"expected_revision": 2})),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(current_accept.status(), StatusCode::CONFLICT);

    let unchanged = app
        .oneshot(request("GET", &format!("/v1/suggestions/{id}"), None, true))
        .await
        .unwrap();
    let unchanged = body_json(unchanged).await;
    assert_eq!(unchanged["suggestion"]["status"], "pending");
    assert_eq!(unchanged["suggestion"]["revision"], 2);
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
