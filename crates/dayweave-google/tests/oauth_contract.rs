use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dayweave_google::{
    GoogleError,
    oauth::{AuthorizationOptions, OAuthClient, OAuthConfig},
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};

fn client(server: &MockServer) -> OAuthClient {
    OAuthClient::new(
        OAuthConfig::with_endpoints(
            "client-id",
            "client-secret",
            Url::parse("https://dayweave.example/oauth/google/callback").expect("redirect URL"),
            &format!("{}/authorize", server.uri()),
            &format!("{}/token", server.uri()),
            &format!("{}/revoke", server.uri()),
        )
        .expect("test OAuth config"),
    )
    .expect("OAuth client")
}

fn options() -> AuthorizationOptions {
    AuthorizationOptions {
        scopes: BTreeSet::from([
            "https://www.googleapis.com/auth/calendar".to_owned(),
            "https://www.googleapis.com/auth/tasks".to_owned(),
        ]),
        force_consent: true,
        login_hint: Some("owner@example.test".to_owned()),
    }
}

#[tokio::test]
async fn authorization_url_uses_offline_incremental_pkce_flow() {
    let server = MockServer::start().await;
    let session = client(&server)
        .begin_authorization(&options())
        .expect("authorization session");
    let query: BTreeMap<_, _> = session
        .authorization_url
        .query_pairs()
        .into_owned()
        .collect();

    assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(
        query.get("access_type").map(String::as_str),
        Some("offline")
    );
    assert_eq!(
        query.get("include_granted_scopes").map(String::as_str),
        Some("true")
    );
    assert_eq!(query.get("prompt").map(String::as_str), Some("consent"));
    assert_eq!(
        query.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        query.get("state").map(String::as_str),
        Some(session.state().expose_secret())
    );
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(
        session.code_verifier().expose_secret().as_bytes(),
    ));
    assert_eq!(query.get("code_challenge"), Some(&expected_challenge));
    assert!(query["scope"].contains("calendar"));
    assert!(query["scope"].contains("tasks"));

    let debug = format!("{session:?}");
    assert!(!debug.contains(session.state().expose_secret()));
    assert!(!debug.contains(session.code_verifier().expose_secret()));
}

#[tokio::test]
async fn mismatched_state_fails_before_token_request() {
    let server = MockServer::start().await;
    let oauth = client(&server);
    let session = oauth
        .begin_authorization(&options())
        .expect("authorization session");

    let error = oauth
        .exchange_code(
            &session,
            "attacker-state",
            &SecretString::from("authorization-code"),
        )
        .await
        .expect_err("state mismatch is rejected");

    assert!(matches!(error, GoogleError::OAuthStateMismatch));
    assert!(
        server
            .received_requests()
            .await
            .expect("request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn exchanges_code_with_verifier_and_returns_redacted_tokens() {
    let server = MockServer::start().await;
    let oauth = client(&server);
    let session = oauth
        .begin_authorization(&options())
        .expect("authorization session");
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=authorization-code"))
        .and(body_string_contains("client_id=client-id"))
        .and(body_string_contains("client_secret=client-secret"))
        .and(body_string_contains(format!(
            "code_verifier={}",
            session.code_verifier().expose_secret()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-secret",
            "refresh_token": "refresh-secret",
            "expires_in": 3599,
            "scope": "scope-a scope-b",
            "token_type": "Bearer",
            "id_token": "identity-secret"
        })))
        .mount(&server)
        .await;

    let tokens = oauth
        .exchange_code(
            &session,
            session.state().expose_secret(),
            &SecretString::from("authorization-code"),
        )
        .await
        .expect("code exchange");

    assert_eq!(tokens.access_token.expose_secret(), "access-secret");
    assert_eq!(tokens.expires_in_seconds, 3599);
    assert_eq!(
        tokens.granted_scopes,
        BTreeSet::from(["scope-a".to_owned(), "scope-b".to_owned()])
    );
    let debug = format!("{tokens:?}");
    assert!(!debug.contains("access-secret"));
    assert!(!debug.contains("refresh-secret"));
    assert!(!debug.contains("identity-secret"));
}

#[tokio::test]
async fn refresh_and_revoke_use_confidential_backchannel() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "replacement-access",
            "expires_in": 1800,
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=refresh-secret"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let oauth = client(&server);
    let refresh = SecretString::from("refresh-secret");

    let tokens = oauth.refresh(&refresh).await.expect("refresh succeeds");
    assert!(tokens.refresh_token.is_none());
    assert_eq!(tokens.access_token.expose_secret(), "replacement-access");
    oauth.revoke(&refresh).await.expect("revocation succeeds");
}

#[tokio::test]
async fn provider_oauth_error_exposes_code_without_description_or_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "authorization code contains sensitive context"
        })))
        .mount(&server)
        .await;
    let error = client(&server)
        .refresh(&SecretString::from("expired-refresh-secret"))
        .await
        .expect_err("provider rejection is surfaced");

    assert!(matches!(
        error,
        GoogleError::OAuthRejected { ref code } if code == "invalid_grant"
    ));
    let display = error.to_string();
    assert!(!display.contains("sensitive context"));
    assert!(!display.contains("expired-refresh-secret"));
}
