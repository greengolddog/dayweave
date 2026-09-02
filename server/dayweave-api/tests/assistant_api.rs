use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use dayweave_api::{
    AppState,
    assistant::{
        AssistantProvider, AssistantProviderError, AssistantProviderRequest,
        AssistantProviderResponse, AssistantTokenUsage, MAX_REPLY_BYTES,
    },
    auth::{
        AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope,
        StaticTokenAuthenticator,
    },
    http::router,
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const TOKEN: &str = "assistant-integration-token";
const REQUEST_ID: &str = "00000000-0000-4000-8000-000000000041";

struct CapturedTurn {
    request_id: uuid::Uuid,
    message: String,
    history_entries: usize,
    private_spans: usize,
}

struct ReplyProvider {
    calls: AtomicUsize,
    captured: Mutex<Option<CapturedTurn>>,
    failure: Option<AssistantProviderError>,
    reply: String,
}

impl ReplyProvider {
    fn success() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            captured: Mutex::new(None),
            failure: None,
            reply: "The redacted schedule has a free hour after item-1.".to_owned(),
        }
    }
}

#[async_trait]
impl AssistantProvider for ReplyProvider {
    async fn respond(
        &self,
        request: AssistantProviderRequest,
    ) -> Result<AssistantProviderResponse, AssistantProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.captured.lock().expect("capture lock") = Some(CapturedTurn {
            request_id: request.request_id,
            message: request.message,
            history_entries: request.history.len(),
            private_spans: request.context.private_busy_spans.len(),
        });
        if let Some(error) = self.failure {
            return Err(error);
        }
        Ok(AssistantProviderResponse {
            reply: self.reply.clone(),
            model: "test-advisory-model".to_owned(),
            generated_at: "2026-09-03T08:01:00Z".parse().unwrap(),
            usage: AssistantTokenUsage {
                input_tokens: 200,
                output_tokens: 20,
                total_tokens: 220,
            },
        })
    }
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

fn proposal_service() -> Arc<ProposalService> {
    let repository: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    Arc::new(ProposalService::new(
        repository,
        Arc::new(SystemClock),
        Duration::from_hours(7 * 24),
    ))
}

fn app_with_principal(
    principal: Principal,
    provider: Option<Arc<dyn AssistantProvider>>,
) -> Router {
    let state = AppState::new(
        proposal_service(),
        Arc::new(PrincipalAuthenticator(principal)),
        Readiness::default(),
    );
    router(provider.map_or(state.clone(), |provider| {
        state.with_assistant_provider(provider)
    }))
}

fn device(scopes: Vec<Scope>) -> Principal {
    Principal {
        subject: "device-session:assistant-test".to_owned(),
        scopes,
        audience: PrincipalAudience::Device,
        workspace_id: Some(uuid::Uuid::from_u128(2)),
        user_id: Some(uuid::Uuid::from_u128(1)),
        credential_id: Some(uuid::Uuid::from_u128(3)),
        allowed_origins: Vec::new(),
    }
}

fn principal(audience: PrincipalAudience, scopes: Vec<Scope>) -> Principal {
    Principal {
        subject: "synthetic-principal".to_owned(),
        scopes,
        audience,
        workspace_id: None,
        user_id: None,
        credential_id: None,
        allowed_origins: Vec::new(),
    }
}

fn valid_turn() -> Value {
    json!({
        "request_id": REQUEST_ID,
        "message": "  What should I do next?  ",
        "history": [{"role":"assistant", "content":"I can inspect the redacted plan."}],
        "context": {
            "schema": "dayweave.assistant-context/1",
            "generated_at": "2026-09-03T08:00:00Z",
            "timezone": "Europe/Paris",
            "scheduled_blocks": [{
                "reference": "block-1",
                "title": "Public meeting",
                "kind": "event",
                "starts_at": "2026-09-03T09:00:00Z",
                "ends_at": "2026-09-03T10:00:00Z",
                "duration_minutes": 60,
                "status": "planned",
                "project": null,
                "energy": "medium",
                "is_flexible": false,
                "is_hard_constraint": true
            }],
            "private_busy_spans": [{
                "starts_at": "2026-09-03T11:00:00Z",
                "ends_at": "2026-09-03T12:00:00Z",
                "duration_minutes": 60
            }],
            "total_scheduled_block_count": 2,
            "planner_items": [{
                "reference": "item-1",
                "parent_reference": null,
                "title": "Write the report",
                "kind": "task",
                "status": "active",
                "timezone": "Europe/Paris",
                "duration_minutes": 45,
                "deadline_at": null,
                "earliest_start_at": null,
                "split_policy": "indivisible",
                "importance": 70,
                "urgency": 60,
                "is_recurring": false,
                "is_executable": true
            }],
            "total_planner_item_count": 1,
            "pending_suggestion_count": 0,
            "omitted_fields": [
                "account identity and credentials",
                "app-storage paths and server configuration",
                "notes and placement diagnostics",
                "raw recurrence and flexible-constraint payloads",
                "stable item, occurrence, and revision identifiers",
                "sensitive item content; occupancy is represented only as generic busy spans"
            ]
        }
    })
}

fn request(body: impl Into<Body>, authenticated: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/assistant/turns")
        .header(header::CONTENT_TYPE, "application/json");
    if authenticated {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    builder.body(body.into()).unwrap()
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn device_turn_is_advisory_bounded_and_never_cacheable() {
    let provider = Arc::new(ReplyProvider::success());
    let app = app_with_principal(
        device(vec![Scope::ScheduleRead, Scope::ItemsRead]),
        Some(provider.clone()),
    );
    let response = app
        .oneshot(request(valid_turn().to_string(), true))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(response.headers()[header::PRAGMA], "no-cache");
    let body = body_json(response).await;
    assert_eq!(body["request_id"], REQUEST_ID);
    assert_eq!(body["model"], "test-advisory-model");
    assert_eq!(body["generated_at"], "2026-09-03T08:01:00Z");
    let captured = provider
        .captured
        .lock()
        .expect("capture lock")
        .take()
        .expect("captured turn");
    assert_eq!(captured.request_id.to_string(), REQUEST_ID);
    assert_eq!(captured.message, "What should I do next?");
    assert_eq!(captured.history_entries, 1);
    assert_eq!(captured.private_spans, 1);
}

#[tokio::test]
async fn legacy_is_allowed_but_native_device_requires_both_read_scopes() {
    let legacy_provider = Arc::new(ReplyProvider::success());
    let legacy = app_with_principal(
        Principal::legacy("legacy-test".to_owned()),
        Some(legacy_provider.clone()),
    )
    .oneshot(request(valid_turn().to_string(), true))
    .await
    .unwrap();
    assert_eq!(legacy.status(), StatusCode::OK);
    assert_eq!(legacy_provider.calls.load(Ordering::SeqCst), 1);

    for scopes in [vec![Scope::ScheduleRead], vec![Scope::ItemsRead]] {
        let provider = Arc::new(ReplyProvider::success());
        let forbidden = app_with_principal(device(scopes), Some(provider.clone()))
            .oneshot(request(valid_turn().to_string(), true))
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn mcp_audiences_and_missing_credentials_are_rejected_before_provider() {
    let provider = Arc::new(ReplyProvider::success());
    let mcp = app_with_principal(
        principal(
            PrincipalAudience::Mcp,
            vec![Scope::ScheduleRead, Scope::ItemsRead],
        ),
        Some(provider.clone()),
    )
    .oneshot(request(valid_turn().to_string(), true))
    .await
    .unwrap();
    assert_eq!(mcp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    let unauthenticated = app_with_principal(
        device(vec![Scope::ScheduleRead, Scope::ItemsRead]),
        Some(provider.clone()),
    )
    .oneshot(request(valid_turn().to_string(), false))
    .await
    .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disabled_provider_fails_closed_without_reflecting_context() {
    let authenticator = Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN]));
    let app = router(AppState::new(
        proposal_service(),
        authenticator,
        Readiness::default(),
    ));
    let response = app
        .oneshot(request(valid_turn().to_string(), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let body = body_json(response).await.to_string();
    assert!(!body.contains("Public meeting"));
    assert!(!body.contains("Write the report"));
}

#[tokio::test]
async fn strict_request_parser_rejects_duplicates_unknown_private_fields_and_bounds() {
    let provider = Arc::new(ReplyProvider::success());
    let make_app = || {
        app_with_principal(
            device(vec![Scope::ScheduleRead, Scope::ItemsRead]),
            Some(provider.clone()),
        )
    };

    let duplicate = format!(
        r#"{{"request_id":"{REQUEST_ID}","request_id":"{REQUEST_ID}","message":"hello","history":[],"context":{{}}}}"#
    );
    let response = make_app().oneshot(request(duplicate, true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let mut private_metadata = valid_turn();
    private_metadata["context"]["private_busy_spans"][0]["title"] = json!("secret canary");
    let response = make_app()
        .oneshot(request(private_metadata.to_string(), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let mut oversized_message = valid_turn();
    oversized_message["message"] = json!("x".repeat(8 * 1024 + 1));
    let response = make_app()
        .oneshot(request(oversized_message.to_string(), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = make_app()
        .oneshot(request(" ".repeat(128 * 1024 + 1), true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn injected_provider_output_is_revalidated_at_the_http_boundary() {
    let provider = Arc::new(ReplyProvider {
        calls: AtomicUsize::new(0),
        captured: Mutex::new(None),
        failure: None,
        reply: "x".repeat(MAX_REPLY_BYTES + 1),
    });
    let response = app_with_principal(
        device(vec![Scope::ScheduleRead, Scope::ItemsRead]),
        Some(provider),
    )
    .oneshot(request(valid_turn().to_string(), true))
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers()[header::PRAGMA], "no-cache");
}

#[tokio::test]
async fn provider_budget_limit_is_stable_non_cacheable_and_does_not_echo_context() {
    let provider = Arc::new(ReplyProvider {
        calls: AtomicUsize::new(0),
        captured: Mutex::new(None),
        failure: Some(AssistantProviderError::RateLimited),
        reply: String::new(),
    });
    let response = app_with_principal(
        device(vec![Scope::ScheduleRead, Scope::ItemsRead]),
        Some(provider),
    )
    .oneshot(request(valid_turn().to_string(), true))
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(response.headers()[header::PRAGMA], "no-cache");
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "rate_limited");
    assert!(!body.to_string().contains("Public meeting"));
}

#[tokio::test]
async fn openapi_registers_the_closed_assistant_contract() {
    let app = app_with_principal(device(vec![]), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let document = body_json(response).await;
    let operation = &document["paths"]["/v1/assistant/turns"]["post"];
    assert_eq!(
        operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/AssistantTurnRequest"
    );
    assert_eq!(
        operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/AssistantTurnResponse"
    );
    for schema in [
        "AssistantTurnRequest",
        "AssistantHistoryEntry",
        "AssistantContext",
        "AssistantScheduledBlock",
        "AssistantPrivateBusySpan",
        "AssistantPlannerItem",
    ] {
        assert_eq!(
            document["components"]["schemas"][schema]["additionalProperties"], false,
            "{schema} must remain closed"
        );
    }
    assert!(
        document["components"]["schemas"]["AssistantHistoryEntry"]["properties"]["content"]
            .is_object()
    );
}
