//! Disabled-by-default OAuth 2.1 resource-server authentication for MCP.
//!
//! Auth0 is the authorization server. `DayWeave` accepts only pinned RS256 access
//! tokens for its exact MCP resource and never handles an OAuth client secret.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::{
    Json,
    extract::State,
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::{
    AppState,
    auth::{Principal, PrincipalAudience, Scope},
    config::McpOAuthConfig,
};

pub const SCOPE_SCHEDULE_READ: &str = "schedule:read";
pub const SCOPE_SCHEDULE_SIMULATE: &str = "schedule:simulate";
pub const SCOPE_SUGGESTIONS_SUBMIT: &str = "suggestions:submit";
pub const ALL_SCOPE_NAMES: [&str; 3] = [
    SCOPE_SCHEDULE_READ,
    SCOPE_SCHEDULE_SIMULATE,
    SCOPE_SUGGESTIONS_SUBMIT,
];

const MAX_TOKEN_LENGTH: usize = 16 * 1024;
const MAX_JWKS_BYTES: usize = 256 * 1024;
const MAX_JWKS_KEYS: usize = 32;
const MAX_KID_LENGTH: usize = 128;
const MIN_RSA_MODULUS_BYTES: usize = 256;
const MAX_RSA_MODULUS_BYTES: usize = 1024;
const MAX_RSA_EXPONENT_BYTES: usize = 8;
const MAX_X5C_CERTIFICATES: usize = 5;
const MAX_X5C_ENCODED_BYTES: usize = 16 * 1024;
const KEY_CACHE_TTL: Duration = Duration::from_mins(5);
const UNKNOWN_KID_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_STALE_KEY_AGE: Duration = Duration::from_hours(1);
const CLOCK_SKEW_SECONDS: u64 = 60;
const MAX_ACCESS_TOKEN_LIFETIME_SECONDS: u64 = 60 * 60;

/// Redacted OAuth verification failures. No token or upstream response is kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpOAuthError {
    InvalidToken,
    KeySetUnavailable,
    InvalidKeySet,
}

impl std::fmt::Display for McpOAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidToken => "invalid OAuth access token",
            Self::KeySetUnavailable => "OAuth verification keys are unavailable",
            Self::InvalidKeySet => "OAuth verification keys are invalid",
        })
    }
}

impl std::error::Error for McpOAuthError {}

/// Fetches the one JWKS endpoint pinned at verifier construction.
///
/// The trait intentionally receives no URL or token-controlled input, which
/// prevents authentication data from becoming an SSRF destination.
#[async_trait]
pub trait JwksSource: Send + Sync {
    async fn fetch(&self) -> Result<Vec<u8>, McpOAuthError>;
}

#[derive(Debug)]
struct HttpJwksSource {
    client: reqwest::Client,
    uri: Url,
}

impl HttpJwksSource {
    fn new(uri: Url) -> Result<Self, McpOAuthError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| McpOAuthError::KeySetUnavailable)?;
        Ok(Self { client, uri })
    }
}

#[async_trait]
impl JwksSource for HttpJwksSource {
    async fn fetch(&self) -> Result<Vec<u8>, McpOAuthError> {
        let mut response = self
            .client
            .get(self.uri.clone())
            .send()
            .await
            .map_err(|_| McpOAuthError::KeySetUnavailable)?;
        if response.status() != reqwest::StatusCode::OK
            || !has_single_json_content_type(response.headers())
            || response
                .content_length()
                .is_some_and(|length| length > MAX_JWKS_BYTES as u64)
        {
            return Err(McpOAuthError::KeySetUnavailable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| McpOAuthError::KeySetUnavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_JWKS_BYTES {
                return Err(McpOAuthError::KeySetUnavailable);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

fn has_single_json_content_type(headers: &reqwest::header::HeaderMap) -> bool {
    let mut values = headers.get_all(reqwest::header::CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<mime::Mime>().ok())
        .is_some_and(|media_type| {
            matches!(
                media_type.essence_str(),
                "application/json" | "application/jwk-set+json"
            )
        })
}

#[derive(Clone)]
struct KeySnapshot {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Instant,
}

#[derive(Default)]
struct RefreshState {
    last_unknown_kid_refresh: Option<Instant>,
}

/// Verifies Auth0 RFC 9068-style access tokens for only the published MCP URL.
pub struct McpOAuthVerifier {
    config: Arc<McpOAuthConfig>,
    source: Arc<dyn JwksSource>,
    cache: RwLock<Option<KeySnapshot>>,
    refresh: Mutex<RefreshState>,
}

impl std::fmt::Debug for McpOAuthVerifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthVerifier")
            .field("resource", &self.config.resource)
            .field("issuer", &self.config.issuer)
            .finish_non_exhaustive()
    }
}

impl McpOAuthVerifier {
    /// Constructs the production verifier without making an outbound request.
    ///
    /// # Errors
    ///
    /// Returns a redacted initialization error if the hardened HTTP client
    /// cannot be constructed.
    pub fn production(config: McpOAuthConfig) -> Result<Self, McpOAuthError> {
        let source = Arc::new(HttpJwksSource::new(config.jwks_uri.clone())?);
        Ok(Self::with_source(config, source))
    }

    /// Constructs a verifier with an injected bounded JWKS source.
    ///
    /// This is public so integration tests can remain synthetic and offline.
    #[must_use]
    pub fn with_source(config: McpOAuthConfig, source: Arc<dyn JwksSource>) -> Self {
        Self {
            config: Arc::new(config),
            source,
            cache: RwLock::new(None),
            refresh: Mutex::new(RefreshState::default()),
        }
    }

    #[must_use]
    pub fn config(&self) -> &McpOAuthConfig {
        &self.config
    }

    #[must_use]
    pub fn protected_resource_metadata(&self) -> Value {
        json!({
            "resource": self.config.resource.as_str(),
            "authorization_servers": [self.config.issuer.as_str()],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ALL_SCOPE_NAMES,
        })
    }

    #[must_use]
    pub fn challenge(&self, scope: Option<&str>, invalid_token: bool) -> String {
        let mut challenge = format!(
            "Bearer resource_metadata=\"{}\"",
            self.config.resource_metadata_uri
        );
        if let Some(scope) = scope {
            let _ = write!(challenge, ", scope=\"{scope}\"");
        }
        if invalid_token {
            challenge.push_str(
                ", error=\"invalid_token\", error_description=\"The access token is invalid or expired\"",
            );
        }
        challenge
    }

    #[must_use]
    pub fn insufficient_scope_challenge(&self, scope: &str) -> String {
        format!(
            "Bearer resource_metadata=\"{}\", scope=\"{scope}\", error=\"insufficient_scope\", error_description=\"The access token lacks the required scope\"",
            self.config.resource_metadata_uri
        )
    }

    /// Verifies the compact JWS and returns the exact configured owner.
    ///
    /// # Errors
    ///
    /// Returns a redacted invalid-token or verification-key error. The token
    /// and upstream response are never included in the error.
    pub async fn authenticate(&self, token: &str) -> Result<Principal, McpOAuthError> {
        validate_compact_jws(token)?;
        let header = decode_header(token).map_err(|_| McpOAuthError::InvalidToken)?;
        validate_header(&header)?;
        let kid = header.kid.as_deref().ok_or(McpOAuthError::InvalidToken)?;
        let key = self.key_for(kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = CLOCK_SKEW_SECONDS;
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        validation.set_audience(&[self.config.resource.as_str()]);
        validation.sub = Some(self.config.owner_subject.clone());
        let claims = decode::<AccessTokenClaims>(token, &key, &validation)
            .map_err(|_| McpOAuthError::InvalidToken)?
            .claims;

        let now = jsonwebtoken::get_current_timestamp();
        if claims.iss != self.config.issuer.as_str()
            || claims.sub != self.config.owner_subject
            || !claims.aud.exactly(self.config.resource.as_str())
            || claims.iat > now.saturating_add(CLOCK_SKEW_SECONDS)
            || claims.exp <= claims.iat
            || claims.exp.saturating_sub(claims.iat) > MAX_ACCESS_TOKEN_LIFETIME_SECONDS
            || claims.nbf.is_some_and(|nbf| nbf >= claims.exp)
            || !self
                .config
                .allowed_client_ids
                .iter()
                .any(|allowed| allowed == &claims.client_id)
        {
            return Err(McpOAuthError::InvalidToken);
        }
        let scopes = parse_scopes(&claims.scope);
        Ok(Principal {
            subject: claims.sub,
            scopes,
            audience: PrincipalAudience::McpOAuth,
            workspace_id: Some(self.config.workspace_id),
            user_id: Some(self.config.user_id),
            credential_id: None,
            allowed_origins: self.config.allowed_origins.as_ref().clone(),
        })
    }

    async fn key_for(&self, kid: &str) -> Result<DecodingKey, McpOAuthError> {
        let now = Instant::now();
        if let Some(key) = self.cached_key(kid, now, KEY_CACHE_TTL).await {
            return Ok(key);
        }

        // This mutex is the singleflight boundary. The cache lock is never held
        // across I/O, and every waiter rechecks after acquiring it.
        let mut refresh = self.refresh.lock().await;
        let now = Instant::now();
        if let Some(key) = self.cached_key(kid, now, KEY_CACHE_TTL).await {
            return Ok(key);
        }
        let snapshot = self.cache.read().await.clone();
        let known_stale = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.keys.get(kid).cloned())
            .zip(snapshot.as_ref().map(|snapshot| snapshot.fetched_at));
        let unknown = known_stale.is_none();
        let previously_loaded = snapshot.is_some();
        if unknown
            && previously_loaded
            && refresh.last_unknown_kid_refresh.is_some_and(|last| {
                now.saturating_duration_since(last) < UNKNOWN_KID_REFRESH_COOLDOWN
            })
        {
            return Err(McpOAuthError::InvalidToken);
        }
        match self.source.fetch().await.and_then(|body| parse_jwks(&body)) {
            Ok(keys) => {
                let key = keys.get(kid).cloned();
                *self.cache.write().await = Some(KeySnapshot {
                    keys,
                    fetched_at: Instant::now(),
                });
                if key.is_none() {
                    refresh.last_unknown_kid_refresh = Some(Instant::now());
                }
                key.ok_or(McpOAuthError::InvalidToken)
            }
            Err(error) => {
                if unknown {
                    refresh.last_unknown_kid_refresh = Some(Instant::now());
                }
                if let Some((key, fetched_at)) = known_stale
                    && now.saturating_duration_since(fetched_at) <= MAX_STALE_KEY_AGE
                {
                    return Ok(key);
                }
                Err(error)
            }
        }
    }

    async fn cached_key(
        &self,
        kid: &str,
        now: Instant,
        maximum_age: Duration,
    ) -> Option<DecodingKey> {
        let cache = self.cache.read().await;
        let snapshot = cache.as_ref()?;
        (now.saturating_duration_since(snapshot.fetched_at) <= maximum_age)
            .then(|| snapshot.keys.get(kid).cloned())
            .flatten()
    }
}

#[derive(Debug, Deserialize)]
struct AccessTokenClaims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: u64,
    iat: u64,
    nbf: Option<u64>,
    scope: String,
    client_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn exactly(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => value == expected,
            Self::Many(values) => values.len() == 1 && values[0] == expected,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JwksDocument {
    keys: Vec<JwkEntry>,
}

#[derive(Debug, Deserialize)]
struct JwkEntry {
    kty: String,
    #[serde(rename = "use")]
    key_use: String,
    alg: String,
    kid: String,
    n: String,
    e: String,
    #[serde(default)]
    x5c: Vec<String>,
    #[serde(default)]
    x5t: Option<String>,
    #[serde(default, rename = "x5t#S256")]
    x5t_s256: Option<String>,
    #[serde(default)]
    key_ops: Option<Vec<String>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

fn parse_jwks(body: &[u8]) -> Result<HashMap<String, DecodingKey>, McpOAuthError> {
    if body.is_empty() || body.len() > MAX_JWKS_BYTES {
        return Err(McpOAuthError::InvalidKeySet);
    }
    let document: JwksDocument =
        serde_json::from_slice(body).map_err(|_| McpOAuthError::InvalidKeySet)?;
    if document.keys.is_empty() || document.keys.len() > MAX_JWKS_KEYS {
        return Err(McpOAuthError::InvalidKeySet);
    }
    let mut keys = HashMap::with_capacity(document.keys.len());
    for jwk in document.keys {
        if jwk.kty != "RSA"
            || jwk.key_use != "sig"
            || jwk.alg != "RS256"
            || !valid_kid(&jwk.kid)
            || !jwk.extra.is_empty()
            || jwk.x5c.len() > MAX_X5C_CERTIFICATES
            || jwk.x5c.iter().any(|certificate| {
                certificate.is_empty()
                    || certificate.len() > MAX_X5C_ENCODED_BYTES
                    || STANDARD.decode(certificate.as_bytes()).is_err()
            })
            || jwk.x5t.as_ref().is_some_and(|thumbprint| {
                URL_SAFE_NO_PAD
                    .decode(thumbprint.as_bytes())
                    .map_or(true, |decoded| decoded.len() != 20)
            })
            || jwk.x5t_s256.as_ref().is_some_and(|thumbprint| {
                URL_SAFE_NO_PAD
                    .decode(thumbprint.as_bytes())
                    .map_or(true, |decoded| decoded.len() != 32)
            })
            || jwk
                .key_ops
                .as_ref()
                .is_some_and(|operations| operations.as_slice() != ["verify"])
        {
            return Err(McpOAuthError::InvalidKeySet);
        }
        // x5c/x5t may be published by Auth0 but are deliberately not trusted;
        // signature verification uses only the bounded RSA components.
        drop((jwk.x5c, jwk.x5t, jwk.x5t_s256));
        let modulus = URL_SAFE_NO_PAD
            .decode(jwk.n.as_bytes())
            .map_err(|_| McpOAuthError::InvalidKeySet)?;
        let exponent = URL_SAFE_NO_PAD
            .decode(jwk.e.as_bytes())
            .map_err(|_| McpOAuthError::InvalidKeySet)?;
        if !(MIN_RSA_MODULUS_BYTES..=MAX_RSA_MODULUS_BYTES).contains(&modulus.len())
            || exponent.is_empty()
            || exponent.len() > MAX_RSA_EXPONENT_BYTES
            || exponent.last().is_none_or(|byte| byte & 1 == 0)
            || exponent
                .iter()
                .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte))
                < 3
        {
            return Err(McpOAuthError::InvalidKeySet);
        }
        let key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
            .map_err(|_| McpOAuthError::InvalidKeySet)?;
        if keys.insert(jwk.kid, key).is_some() {
            return Err(McpOAuthError::InvalidKeySet);
        }
    }
    Ok(keys)
}

fn validate_compact_jws(token: &str) -> Result<(), McpOAuthError> {
    if token.is_empty()
        || token.len() > MAX_TOKEN_LENGTH
        || !token.is_ascii()
        || token.split('.').count() != 3
        || token.split('.').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err(McpOAuthError::InvalidToken);
    }
    Ok(())
}

fn validate_header(header: &jsonwebtoken::Header) -> Result<(), McpOAuthError> {
    if header.alg != Algorithm::RS256
        || header.typ.as_deref() != Some("at+jwt")
        || header.kid.as_deref().is_none_or(|kid| !valid_kid(kid))
        || header.cty.is_some()
        || header.jku.is_some()
        || header.jwk.is_some()
        || header.x5u.is_some()
        || header.x5c.is_some()
        || header.x5t.is_some()
        || header.x5t_s256.is_some()
        || header.crit.is_some()
        || header.enc.is_some()
        || header.zip.is_some()
        || header.url.is_some()
        || header.nonce.is_some()
        || !header.extras.inner().is_empty()
    {
        return Err(McpOAuthError::InvalidToken);
    }
    Ok(())
}

fn valid_kid(kid: &str) -> bool {
    !kid.is_empty()
        && kid.len() <= MAX_KID_LENGTH
        && kid.is_ascii()
        && !kid.chars().any(char::is_whitespace)
        && !kid.chars().any(char::is_control)
}

fn parse_scopes(raw: &str) -> Vec<Scope> {
    let mut scopes = Vec::new();
    for scope in raw.split_ascii_whitespace() {
        let mapped = match scope {
            SCOPE_SCHEDULE_READ => Some(Scope::ScheduleRead),
            SCOPE_SCHEDULE_SIMULATE => Some(Scope::ScheduleSimulate),
            SCOPE_SUGGESTIONS_SUBMIT => Some(Scope::SuggestionsSubmit),
            _ => None,
        };
        if let Some(scope) = mapped
            && !scopes.contains(&scope)
        {
            scopes.push(scope);
        }
    }
    scopes
}

#[must_use]
pub const fn scope_name(scope: Scope) -> Option<&'static str> {
    match scope {
        Scope::ScheduleRead => Some(SCOPE_SCHEDULE_READ),
        Scope::ScheduleSimulate => Some(SCOPE_SCHEDULE_SIMULATE),
        Scope::SuggestionsSubmit => Some(SCOPE_SUGGESTIONS_SUBMIT),
        _ => None,
    }
}

/// Serves the same RFC 9728 document at both supported well-known paths.
pub async fn protected_resource_metadata(State(state): State<AppState>) -> Response {
    let Some(verifier) = state.mcp_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = Json(verifier.protected_resource_metadata()).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fmt::Write as _,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use jsonwebtoken::{EncodingKey, Header, encode, get_current_timestamp};
    use rand::rngs::OsRng;
    use rsa::{RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts};
    use serde_json::{Map, json};
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
    };
    use uuid::Uuid;

    use super::*;

    async fn fetch_synthetic_http_response(headers: &[&str]) -> Result<Vec<u8>, McpOAuthError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind synthetic JWKS server");
        let address = listener.local_addr().expect("synthetic server address");
        let headers = headers.iter().map(ToString::to_string).collect::<Vec<_>>();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept JWKS request");
            let mut request = [0_u8; 4_096];
            let _ = stream.read(&mut request).await.expect("read JWKS request");
            let body = br#"{"keys":[]}"#;
            let header_block = headers.iter().fold(String::new(), |mut block, header| {
                let _ = write!(block, "{header}\r\n");
                block
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                header_block,
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write JWKS headers");
            stream.write_all(body).await.expect("write JWKS body");
        });
        let source = HttpJwksSource::new(
            Url::parse(&format!("http://{address}/.well-known/jwks.json")).expect("synthetic URL"),
        )
        .expect("HTTP source");
        let result = source.fetch().await;
        server.await.expect("synthetic server task");
        result
    }

    #[tokio::test]
    async fn http_jwks_requires_one_supported_json_media_type() {
        for content_type in [
            "Content-Type: application/json; charset=utf-8",
            "Content-Type: application/jwk-set+json",
        ] {
            assert_eq!(
                fetch_synthetic_http_response(&[content_type]).await,
                Ok(br#"{"keys":[]}"#.to_vec())
            );
        }
        for headers in [
            Vec::<&str>::new(),
            vec!["Content-Type: text/html"],
            vec![
                "Content-Type: application/json",
                "Content-Type: application/json",
            ],
            vec!["Content-Type: application/json, text/html"],
        ] {
            assert_eq!(
                fetch_synthetic_http_response(&headers).await,
                Err(McpOAuthError::KeySetUnavailable)
            );
        }
    }

    struct KeyMaterial {
        kid: String,
        private: RsaPrivateKey,
    }

    impl KeyMaterial {
        fn generate(kid: &str) -> Self {
            Self {
                kid: kid.to_owned(),
                private: RsaPrivateKey::new(&mut OsRng, 2_048).expect("runtime RSA key"),
            }
        }

        fn jwk(&self) -> Value {
            json!({
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": self.kid,
                "n": URL_SAFE_NO_PAD.encode(self.private.n().to_bytes_be()),
                "e": URL_SAFE_NO_PAD.encode(self.private.e().to_bytes_be()),
            })
        }

        fn sign(&self, claims: &Value, mutate: impl FnOnce(&mut Header)) -> String {
            let mut header = Header::new(Algorithm::RS256);
            header.typ = Some("at+jwt".to_owned());
            header.kid = Some(self.kid.clone());
            mutate(&mut header);
            let der = self.private.to_pkcs1_der().expect("PKCS#1 DER");
            encode(&header, claims, &EncodingKey::from_rsa_der(der.as_bytes()))
                .expect("signed test token")
        }
    }

    #[derive(Default)]
    struct FakeSource {
        responses: Mutex<VecDeque<Result<Vec<u8>, McpOAuthError>>>,
        calls: AtomicUsize,
    }

    impl FakeSource {
        fn from_responses(responses: Vec<Result<Vec<u8>, McpOAuthError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl JwksSource for FakeSource {
        async fn fetch(&self) -> Result<Vec<u8>, McpOAuthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.responses
                .lock()
                .await
                .pop_front()
                .unwrap_or(Err(McpOAuthError::KeySetUnavailable))
        }
    }

    fn config() -> McpOAuthConfig {
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

    fn claims() -> Value {
        let now = get_current_timestamp();
        json!({
            "iss": "https://tenant.eu.auth0.com/",
            "sub": "auth0|personal-owner",
            "aud": "https://api.example.test/mcp",
            "exp": now + 300,
            "iat": now - 1,
            "nbf": now - 1,
            "scope": "openid schedule:read suggestions:submit",
            "client_id": "https://chatgpt.com/oauth/client.json",
        })
    }

    fn jwks(keys: &[&KeyMaterial]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "keys": keys.iter().map(|key| key.jwk()).collect::<Vec<_>>()
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn verifies_exact_token_contract_and_maps_only_dayweave_scopes() {
        let key = KeyMaterial::generate("key-1");
        let source = Arc::new(FakeSource::from_responses(vec![Ok(jwks(&[&key]))]));
        let verifier = McpOAuthVerifier::with_source(config(), source.clone());
        let principal = verifier
            .authenticate(&key.sign(&claims(), |_| {}))
            .await
            .expect("valid access token");

        assert_eq!(principal.subject, "auth0|personal-owner");
        assert_eq!(principal.audience, PrincipalAudience::McpOAuth);
        assert_eq!(
            principal.scopes,
            vec![Scope::ScheduleRead, Scope::SuggestionsSubmit]
        );
        assert_eq!(principal.user_id, Some(Uuid::from_u128(1)));
        assert_eq!(principal.workspace_id, Some(Uuid::from_u128(2)));
        assert_eq!(principal.credential_id, None);
        assert_eq!(principal.allowed_origins, ["https://chatgpt.com"]);
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            verifier.protected_resource_metadata(),
            json!({
                "resource": "https://api.example.test/mcp",
                "authorization_servers": ["https://tenant.eu.auth0.com/"],
                "bearer_methods_supported": ["header"],
                "scopes_supported": ALL_SCOPE_NAMES,
            })
        );
    }

    #[tokio::test]
    async fn rejects_wrong_or_extra_identity_claims_and_client_id_fallbacks() {
        let key = KeyMaterial::generate("key-1");
        let source = Arc::new(FakeSource::from_responses(vec![Ok(jwks(&[&key]))]));
        let verifier = McpOAuthVerifier::with_source(config(), source);

        for (field, value) in [
            ("iss", json!("https://attacker.example/")),
            ("sub", json!("auth0|someone-else")),
            ("aud", json!("https://attacker.example/mcp")),
            ("client_id", json!("unapproved-client")),
        ] {
            let mut changed = claims();
            changed[field] = value;
            assert_eq!(
                verifier.authenticate(&key.sign(&changed, |_| {})).await,
                Err(McpOAuthError::InvalidToken),
                "{field} must be exact"
            );
        }

        let mut extra_audience = claims();
        extra_audience["aud"] = json!([
            "https://api.example.test/mcp",
            "https://attacker.example/mcp"
        ]);
        assert_eq!(
            verifier
                .authenticate(&key.sign(&extra_audience, |_| {}))
                .await,
            Err(McpOAuthError::InvalidToken)
        );

        let mut azp_only = claims();
        let claims_object = azp_only.as_object_mut().unwrap();
        claims_object.remove("client_id");
        claims_object.insert(
            "azp".to_owned(),
            json!("https://chatgpt.com/oauth/client.json"),
        );
        assert_eq!(
            verifier.authenticate(&key.sign(&azp_only, |_| {})).await,
            Err(McpOAuthError::InvalidToken)
        );
    }

    #[tokio::test]
    async fn rejects_time_signature_and_header_downgrades_before_authorizing() {
        let trusted = KeyMaterial::generate("trusted");
        let attacker = KeyMaterial::generate("attacker");
        let source = Arc::new(FakeSource::from_responses(vec![Ok(jwks(&[&trusted]))]));
        let verifier = McpOAuthVerifier::with_source(config(), source);
        let now = get_current_timestamp();

        let mut expired = claims();
        expired["exp"] = json!(now - 1);
        assert_eq!(
            verifier.authenticate(&trusted.sign(&expired, |_| {})).await,
            Err(McpOAuthError::InvalidToken)
        );

        let mut future = claims();
        future["nbf"] = json!(now + CLOCK_SKEW_SECONDS + 10);
        assert_eq!(
            verifier.authenticate(&trusted.sign(&future, |_| {})).await,
            Err(McpOAuthError::InvalidToken)
        );

        let mut at_skew_boundary = claims();
        at_skew_boundary["nbf"] = json!(now + CLOCK_SKEW_SECONDS);
        verifier
            .authenticate(&trusted.sign(&at_skew_boundary, |_| {}))
            .await
            .expect("bounded nbf skew is accepted");

        let mut without_nbf = claims();
        without_nbf.as_object_mut().unwrap().remove("nbf");
        verifier
            .authenticate(&trusted.sign(&without_nbf, |_| {}))
            .await
            .expect("RFC 9068 does not require nbf");

        for required in ["exp", "iss", "aud", "sub", "client_id", "iat", "scope"] {
            let mut missing = claims();
            missing.as_object_mut().unwrap().remove(required);
            assert_eq!(
                verifier.authenticate(&trusted.sign(&missing, |_| {})).await,
                Err(McpOAuthError::InvalidToken),
                "{required} is required"
            );
        }

        let mut future_iat = claims();
        future_iat["iat"] = json!(now + CLOCK_SKEW_SECONDS + 10);
        assert_eq!(
            verifier
                .authenticate(&trusted.sign(&future_iat, |_| {}))
                .await,
            Err(McpOAuthError::InvalidToken)
        );

        let mut overlong = claims();
        overlong["iat"] = json!(now - 1);
        overlong["exp"] = json!(now + MAX_ACCESS_TOKEN_LIFETIME_SECONDS + 1);
        assert_eq!(
            verifier
                .authenticate(&trusted.sign(&overlong, |_| {}))
                .await,
            Err(McpOAuthError::InvalidToken)
        );

        assert_eq!(
            verifier
                .authenticate(&attacker.sign(&claims(), |header| {
                    header.kid = Some("trusted".to_owned());
                }))
                .await,
            Err(McpOAuthError::InvalidToken)
        );

        for token in [
            trusted.sign(&claims(), |header| header.typ = Some("JWT".to_owned())),
            trusted.sign(&claims(), |header| header.alg = Algorithm::RS384),
            trusted.sign(&claims(), |header| header.kid = None),
            trusted.sign(&claims(), |header| {
                header.jku = Some("https://attacker.example/jwks.json".to_owned());
            }),
            trusted.sign(&claims(), |header| {
                header.crit = Some(vec!["unknown".to_owned()]);
            }),
            trusted.sign(&claims(), |header| {
                header.extras.insert("unknown", true);
            }),
        ] {
            assert_eq!(
                verifier.authenticate(&token).await,
                Err(McpOAuthError::InvalidToken)
            );
        }
        assert_eq!(
            validate_compact_jws(&"a".repeat(MAX_TOKEN_LENGTH + 1)),
            Err(McpOAuthError::InvalidToken)
        );
        assert_eq!(
            validate_compact_jws("not.a.compact.jws"),
            Err(McpOAuthError::InvalidToken)
        );
    }

    #[tokio::test]
    async fn refreshes_rotated_keys_singleflight_and_throttles_unknown_kids() {
        let first = KeyMaterial::generate("first");
        let second = KeyMaterial::generate("second");
        let unknown = KeyMaterial::generate("unknown");
        let source = Arc::new(FakeSource::from_responses(vec![
            Ok(jwks(&[&first])),
            Ok(jwks(&[&second])),
            Ok(jwks(&[&second])),
        ]));
        let verifier = Arc::new(McpOAuthVerifier::with_source(config(), source.clone()));
        verifier
            .authenticate(&first.sign(&claims(), |_| {}))
            .await
            .expect("initial key");
        verifier
            .authenticate(&second.sign(&claims(), |_| {}))
            .await
            .expect("rotated key");
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);

        assert_eq!(
            verifier
                .authenticate(&unknown.sign(&claims(), |_| {}))
                .await,
            Err(McpOAuthError::InvalidToken)
        );
        assert_eq!(source.calls.load(Ordering::SeqCst), 3);
        let another_unknown = KeyMaterial::generate("another-unknown");
        assert_eq!(
            verifier
                .authenticate(&another_unknown.sign(&claims(), |_| {}))
                .await,
            Err(McpOAuthError::InvalidToken)
        );
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            3,
            "negative cache prevents an unknown-kid fetch storm"
        );
    }

    #[tokio::test]
    async fn concurrent_cold_cache_requests_share_one_jwks_fetch() {
        let key = KeyMaterial::generate("shared");
        let token = Arc::new(key.sign(&claims(), |_| {}));
        let source = Arc::new(FakeSource::from_responses(vec![Ok(jwks(&[&key]))]));
        let verifier = Arc::new(McpOAuthVerifier::with_source(config(), source.clone()));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let verifier = verifier.clone();
            let token = token.clone();
            tasks.push(tokio::spawn(async move {
                verifier.authenticate(token.as_str()).await
            }));
        }
        for task in tasks {
            task.await
                .expect("task completed")
                .expect("token authenticated");
        }
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn bounded_stale_known_key_survives_outage_then_fails_closed() {
        let key = KeyMaterial::generate("known");
        let source = Arc::new(FakeSource::from_responses(vec![
            Ok(jwks(&[&key])),
            Err(McpOAuthError::KeySetUnavailable),
            Err(McpOAuthError::KeySetUnavailable),
        ]));
        let verifier = McpOAuthVerifier::with_source(config(), source);
        let token = key.sign(&claims(), |_| {});
        verifier.authenticate(&token).await.expect("initial fetch");

        verifier.cache.write().await.as_mut().unwrap().fetched_at = Instant::now()
            .checked_sub(KEY_CACHE_TTL + Duration::from_secs(1))
            .expect("test duration is representable");
        verifier
            .authenticate(&token)
            .await
            .expect("bounded stale known key");

        verifier.cache.write().await.as_mut().unwrap().fetched_at = Instant::now()
            .checked_sub(MAX_STALE_KEY_AGE + Duration::from_secs(1))
            .expect("test duration is representable");
        assert_eq!(
            verifier.authenticate(&token).await,
            Err(McpOAuthError::KeySetUnavailable)
        );
    }

    #[test]
    fn rejects_oversized_duplicate_or_dynamic_jwks_material() {
        let key = KeyMaterial::generate("key-1");
        let mut auth0_style = key.jwk();
        auth0_style["x5c"] = json!([STANDARD.encode(b"synthetic ignored certificate")]);
        auth0_style["x5t"] = json!(URL_SAFE_NO_PAD.encode([7_u8; 20]));
        auth0_style["x5t#S256"] = json!(URL_SAFE_NO_PAD.encode([8_u8; 32]));
        auth0_style["key_ops"] = json!(["verify"]);
        assert!(parse_jwks(&serde_json::to_vec(&json!({"keys": [auth0_style]})).unwrap()).is_ok());

        let duplicate = serde_json::to_vec(&json!({"keys": [key.jwk(), key.jwk()]})).unwrap();
        assert!(matches!(
            parse_jwks(&duplicate),
            Err(McpOAuthError::InvalidKeySet)
        ));

        let mut dynamic = key.jwk();
        dynamic["x5u"] = json!("https://attacker.example/cert.pem");
        assert!(matches!(
            parse_jwks(&serde_json::to_vec(&json!({"keys": [dynamic]})).unwrap()),
            Err(McpOAuthError::InvalidKeySet)
        ));

        for certificates in [
            json!(["not base64!"]),
            json!([STANDARD.encode(vec![0_u8; MAX_X5C_ENCODED_BYTES + 1])]),
            json!(["QQ==", "QQ==", "QQ==", "QQ==", "QQ==", "QQ=="]),
        ] {
            let mut malformed = key.jwk();
            malformed["x5c"] = certificates;
            assert!(matches!(
                parse_jwks(&serde_json::to_vec(&json!({"keys": [malformed]})).unwrap()),
                Err(McpOAuthError::InvalidKeySet)
            ));
        }

        for operations in [json!(["sign"]), json!(["verify", "sign"]), json!([])] {
            let mut contradictory = key.jwk();
            contradictory["key_ops"] = operations;
            assert!(matches!(
                parse_jwks(&serde_json::to_vec(&json!({"keys": [contradictory]})).unwrap()),
                Err(McpOAuthError::InvalidKeySet)
            ));
        }
        assert!(matches!(
            parse_jwks(&vec![b' '; MAX_JWKS_BYTES + 1]),
            Err(McpOAuthError::InvalidKeySet)
        ));

        let mut unexpected_document = Map::new();
        unexpected_document.insert("keys".to_owned(), json!([key.jwk()]));
        unexpected_document.insert("issuer".to_owned(), json!("untrusted"));
        assert!(matches!(
            parse_jwks(&serde_json::to_vec(&unexpected_document).unwrap()),
            Err(McpOAuthError::InvalidKeySet)
        ));
    }
}
