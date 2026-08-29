use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use aws_lc_rs::{
    rand::SystemRandom,
    rsa::KeySize,
    signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair},
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dayweave_api::{
    AppState,
    auth::{AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope},
    config::McpOAuthConfig,
    http::router,
    mcp_oauth::{JwksSource, McpOAuthError, McpOAuthVerifier},
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
    scheduling::{UnavailableScheduleQueryPort, UnavailableSimulationPort},
};
use http_body_util::BodyExt as _;
use jsonwebtoken::{Algorithm, Header, get_current_timestamp};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use url::Url;
use uuid::Uuid;

const NATIVE_TOKEN: &str = "dw_mc1_native-test-token";
const CURRENT_VERSION: &str = "2026-07-28";

struct RuntimeKey {
    private: RsaKeyPair,
}

impl RuntimeKey {
    fn generate() -> Self {
        Self {
            private: RsaKeyPair::generate(KeySize::Rsa2048).expect("runtime-only RSA key"),
        }
    }

    fn jwks(&self) -> Vec<u8> {
        let public = self.private.public_key();
        serde_json::to_vec(&json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": "route-key",
                "n": URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
                "e": URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
            }]
        }))
        .unwrap()
    }

    fn token(&self, scopes: &str) -> String {
        let now = get_current_timestamp();
        let claims = json!({
            "iss": "https://tenant.eu.auth0.com/",
            "sub": "auth0|personal-owner",
            "aud": "https://api.example.test/mcp",
            "exp": now + 300,
            "iat": now - 1,
            "nbf": now - 1,
            "scope": scopes,
            "client_id": "https://chatgpt.com/oauth/client.json",
        });
        let mut header = Header::new(Algorithm::RS256);
        header.typ = Some("at+jwt".to_owned());
        header.kid = Some("route-key".to_owned());
        let encoded_header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&header).expect("serializable synthetic access-token header"),
        );
        let encoded_claims = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&claims).expect("serializable synthetic access-token claims"),
        );
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        let mut signature = vec![0_u8; self.private.public_modulus_len()];
        self.private
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("signed synthetic access token");
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

struct CountingJwks {
    body: Vec<u8>,
    calls: AtomicUsize,
}

#[async_trait]
impl JwksSource for CountingJwks {
    async fn fetch(&self) -> Result<Vec<u8>, McpOAuthError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.body.clone())
    }
}

struct NativeAuthenticator {
    calls: AtomicUsize,
}

#[async_trait]
impl Authenticator for NativeAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if token != NATIVE_TOKEN {
            return Err(AuthenticationError::InvalidCredentials);
        }
        Ok(Principal {
            subject: "mcp-client:native-test".to_owned(),
            scopes: vec![Scope::ScheduleRead],
            audience: PrincipalAudience::Mcp,
            workspace_id: Some(Uuid::from_u128(2)),
            user_id: Some(Uuid::from_u128(1)),
            credential_id: Some(Uuid::from_u128(3)),
            allowed_origins: vec!["https://chatgpt.com".to_owned()],
        })
    }
}

fn oauth_config() -> McpOAuthConfig {
    McpOAuthConfig {
        resource: Url::parse("https://api.example.test/mcp").unwrap(),
        issuer: Url::parse("https://tenant.eu.auth0.com/").unwrap(),
        jwks_uri: Url::parse("https://tenant.eu.auth0.com/.well-known/jwks.json").unwrap(),
        resource_metadata_uri: Url::parse(
            "https://api.example.test/.well-known/oauth-protected-resource/mcp",
        )
        .unwrap(),
        owner_subject: "auth0|personal-owner".to_owned(),
        allowed_client_ids: Arc::new(vec!["https://chatgpt.com/oauth/client.json".to_owned()]),
        allowed_origins: Arc::new(vec!["https://chatgpt.com".to_owned()]),
        user_id: Uuid::from_u128(1),
        workspace_id: Uuid::from_u128(2),
    }
}

fn proposal_service() -> Arc<ProposalService> {
    let repository: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    Arc::new(ProposalService::new(
        repository,
        Arc::new(SystemClock),
        Duration::from_hours(168),
    ))
}

fn fixture() -> (
    Router,
    Arc<NativeAuthenticator>,
    Arc<CountingJwks>,
    RuntimeKey,
) {
    let key = RuntimeKey::generate();
    let source = Arc::new(CountingJwks {
        body: key.jwks(),
        calls: AtomicUsize::new(0),
    });
    let native = Arc::new(NativeAuthenticator {
        calls: AtomicUsize::new(0),
    });
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let state = AppState::new(proposal_service(), native.clone(), readiness)
        .with_mcp_ports(
            Arc::new(UnavailableScheduleQueryPort),
            Arc::new(UnavailableSimulationPort),
            Arc::new(vec!["https://chatgpt.com".to_owned()]),
        )
        .with_mcp_oauth(Arc::new(McpOAuthVerifier::with_source(
            oauth_config(),
            source.clone(),
        )));
    (router(state), native, source, key)
}

fn mcp_request(token: Option<&str>, method: &str, id: i64, params: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", CURRENT_VERSION)
        .header("mcp-method", method)
        .header("x-request-id", "oauth-route-test");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if method == "tools/call"
        && let Some(name) = params.get("name").and_then(Value::as_str)
    {
        builder = builder.header("mcp-name", name);
    }
    let mut params = params;
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": CURRENT_VERSION,
        "io.modelcontextprotocol/clientInfo": { "name": "oauth-test", "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": {},
    });
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

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
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
#[allow(clippy::too_many_lines)] // One end-to-end contract keeps shared call counters meaningful.
async fn metadata_challenges_dispatch_catalog_and_step_up_are_isolated() {
    let (app, native, source, key) = fixture();

    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let (status, headers, body) = send(
            &app,
            Request::builder()
                .uri(path)
                .header(header::HOST, "attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=300");
        assert_eq!(body["resource"], "https://api.example.test/mcp");
        assert_eq!(
            body["authorization_servers"],
            json!(["https://tenant.eu.auth0.com/"])
        );
        assert_eq!(
            body["scopes_supported"],
            json!(["schedule:read", "schedule:simulate", "suggestions:submit"])
        );
    }

    let mut missing_token = mcp_request(None, "tools/list", 1, json!({}));
    missing_token
        .headers_mut()
        .insert(header::HOST, "attacker.example".parse().unwrap());
    let (status, headers, _) = send(&app, missing_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers[header::WWW_AUTHENTICATE],
        "Bearer resource_metadata=\"https://api.example.test/.well-known/oauth-protected-resource/mcp\", scope=\"schedule:read schedule:simulate suggestions:submit\""
    );

    let before_native = native.calls.load(Ordering::SeqCst);
    let before_jwks = source.calls.load(Ordering::SeqCst);
    let (status, headers, _) = send(
        &app,
        mcp_request(Some("dw_mc1_invalid"), "tools/list", 2, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .starts_with("Bearer realm=\"dayweave-native-mcp\"")
    );
    assert_eq!(native.calls.load(Ordering::SeqCst), before_native + 1);
    assert_eq!(source.calls.load(Ordering::SeqCst), before_jwks);

    for (id, token) in [
        (3, "opaque-invalid-token"),
        (30, "dw_ac1_not-an-mcp-token"),
        (31, "dw_rf1_not-an-mcp-token"),
        (32, "dw_da1_not-an-mcp-token"),
        (33, "dw_dr1_not-an-mcp-token"),
        (34, "dw_en1_not-an-mcp-token"),
        (35, "dw_generic-reserved-token"),
        (36, "eyJhbGciOiJSUzI1NiJ9.e30.invalid"),
    ] {
        let native_before = native.calls.load(Ordering::SeqCst);
        let jwks_before = source.calls.load(Ordering::SeqCst);
        let (status, headers, _) =
            send(&app, mcp_request(Some(token), "tools/list", id, json!({}))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token}");
        let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap();
        assert!(challenge.contains("resource_metadata="), "{token}");
        assert!(challenge.contains("error=\"invalid_token\""), "{token}");
        assert_eq!(
            native.calls.load(Ordering::SeqCst),
            native_before,
            "only dw_mc1_ may enter native MCP auth: {token}"
        );
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            jwks_before,
            "structurally invalid OAuth tokens fail before JWKS: {token}"
        );
    }
    assert_eq!(native.calls.load(Ordering::SeqCst), before_native + 1);
    assert_eq!(source.calls.load(Ordering::SeqCst), before_jwks);

    let token = key.token("schedule:read");
    let (_, _, body) = send(&app, mcp_request(Some(&token), "tools/list", 4, json!({}))).await;
    let tools = body["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 6, "OAuth catalog supports scope step-up");
    for tool in tools {
        let scope = match tool["name"].as_str().unwrap() {
            "get_schedule" | "search_items" | "explain_placement" | "get_conflicts" => {
                "schedule:read"
            }
            "simulate_plan" => "schedule:simulate",
            "submit_proposal" => "suggestions:submit",
            name => panic!("unexpected tool {name}"),
        };
        assert_eq!(
            tool["securitySchemes"],
            json!([{ "type": "oauth2", "scopes": [scope] }])
        );
    }
    assert_eq!(native.calls.load(Ordering::SeqCst), before_native + 1);
    assert_eq!(source.calls.load(Ordering::SeqCst), before_jwks + 1);

    let (_, _, body) = send(
        &app,
        mcp_request(
            Some(&token),
            "tools/call",
            5,
            json!({ "name": "simulate_plan", "arguments": null }),
        ),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "insufficient_scope"
    );
    assert_eq!(body["result"]["isError"], true);
    let challenge = body["result"]["_meta"]["mcp/www_authenticate"][0]
        .as_str()
        .unwrap();
    assert!(challenge.contains("scope=\"schedule:simulate\""));
    assert!(challenge.contains("error=\"insufficient_scope\""));
    assert_eq!(
        body["result"]["_meta"]["com.greengolddog.dayweave/requestId"], "oauth-route-test",
        "server metadata must merge without erasing the step-up challenge"
    );

    let (status, _, body) = send(
        &app,
        mcp_request(
            Some(&token),
            "tools/call",
            6,
            json!({ "name": "submit_proposal", "arguments": {} }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "authorization precedes mirror validation"
    );
    assert_eq!(
        body["result"]["structuredContent"]["code"],
        "insufficient_scope"
    );

    let jwks_before_native = source.calls.load(Ordering::SeqCst);
    let (_, _, native_catalog) = send(
        &app,
        mcp_request(Some(NATIVE_TOKEN), "tools/list", 7, json!({})),
    )
    .await;
    assert_eq!(
        native_catalog["result"]["tools"].as_array().unwrap().len(),
        4
    );
    assert!(
        native_catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool.get("securitySchemes").is_none())
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), jwks_before_native);

    let mut allowed_origin = mcp_request(Some(&token), "tools/list", 8, json!({}));
    allowed_origin
        .headers_mut()
        .insert(header::ORIGIN, "https://chatgpt.com".parse().unwrap());
    assert_eq!(send(&app, allowed_origin).await.0, StatusCode::OK);

    let mut denied_origin = mcp_request(Some(&token), "tools/list", 9, json!({}));
    denied_origin
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(send(&app, denied_origin).await.0, StatusCode::FORBIDDEN);

    let calls_before_rest = source.calls.load(Ordering::SeqCst);
    let native_before_rest = native.calls.load(Ordering::SeqCst);
    let (status, _, _) = send(
        &app,
        Request::builder()
            .uri("/v1/suggestions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(source.calls.load(Ordering::SeqCst), calls_before_rest);
    assert_eq!(native.calls.load(Ordering::SeqCst), native_before_rest + 1);
}

#[tokio::test]
async fn disabled_mode_exposes_no_metadata_surface() {
    let readiness = Readiness::default();
    let native: Arc<dyn Authenticator> = Arc::new(NativeAuthenticator {
        calls: AtomicUsize::new(0),
    });
    let app = router(AppState::new(proposal_service(), native, readiness));
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        assert_eq!(
            send(
                &app,
                Request::builder().uri(path).body(Body::empty()).unwrap(),
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
}
