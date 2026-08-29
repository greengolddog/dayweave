use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, Response, StatusCode, header},
};
use chrono::{DateTime, Utc};
use dayweave_api::{
    AppState,
    auth::{
        AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope,
        StaticTokenAuthenticator,
    },
    http::router,
    proposals::{
        Clock, InMemoryProposalRepository, ProposalQuery, ProposalRepository, ProposalService,
        ProposalSource, ProposalStatus,
    },
    readiness::Readiness,
    scheduling::{
        InMemoryScheduleQueryPort, InMemorySimulationPort, PlacementAlternative,
        PlacementExplanation, PlacementReason, PlanningSimulationPort, ScheduleAccess,
        ScheduleConflict, SchedulingPortError, SimulationRequest, SimulationResult, StoredItem,
        StoredSchedule, StoredScheduleBlock,
    },
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "mcp-integration-token";
const CURRENT_VERSION: &str = "2026-07-28";
const LEGACY_VERSION: &str = "2025-11-25";
const PRIVATE_CANARY: &str = "CANARY-PRIVATE-MEDICAL-APPOINTMENT";

#[derive(Clone)]
struct TestClock(Arc<RwLock<DateTime<Utc>>>);

impl TestClock {
    fn new(now: DateTime<Utc>) -> Self {
        Self(Arc::new(RwLock::new(now)))
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.read().expect("clock lock")
    }
}

#[derive(Debug)]
struct RepublishRequiredSimulationPort;

#[async_trait]
impl PlanningSimulationPort for RepublishRequiredSimulationPort {
    async fn simulate(
        &self,
        _access: &ScheduleAccess,
        _request: SimulationRequest,
    ) -> Result<SimulationResult, SchedulingPortError> {
        Err(SchedulingPortError::RepublishRequired)
    }

    async fn consume_simulation(
        &self,
        _access: &ScheduleAccess,
        _token: &str,
        _expected_request_digest: &str,
    ) -> Result<SimulationResult, SchedulingPortError> {
        Err(SchedulingPortError::RepublishRequired)
    }
}

struct McpFixture {
    app: Router,
    proposals: Arc<ProposalService>,
    schedule: InMemoryScheduleQueryPort,
}

fn fixture() -> McpFixture {
    fixture_with_authenticator(Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])))
}

fn fixture_with_authenticator(authenticator: Arc<dyn Authenticator>) -> McpFixture {
    let stored_schedule = schedule_fixture();
    let schedule = InMemoryScheduleQueryPort::new(
        stored_schedule.clone(),
        item_fixture(),
        explanation_fixture(),
        conflict_fixture(),
    );
    let simulations = InMemorySimulationPort::new(stored_schedule);
    let repository: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        repository,
        Arc::new(TestClock::new("2026-08-29T08:00:00Z".parse().unwrap())),
        Duration::from_hours(7 * 24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let state = AppState::new(proposals.clone(), authenticator, readiness).with_mcp_ports(
        Arc::new(schedule.clone()),
        Arc::new(simulations),
        Arc::new(vec!["https://chatgpt.com".to_owned()]),
    );
    McpFixture {
        app: router(state),
        proposals,
        schedule,
    }
}

fn republish_required_fixture() -> Router {
    let stored_schedule = schedule_fixture();
    let schedule = InMemoryScheduleQueryPort::new(
        stored_schedule,
        item_fixture(),
        explanation_fixture(),
        conflict_fixture(),
    );
    let repository: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        repository,
        Arc::new(TestClock::new("2026-08-29T08:00:00Z".parse().unwrap())),
        Duration::from_hours(7 * 24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let state = AppState::new(
        proposals,
        Arc::new(StaticTokenAuthenticator::from_plaintext(&[TOKEN])),
        readiness,
    )
    .with_mcp_ports(
        Arc::new(schedule),
        Arc::new(RepublishRequiredSimulationPort),
        Arc::new(vec!["https://chatgpt.com".to_owned()]),
    );
    router(state)
}

fn schedule_fixture() -> StoredSchedule {
    StoredSchedule {
        revision: "revision-7".to_owned(),
        timezone: "Europe/Madrid".to_owned(),
        blocks: vec![
            StoredScheduleBlock {
                id: "block-public".to_owned(),
                item_id: Some("item-public".to_owned()),
                title: "Write weekly plan".to_owned(),
                start: "2026-08-29T09:00:00Z".parse().unwrap(),
                end: "2026-08-29T10:00:00Z".parse().unwrap(),
                kind: "planned".to_owned(),
                status: "scheduled".to_owned(),
                sensitive: false,
            },
            StoredScheduleBlock {
                id: "block-private".to_owned(),
                item_id: Some("item-private".to_owned()),
                title: PRIVATE_CANARY.to_owned(),
                start: "2026-08-29T10:00:00Z".parse().unwrap(),
                end: "2026-08-29T11:00:00Z".parse().unwrap(),
                kind: "calendar_event".to_owned(),
                status: "scheduled".to_owned(),
                sensitive: true,
            },
        ],
    }
}

fn item_fixture() -> Vec<StoredItem> {
    vec![
        StoredItem {
            id: "item-public".to_owned(),
            title: "Write weekly plan".to_owned(),
            status: "scheduled".to_owned(),
            kind: "task".to_owned(),
            project_id: Some("project-1".to_owned()),
            goal_id: None,
            deadline: Some("2026-08-30T18:00:00Z".parse().unwrap()),
            scheduled_start: Some("2026-08-29T09:00:00Z".parse().unwrap()),
            sensitive: false,
        },
        StoredItem {
            id: "item-private".to_owned(),
            title: PRIVATE_CANARY.to_owned(),
            status: "scheduled".to_owned(),
            kind: "calendar_event".to_owned(),
            project_id: None,
            goal_id: None,
            deadline: None,
            scheduled_start: Some("2026-08-29T10:00:00Z".parse().unwrap()),
            sensitive: true,
        },
    ]
}

fn explanation_fixture() -> Vec<PlacementExplanation> {
    vec![
        PlacementExplanation {
            block_id: "block-public".to_owned(),
            summary: "Placed before the deadline in a preferred focus window.".to_owned(),
            reasons: vec![PlacementReason {
                code: "hard_deadline".to_owned(),
                message: "This slot protects the deadline.".to_owned(),
                strength: "hard".to_owned(),
            }],
            active_constraints: vec!["allowed weekday".to_owned()],
            alternatives: vec![PlacementAlternative {
                start: "2026-08-29T12:00:00Z".parse().unwrap(),
                end: "2026-08-29T13:00:00Z".parse().unwrap(),
                tradeoff: "Adds one context switch.".to_owned(),
            }],
            stability_cost: 0,
            sensitive: false,
        },
        PlacementExplanation {
            block_id: "block-private".to_owned(),
            summary: PRIVATE_CANARY.to_owned(),
            reasons: Vec::new(),
            active_constraints: Vec::new(),
            alternatives: Vec::new(),
            stability_cost: 0,
            sensitive: true,
        },
    ]
}

fn conflict_fixture() -> Vec<ScheduleConflict> {
    vec![
        ScheduleConflict {
            id: "conflict-public".to_owned(),
            kind: "deadline_risk".to_owned(),
            severity: "warning".to_owned(),
            message: "Little slack remains before the deadline.".to_owned(),
            start: Some("2026-08-29T09:00:00Z".parse().unwrap()),
            end: Some("2026-08-29T10:00:00Z".parse().unwrap()),
            related_item_ids: vec!["item-public".to_owned()],
            penalty: 20,
            sensitive: false,
        },
        ScheduleConflict {
            id: "conflict-private".to_owned(),
            kind: "private".to_owned(),
            severity: "warning".to_owned(),
            message: PRIVATE_CANARY.to_owned(),
            start: Some("2026-08-29T10:00:00Z".parse().unwrap()),
            end: Some("2026-08-29T11:00:00Z".parse().unwrap()),
            related_item_ids: vec!["item-private".to_owned()],
            penalty: 0,
            sensitive: true,
        },
    ]
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": CURRENT_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "dayweave-contract-test",
            "version": "1.0.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

#[allow(clippy::needless_pass_by_value)]
fn modern_request(
    method: &str,
    id: Value,
    mut params: Value,
    tool_name: Option<&str>,
) -> Request<Body> {
    params["_meta"] = modern_meta();
    let mut builder = base_builder()
        .header("mcp-protocol-version", CURRENT_VERSION)
        .header("mcp-method", method)
        .header("x-request-id", "mcp-test-request-id");
    if let Some(name) = tool_name {
        builder = builder.header("mcp-name", name);
    }
    builder
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
            .to_string(),
        ))
        .unwrap()
}

fn base_builder() -> axum::http::request::Builder {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
}

#[allow(clippy::needless_pass_by_value)]
fn tool_request(name: &str, arguments: Value, id: i64) -> Request<Body> {
    modern_request(
        "tools/call",
        json!(id),
        json!({ "name": name, "arguments": arguments }),
        Some(name),
    )
}

fn proposal_request(arguments: Value, id: i64, mirrored_key: Option<&str>) -> Request<Body> {
    let mut request = tool_request("submit_proposal", arguments, id);
    if let Some(key) = mirrored_key {
        request.headers_mut().insert(
            "mcp-param-idempotency-key",
            key.parse().expect("valid idempotency header"),
        );
    }
    request
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    response_parts(response).await
}

async fn response_parts(response: Response<Body>) -> (StatusCode, HeaderMap, Value) {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, headers, body)
}

#[tokio::test]
async fn modern_discovery_is_stateless_self_describing_and_proposal_only() {
    let fixture = fixture();
    let request = modern_request("server/discover", json!(1), json!({}), None);

    let (status, headers, body) = send(&fixture.app, request).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["resultType"], "complete");
    assert_eq!(
        body["result"]["supportedVersions"],
        json!([CURRENT_VERSION, LEGACY_VERSION])
    );
    let instructions = body["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("proposal-only"));
    assert!(instructions.contains("never applies"));
    assert_eq!(
        body["result"]["_meta"]["com.greengolddog.dayweave/requestId"],
        "mcp-test-request-id"
    );
    assert_eq!(headers.get("x-request-id").unwrap(), "mcp-test-request-id");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store, max-age=0");
    assert!(body["result"].get("securitySchemes").is_none());
    assert!(!headers.contains_key("mcp-session-id"));
}

#[tokio::test]
async fn legacy_initialize_and_initialized_notification_remain_compatible_without_sessions() {
    let fixture = fixture();
    let initialize = base_builder()
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": "init-1",
                "method": "initialize",
                "params": {
                    "protocolVersion": LEGACY_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "legacy-client", "version": "1" }
                }
            })
            .to_string(),
        ))
        .unwrap();
    let (status, headers, body) = send(&fixture.app, initialize).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["protocolVersion"], LEGACY_VERSION);
    assert!(
        body["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("Suggestions Inbox")
    );
    assert!(!headers.contains_key("mcp-session-id"));

    let initialized = base_builder()
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
        ))
        .unwrap();
    let (status, _, body) = send(&fixture.app, initialized).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body.is_null());

    let legacy_list = base_builder()
        .header("mcp-protocol-version", LEGACY_VERSION)
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            })
            .to_string(),
        ))
        .unwrap();
    let (status, _, body) = send(&fixture.app, legacy_list).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["result"]["tools"].is_array());
    assert!(body["result"].get("resultType").is_none());
}

#[tokio::test]
async fn transport_rejects_missing_auth_bad_origins_and_invalid_media_contracts() {
    let fixture = fixture();
    let mut missing_auth = modern_request("server/discover", json!(1), json!({}), None);
    missing_auth.headers_mut().remove(header::AUTHORIZATION);
    let (status, headers, body) = send(&fixture.app, missing_auth).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let challenge = headers
        .get(header::WWW_AUTHENTICATE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(challenge.starts_with("Bearer realm=\"dayweave-native-mcp\""));
    assert!(!challenge.contains("resource_metadata"));
    assert_eq!(body["error"]["code"], -33001);

    let mut bad_origin = modern_request("server/discover", json!(2), json!({}), None);
    bad_origin
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    let (status, _, body) = send(&fixture.app, bad_origin).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], -33003);

    let mut allowed_origin = modern_request("server/discover", json!(3), json!({}), None);
    allowed_origin
        .headers_mut()
        .insert(header::ORIGIN, "https://chatgpt.com".parse().unwrap());
    assert_eq!(send(&fixture.app, allowed_origin).await.0, StatusCode::OK);

    let mut invalid_accept = modern_request("server/discover", json!(4), json!({}), None);
    invalid_accept
        .headers_mut()
        .insert(header::ACCEPT, "application/json".parse().unwrap());
    assert_eq!(
        send(&fixture.app, invalid_accept).await.0,
        StatusCode::NOT_ACCEPTABLE
    );
}

#[tokio::test]
async fn durable_mcp_origin_policy_is_the_intersection_and_device_audience_is_rejected() {
    let denied = fixture_with_authenticator(Arc::new(ScopedAuthenticator {
        token: TOKEN.to_owned(),
        scopes: vec![Scope::ScheduleRead],
        audience: PrincipalAudience::Mcp,
        allowed_origins: vec!["https://different.example".to_owned()],
    }));
    let mut globally_allowed = modern_request("server/discover", json!(1), json!({}), None);
    globally_allowed
        .headers_mut()
        .insert(header::ORIGIN, "https://chatgpt.com".parse().unwrap());
    assert_eq!(
        send(&denied.app, globally_allowed).await.0,
        StatusCode::FORBIDDEN,
        "global permission alone is insufficient for a durable MCP client"
    );
    assert_eq!(
        send(
            &denied.app,
            modern_request("server/discover", json!(2), json!({}), None),
        )
        .await
        .0,
        StatusCode::OK,
        "native clients without an Origin remain supported"
    );

    let allowed = fixture_with_authenticator(Arc::new(ScopedAuthenticator {
        token: TOKEN.to_owned(),
        scopes: vec![Scope::ScheduleRead],
        audience: PrincipalAudience::Mcp,
        allowed_origins: vec!["https://chatgpt.com".to_owned()],
    }));
    let mut intersected = modern_request("server/discover", json!(3), json!({}), None);
    intersected
        .headers_mut()
        .insert(header::ORIGIN, "https://chatgpt.com".parse().unwrap());
    assert_eq!(send(&allowed.app, intersected).await.0, StatusCode::OK);

    let device = fixture_with_authenticator(Arc::new(ScopedAuthenticator {
        token: TOKEN.to_owned(),
        scopes: vec![Scope::ScheduleRead],
        audience: PrincipalAudience::Device,
        allowed_origins: Vec::new(),
    }));
    assert_eq!(
        send(
            &device.app,
            modern_request("server/discover", json!(4), json!({}), None),
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn transport_reports_parse_unknown_method_and_unsupported_get_errors() {
    let fixture = fixture();
    let malformed = base_builder()
        .header("x-request-id", "malformed-request")
        .body(Body::from("{not valid json"))
        .unwrap();
    let (status, headers, body) = send(&fixture.app, malformed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32700);
    assert_eq!(body["error"]["data"]["requestId"], "malformed-request");
    assert_eq!(headers.get("x-request-id").unwrap(), "malformed-request");

    let unknown = modern_request("unknown/method", json!(7), json!({}), None);
    let (status, _, body) = send(&fixture.app, unknown).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], -32601);
    assert_eq!(body["id"], 7);

    let get = Request::builder()
        .method("GET")
        .uri("/mcp")
        .body(Body::empty())
        .unwrap();
    let response = fixture.app.clone().oneshot(get).await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn modern_header_and_body_metadata_are_strictly_cross_checked() {
    let fixture = fixture();
    let mut missing_method = modern_request("tools/list", json!(1), json!({}), None);
    missing_method.headers_mut().remove("mcp-method");
    let (status, _, body) = send(&fixture.app, missing_method).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32020);

    let mut wrong_name = tool_request("get_schedule", json!({}), 2);
    wrong_name
        .headers_mut()
        .insert("mcp-name", "search_items".parse().unwrap());
    let (status, _, body) = send(&fixture.app, wrong_name).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32020);

    let wrong_body_version = base_builder()
        .header("mcp-protocol-version", CURRENT_VERSION)
        .header("mcp-method", "tools/list")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": LEGACY_VERSION,
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            })
            .to_string(),
        ))
        .unwrap();
    let (status, _, body) = send(&fixture.app, wrong_body_version).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32020);

    let mut unsupported = modern_request("tools/list", json!(4), json!({}), None);
    unsupported
        .headers_mut()
        .insert("mcp-protocol-version", "2099-01-01".parse().unwrap());
    let (status, _, body) = send(&fixture.app, unsupported).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32022);
    assert_eq!(
        body["error"]["data"]["supportedVersions"],
        json!([CURRENT_VERSION, LEGACY_VERSION])
    );
}

#[tokio::test]
async fn tools_list_is_deterministic_schema_complete_and_scope_filtered() {
    let fixture = fixture();
    let request = modern_request("tools/list", json!(1), json!({}), None);
    let (_, _, body) = send(&fixture.app, request).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "get_schedule",
            "search_items",
            "explain_placement",
            "get_conflicts",
            "simulate_plan",
            "submit_proposal"
        ]
    );
    for tool in tools {
        assert_eq!(
            tool["inputSchema"]["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["outputSchema"].is_object());
    }
    let submit = tools.last().unwrap();
    assert_eq!(
        submit["inputSchema"]["properties"]["idempotency_key"]["x-mcp-header"],
        "Idempotency-Key"
    );
    assert_eq!(submit["annotations"]["destructiveHint"], false);
    assert_eq!(submit["annotations"]["idempotentHint"], true);

    let scoped = fixture_with_authenticator(Arc::new(ScopedAuthenticator {
        token: TOKEN.to_owned(),
        scopes: vec![Scope::ScheduleRead],
        audience: PrincipalAudience::Legacy,
        allowed_origins: Vec::new(),
    }));
    let (_, _, body) = send(
        &scoped.app,
        modern_request("tools/list", json!(2), json!({}), None),
    )
    .await;
    let names: Vec<_> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "get_schedule",
            "search_items",
            "explain_placement",
            "get_conflicts"
        ]
    );
    let (_, _, hidden) = send(
        &scoped.app,
        tool_request(
            "simulate_plan",
            json!({
                "base_revision": "revision-7",
                "operations": [{ "kind": "create_item", "parameters": {} }]
            }),
            3,
        ),
    )
    .await;
    assert_eq!(hidden["error"]["code"], -32602);
}

#[tokio::test]
async fn read_tools_return_grounded_data_without_leaking_sensitive_canaries() {
    let fixture = fixture();
    let schedule = send(
        &fixture.app,
        tool_request(
            "get_schedule",
            json!({
                "start": "2026-08-29T08:00:00Z",
                "end": "2026-08-29T12:00:00Z",
                "detail": "full"
            }),
            1,
        ),
    )
    .await
    .2;
    let serialized = schedule.to_string();
    assert!(!serialized.contains(PRIVATE_CANARY));
    assert_eq!(schedule["result"]["structuredContent"]["redacted_count"], 1);
    assert_eq!(
        schedule["result"]["structuredContent"]["blocks"][0]["title"],
        "Write weekly plan"
    );
    assert!(schedule["result"]["structuredContent"]["blocks"][1]["id"].is_null());

    let search = send(
        &fixture.app,
        tool_request("search_items", json!({ "limit": 100 }), 2),
    )
    .await
    .2;
    assert!(!search.to_string().contains(PRIVATE_CANARY));
    assert_eq!(
        search["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let explanation = send(
        &fixture.app,
        tool_request(
            "explain_placement",
            json!({ "block_id": "block-public" }),
            3,
        ),
    )
    .await
    .2;
    assert_eq!(
        explanation["result"]["structuredContent"]["reasons"][0]["code"],
        "hard_deadline"
    );

    let conflicts = send(
        &fixture.app,
        tool_request(
            "get_conflicts",
            json!({
                "start": "2026-08-29T08:00:00Z",
                "end": "2026-08-29T12:00:00Z"
            }),
            4,
        ),
    )
    .await
    .2;
    assert!(!conflicts.to_string().contains(PRIVATE_CANARY));
    assert_eq!(
        conflicts["result"]["structuredContent"]["redacted_count"],
        1
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One boundary matrix keeps read and proposal precision aligned.
async fn timestamp_boundaries_and_proposal_expiry_require_microsecond_precision() {
    let fixture = fixture();
    let accepted = send(
        &fixture.app,
        tool_request(
            "get_schedule",
            json!({
                "start": "2026-08-29T08:00:00.000001Z",
                "end": "2026-08-29T12:00:00.000001Z",
                "detail": "summary"
            }),
            1,
        ),
    )
    .await
    .2;
    assert_eq!(accepted["result"]["isError"], false);

    for (name, arguments) in [
        (
            "get_schedule",
            json!({
                "start": "2026-08-29T08:00:00.000000001Z",
                "end": "2026-08-29T12:00:00Z",
                "detail": "summary"
            }),
        ),
        (
            "search_items",
            json!({"start": "2026-08-29T08:00:00.000000001Z", "limit": 20}),
        ),
        (
            "search_items",
            json!({"end": "2026-08-29T12:00:00.000000001Z", "limit": 20}),
        ),
        (
            "get_conflicts",
            json!({
                "start": "2026-08-29T08:00:00Z",
                "end": "2026-08-29T12:00:00.000000001Z"
            }),
        ),
    ] {
        let rejected = send(&fixture.app, tool_request(name, arguments, 2)).await.2;
        assert_eq!(rejected["result"]["isError"], true, "{name}");
        assert_eq!(
            rejected["result"]["structuredContent"]["code"], "invalid_arguments",
            "{name}"
        );
    }

    let operation = json!({
        "kind": "move_block",
        "target_id": "block-public",
        "parameters": { "start": "2026-08-29T12:00:00Z" }
    });
    let simulation = send(
        &fixture.app,
        tool_request(
            "simulate_plan",
            json!({
                "base_revision": "revision-7",
                "operations": [operation.clone()]
            }),
            3,
        ),
    )
    .await
    .2;
    let token = simulation["result"]["structuredContent"]["simulation_token"]
        .as_str()
        .unwrap();
    let key = "timestamp-expiration-precision";
    let proposal = json!({
        "idempotency_key": key,
        "title": "Precision test proposal",
        "explanation": "Synthetic timestamp precision boundary.",
        "source_conversation_label": "Synthetic MCP test",
        "base_revision": "revision-7",
        "simulation_token": token,
        "operations": [operation],
        "expires_at": "2026-08-30T08:00:00.000000001Z"
    });
    let rejected = send(
        &fixture.app,
        proposal_request(proposal.clone(), 4, Some(key)),
    )
    .await
    .2;
    assert_eq!(rejected["result"]["isError"], true);
    assert_eq!(
        rejected["result"]["structuredContent"]["code"],
        "invalid_expiration"
    );
    assert!(
        fixture
            .proposals
            .list(ProposalQuery {
                limit: 10,
                ..ProposalQuery::default()
            })
            .await
            .unwrap()
            .is_empty()
    );

    let mut accepted_proposal = proposal;
    accepted_proposal["expires_at"] = json!("2026-08-30T08:00:00.000001Z");
    let accepted = send(
        &fixture.app,
        proposal_request(accepted_proposal, 5, Some(key)),
    )
    .await
    .2;
    assert_eq!(accepted["result"]["isError"], false);
}

#[tokio::test]
async fn simulation_is_deterministic_honest_and_never_mutates_the_stored_schedule() {
    let fixture = fixture();
    let original = fixture.schedule.stored_schedule().clone();
    let arguments = json!({
        "base_revision": "revision-7",
        "operations": [{
            "kind": "move_block",
            "target_id": "block-public",
            "parameters": { "start": "2026-08-29T12:00:00Z" }
        }],
        "assumptions": ["Keep the one-hour duration"]
    });

    let first = send(
        &fixture.app,
        tool_request("simulate_plan", arguments.clone(), 1),
    )
    .await
    .2;
    let second = send(&fixture.app, tool_request("simulate_plan", arguments, 2))
        .await
        .2;

    assert_eq!(first["result"]["isError"], false);
    assert_eq!(
        first["result"]["structuredContent"]["simulation_token"],
        second["result"]["structuredContent"]["simulation_token"]
    );
    assert_eq!(
        first["result"]["structuredContent"]["moved_blocks"],
        json!([])
    );
    assert_eq!(
        first["result"]["structuredContent"]["warnings"][0]["code"],
        "not_modeled"
    );
    assert_eq!(fixture.schedule.stored_schedule(), &original);
    assert_eq!(
        fixture.schedule.stored_schedule().blocks[0].start,
        "2026-08-29T09:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[tokio::test]
async fn submit_proposal_is_idempotent_consumes_simulation_and_only_writes_inbox() {
    let fixture = fixture();
    let operation = json!({
        "kind": "move_block",
        "target_id": "block-public",
        "parameters": { "start": "2026-08-29T12:00:00Z" }
    });
    let simulation = send(
        &fixture.app,
        tool_request(
            "simulate_plan",
            json!({
                "base_revision": "revision-7",
                "operations": [operation.clone()]
            }),
            1,
        ),
    )
    .await
    .2;
    let token = simulation["result"]["structuredContent"]["simulation_token"]
        .as_str()
        .unwrap();
    let key = "chatgpt-conversation-42-v1";
    let mut arguments = json!({
        "idempotency_key": key,
        "title": "Move weekly planning block",
        "explanation": "The user asked to move this block after lunch.",
        "source_conversation_label": "ChatGPT planning chat",
        "base_revision": "revision-7",
        "simulation_token": token,
        "operations": [operation]
    });

    let first = send(
        &fixture.app,
        proposal_request(arguments.clone(), 2, Some(key)),
    )
    .await
    .2;
    assert_eq!(first["result"]["isError"], false);
    assert_eq!(
        first["result"]["structuredContent"]["canonical_state_mutated"],
        false
    );
    assert_eq!(
        first["result"]["structuredContent"]["review_required"],
        true
    );
    let proposal_id = first["result"]["structuredContent"]["proposal_id"].clone();

    let replay = send(
        &fixture.app,
        proposal_request(arguments.clone(), 3, Some(key)),
    )
    .await
    .2;
    assert_eq!(replay["result"]["structuredContent"]["duplicate"], true);
    assert_eq!(
        replay["result"]["structuredContent"]["proposal_id"],
        proposal_id
    );

    let mut conflicting_retry = arguments.clone();
    conflicting_retry["title"] = json!("Different proposal under a reused key");
    let conflict = send(
        &fixture.app,
        proposal_request(conflicting_retry, 4, Some(key)),
    )
    .await
    .2;
    assert_eq!(conflict["result"]["isError"], true);
    assert_eq!(
        conflict["result"]["structuredContent"]["code"],
        "idempotency_conflict"
    );

    let proposals = fixture
        .proposals
        .list(ProposalQuery {
            limit: 10,
            ..ProposalQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].source, ProposalSource::ExternalMcp);
    assert_eq!(proposals[0].status, ProposalStatus::Pending);
    assert_eq!(proposals[0].payload["safety"]["proposal_only"], true);
    assert_eq!(
        fixture.schedule.stored_schedule().blocks[0].start,
        "2026-08-29T09:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );

    arguments["idempotency_key"] = json!("different-key-for-consumed-token");
    let consumed = send(
        &fixture.app,
        proposal_request(arguments, 5, Some("different-key-for-consumed-token")),
    )
    .await
    .2;
    assert_eq!(consumed["result"]["isError"], true);
    assert_eq!(consumed["result"]["structuredContent"]["code"], "not_found");
}

#[tokio::test]
async fn simulation_token_is_bound_to_exact_operations_and_mismatch_does_not_consume_it() {
    let fixture = fixture();
    let simulated_operation = json!({
        "kind": "move_block",
        "target_id": "block-public",
        "parameters": { "start": "2026-08-29T12:00:00Z" }
    });
    let simulation = send(
        &fixture.app,
        tool_request(
            "simulate_plan",
            json!({
                "base_revision": "revision-7",
                "operations": [simulated_operation.clone()]
            }),
            1,
        ),
    )
    .await
    .2;
    let token = simulation["result"]["structuredContent"]["simulation_token"]
        .as_str()
        .unwrap();
    let key = "simulation-binding-test";
    let proposal = |operation: Value| {
        json!({
            "idempotency_key": key,
            "title": "Move planning block",
            "explanation": "A proposal grounded in an explicit what-if simulation.",
            "source_conversation_label": "binding test",
            "base_revision": "revision-7",
            "simulation_token": token,
            "operations": [operation]
        })
    };

    let mismatched_operation = json!({
        "kind": "move_block",
        "target_id": "block-public",
        "parameters": { "start": "2026-08-29T13:00:00Z" }
    });
    let mismatch = send(
        &fixture.app,
        proposal_request(proposal(mismatched_operation), 2, Some(key)),
    )
    .await
    .2;
    assert_eq!(mismatch["result"]["isError"], true);
    assert_eq!(
        mismatch["result"]["structuredContent"]["code"],
        "invalid_arguments"
    );

    let matched = send(
        &fixture.app,
        proposal_request(proposal(simulated_operation), 3, Some(key)),
    )
    .await
    .2;
    assert_eq!(matched["result"]["isError"], false);
    assert_eq!(
        matched["result"]["structuredContent"]["review_required"],
        true
    );
}

#[tokio::test]
async fn submit_requires_mirrored_idempotency_header_and_stale_simulation_is_actionable() {
    let fixture = fixture();
    let arguments = json!({
        "idempotency_key": "missing-mirrored-header",
        "title": "Draft",
        "explanation": "Draft only",
        "source_conversation_label": "test",
        "base_revision": "revision-7",
        "operations": [{ "kind": "create_item", "parameters": {} }]
    });
    let (status, _, body) = send(&fixture.app, proposal_request(arguments, 1, None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32020);

    let stale = send(
        &fixture.app,
        tool_request(
            "simulate_plan",
            json!({
                "base_revision": "revision-stale",
                "operations": [{ "kind": "create_item", "parameters": {} }]
            }),
            2,
        ),
    )
    .await
    .2;
    assert_eq!(stale["result"]["isError"], true);
    assert_eq!(
        stale["result"]["structuredContent"]["code"],
        "revision_conflict"
    );
    assert_eq!(
        stale["result"]["structuredContent"]["details"]["current_revision"],
        "revision-7"
    );
}

#[tokio::test]
async fn legacy_simulation_requires_a_fresh_publication_on_the_mcp_wire() {
    let app = republish_required_fixture();
    let (status, _, body) = send(
        &app,
        tool_request(
            "simulate_plan",
            json!({
                "base_revision": "legacy-revision",
                "operations": [{ "kind": "create_item", "parameters": {} }]
            }),
            1,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "republish_required"
    );
    assert!(
        body["result"]["structuredContent"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("publish a fresh schedule"))
    );
}

#[derive(Clone)]
struct ScopedAuthenticator {
    token: String,
    scopes: Vec<Scope>,
    audience: PrincipalAudience,
    allowed_origins: Vec<String>,
}

#[async_trait]
impl Authenticator for ScopedAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        if token != self.token {
            return Err(AuthenticationError::InvalidCredentials);
        }
        Ok(Principal {
            subject: "scoped-test-client".to_owned(),
            scopes: self.scopes.clone(),
            audience: self.audience,
            workspace_id: None,
            user_id: None,
            credential_id: None,
            allowed_origins: self.allowed_origins.clone(),
        })
    }
}
