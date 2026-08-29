use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dayweave_google::{
    GoogleError,
    oauth::{AuthorizationOptions, OAuthClient, OAuthConfig},
};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path},
};

struct TlsMockServer {
    mock: MockServer,
    origin: String,
    root_certificate: reqwest::Certificate,
    accept_loop: JoinHandle<()>,
}

impl TlsMockServer {
    async fn start() -> Self {
        let mock = MockServer::start().await;
        let root_signing_key = KeyPair::generate().expect("ephemeral root key");
        let mut root_params = CertificateParams::default();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, root_signing_key)
            .expect("ephemeral root certificate");
        let server_signing_key = KeyPair::generate().expect("ephemeral server key");
        let mut server_params =
            CertificateParams::new(["127.0.0.1".to_owned()]).expect("server identity");
        server_params.is_ca = IsCa::ExplicitNoCa;
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_certificate = server_params
            .signed_by(&server_signing_key, &root)
            .expect("ephemeral server certificate");
        let root_certificate =
            reqwest::Certificate::from_der(root.der()).expect("ephemeral trusted root");
        let private_key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_signing_key.serialize_der()));
        let server_config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("safe TLS protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![server_certificate.der().clone()], private_key)
        .expect("ephemeral TLS server config");
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral TLS listener");
        let address = listener.local_addr().expect("TLS listener address");
        let backend_address = *mock.address();
        let accept_loop = tokio::spawn(async move {
            loop {
                let Ok((client_stream, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut client_stream) = acceptor.accept(client_stream).await else {
                        return;
                    };
                    let Ok(mut backend_stream) = TcpStream::connect(backend_address).await else {
                        return;
                    };
                    let _ = copy_bidirectional(&mut client_stream, &mut backend_stream).await;
                });
            }
        });

        Self {
            mock,
            origin: format!("https://{address}"),
            root_certificate,
            accept_loop,
        }
    }
}

impl Drop for TlsMockServer {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}

fn config(server: &TlsMockServer) -> OAuthConfig {
    OAuthConfig::with_endpoints(
        "client-id",
        "client-secret",
        Url::parse("https://dayweave.example/oauth/google/callback").expect("redirect URL"),
        &format!("{}/authorize", server.origin),
        &format!("{}/token", server.origin),
        &format!("{}/revoke", server.origin),
    )
    .expect("test OAuth config")
}

fn client(server: &TlsMockServer) -> OAuthClient {
    OAuthClient::with_additional_root_certificate(config(server), server.root_certificate.clone())
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

#[test]
fn oauth_config_requires_https_and_rejects_embedded_endpoint_metadata() {
    let safe_redirect =
        Url::parse("https://dayweave.example/oauth/google/callback").expect("redirect URL");
    let safe_authorization = "https://accounts.google.com/o/oauth2/v2/auth";
    let safe_token = "https://oauth2.googleapis.com/token";
    let safe_revocation = "https://oauth2.googleapis.com/revoke";
    let unsafe_configurations = [
        (
            Url::parse("http://127.0.0.1/callback").expect("cleartext redirect"),
            safe_authorization,
            safe_token,
            safe_revocation,
        ),
        (
            safe_redirect.clone(),
            "http://127.0.0.1/authorize",
            safe_token,
            safe_revocation,
        ),
        (
            safe_redirect.clone(),
            safe_authorization,
            "http://127.0.0.1/token",
            safe_revocation,
        ),
        (
            safe_redirect.clone(),
            safe_authorization,
            safe_token,
            "http://127.0.0.1/revoke",
        ),
        (
            safe_redirect.clone(),
            safe_authorization,
            "https://user:password@oauth2.googleapis.com/token",
            safe_revocation,
        ),
        (
            safe_redirect.clone(),
            safe_authorization,
            "https://oauth2.googleapis.com/token?credential=value",
            safe_revocation,
        ),
        (
            safe_redirect,
            safe_authorization,
            "https://oauth2.googleapis.com/token#fragment",
            safe_revocation,
        ),
    ];

    for (redirect, authorization_endpoint, token_endpoint, revocation_endpoint) in
        unsafe_configurations
    {
        let error = OAuthConfig::with_endpoints(
            "client-id",
            "client-secret",
            redirect,
            authorization_endpoint,
            token_endpoint,
            revocation_endpoint,
        )
        .expect_err("unsafe endpoint must be rejected");
        assert!(matches!(error, GoogleError::InvalidOAuthRequest(_)));
        assert!(!error.to_string().contains("credential=value"));
    }
}

#[tokio::test]
async fn ephemeral_tls_root_must_be_explicitly_trusted() {
    let server = TlsMockServer::start().await;
    let oauth = OAuthClient::new(config(&server)).expect("OAuth client without test root");
    let credential = SecretString::from("synthetic-refresh-credential");

    let error = oauth
        .refresh(&credential)
        .await
        .expect_err("untrusted ephemeral certificate must fail closed");

    assert!(matches!(error, GoogleError::Transport(_)));
    assert!(!error.to_string().contains(credential.expose_secret()));
    assert!(
        server
            .mock
            .received_requests()
            .await
            .expect("request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn authorization_url_uses_offline_incremental_pkce_flow() {
    let server = TlsMockServer::start().await;
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
async fn token_transport_never_follows_provider_redirects() {
    let server = TlsMockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(302)
                .append_header("Location", "http://127.0.0.1/cleartext-target"),
        )
        .mount(&server.mock)
        .await;
    let oauth = client(&server);
    let credential = SecretString::from("synthetic-refresh-credential");

    let error = oauth
        .refresh(&credential)
        .await
        .expect_err("OAuth redirect must not be followed");

    assert!(matches!(error, GoogleError::Api { status: 302 }));
    assert!(!error.to_string().contains(credential.expose_secret()));
    assert_eq!(
        server
            .mock
            .received_requests()
            .await
            .expect("request journal")
            .len(),
        1
    );
}

#[tokio::test]
async fn mismatched_state_fails_before_token_request() {
    let server = TlsMockServer::start().await;
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
            .mock
            .received_requests()
            .await
            .expect("request journal")
            .is_empty()
    );
}

#[tokio::test]
async fn exchanges_code_with_verifier_and_returns_redacted_tokens() {
    let server = TlsMockServer::start().await;
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
        .mount(&server.mock)
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
    let server = TlsMockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "replacement-access",
            "expires_in": 1800,
            "token_type": "Bearer"
        })))
        .mount(&server.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=refresh-secret"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server.mock)
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
    let server = TlsMockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "authorization code contains sensitive context"
        })))
        .mount(&server.mock)
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
