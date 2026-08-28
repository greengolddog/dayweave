//! Confidential web-server OAuth with PKCE, offline access, and incremental
//! authorization. Credential persistence belongs to the encrypted service
//! adapter; this module only constructs and executes protocol requests.

use std::collections::BTreeSet;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use crate::GoogleError;

const AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const REVOCATION_ENDPOINT: &str = "https://oauth2.googleapis.com/revoke";

/// OAuth endpoints and confidential-client identity. `Debug` output from the
/// secret field is redacted by `secrecy`.
#[derive(Clone, Debug)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: SecretString,
    pub redirect_uri: Url,
    authorization_endpoint: Url,
    token_endpoint: Url,
    revocation_endpoint: Url,
}

impl OAuthConfig {
    /// Creates the production Google web-server OAuth configuration.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::InvalidOAuthRequest`] when the redirect URI or
    /// one of Google's constant endpoints cannot be parsed.
    pub fn production(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: &str,
    ) -> Result<Self, GoogleError> {
        let redirect_uri = parse_url(redirect_uri, "redirect URI is invalid")?;
        Self::with_endpoints(
            client_id,
            client_secret,
            redirect_uri,
            AUTHORIZATION_ENDPOINT,
            TOKEN_ENDPOINT,
            REVOCATION_ENDPOINT,
        )
    }

    /// Supplies alternate endpoints for isolated contract tests.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::InvalidOAuthRequest`] for an invalid endpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn with_endpoints(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: Url,
        authorization_endpoint: &str,
        token_endpoint: &str,
        revocation_endpoint: &str,
    ) -> Result<Self, GoogleError> {
        let client_id = client_id.into();
        if client_id.trim().is_empty() {
            return Err(GoogleError::InvalidOAuthRequest("client ID is empty"));
        }
        let client_secret = client_secret.into();
        if client_secret.is_empty() {
            return Err(GoogleError::InvalidOAuthRequest("client secret is empty"));
        }
        Ok(Self {
            client_id,
            client_secret: SecretString::from(client_secret),
            redirect_uri,
            authorization_endpoint: parse_url(
                authorization_endpoint,
                "authorization endpoint is invalid",
            )?,
            token_endpoint: parse_url(token_endpoint, "token endpoint is invalid")?,
            revocation_endpoint: parse_url(revocation_endpoint, "revocation endpoint is invalid")?,
        })
    }
}

fn parse_url(value: &str, message: &'static str) -> Result<Url, GoogleError> {
    let url = Url::parse(value).map_err(|_| GoogleError::InvalidOAuthRequest(message))?;
    if url.cannot_be_a_base() {
        return Err(GoogleError::InvalidOAuthRequest(message));
    }
    Ok(url)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorizationOptions {
    /// OAuth scope URIs. A sorted set makes generated authorization URLs
    /// deterministic apart from secure state and verifier values.
    pub scopes: BTreeSet<String>,
    /// Forces Google's consent screen, useful when a new refresh token is
    /// required after disconnect or credential loss.
    pub force_consent: bool,
    pub login_hint: Option<String>,
}

/// Pending authorization material. Persist the state and verifier only in an
/// encrypted, short-lived server record; neither should enter logs.
#[derive(Clone)]
pub struct AuthorizationSession {
    pub authorization_url: Url,
    state: SecretString,
    code_verifier: SecretString,
}

impl std::fmt::Debug for AuthorizationSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut redacted_url = self.authorization_url.clone();
        redacted_url.set_query(None);
        formatter
            .debug_struct("AuthorizationSession")
            .field("authorization_endpoint", &redacted_url)
            .field("state", &"[REDACTED]")
            .field("code_verifier", &"[REDACTED]")
            .finish()
    }
}

impl AuthorizationSession {
    /// Rehydrates a short-lived session from encrypted server state.
    #[must_use]
    pub fn from_stored(
        authorization_url: Url,
        state: SecretString,
        code_verifier: SecretString,
    ) -> Self {
        Self {
            authorization_url,
            state,
            code_verifier,
        }
    }

    /// Returns the opaque state for encrypted persistence, not presentation.
    #[must_use]
    pub const fn state(&self) -> &SecretString {
        &self.state
    }

    /// Returns the verifier for encrypted persistence, not presentation.
    #[must_use]
    pub const fn code_verifier(&self) -> &SecretString {
        &self.code_verifier
    }
}

#[derive(Clone, Debug)]
pub struct OAuthTokenSet {
    pub access_token: SecretString,
    /// Google may omit this during repeat authorization. Callers must retain
    /// the previously stored refresh token unless the account is disconnected.
    pub refresh_token: Option<SecretString>,
    pub expires_in_seconds: u64,
    pub token_type: String,
    pub granted_scopes: BTreeSet<String>,
    pub id_token: Option<SecretString>,
}

#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    config: OAuthConfig,
}

impl OAuthClient {
    /// Creates an OAuth transport that never enables automatic redirect
    /// following for token calls.
    ///
    /// # Errors
    ///
    /// Returns a transport error if the HTTP client cannot be constructed.
    pub fn new(config: OAuthConfig) -> Result<Self, GoogleError> {
        Ok(Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("DayWeave/0.1")
                .build()
                .map_err(GoogleError::Transport)?,
            config,
        })
    }

    /// Generates a CSRF state value and RFC 7636 S256 challenge, then builds
    /// Google's incremental, offline authorization URL.
    ///
    /// # Errors
    ///
    /// Returns an invalid-request error for empty scopes or a randomness error
    /// if the operating system cannot provide entropy.
    pub fn begin_authorization(
        &self,
        options: &AuthorizationOptions,
    ) -> Result<AuthorizationSession, GoogleError> {
        if options.scopes.is_empty() || options.scopes.iter().any(|scope| scope.trim().is_empty()) {
            return Err(GoogleError::InvalidOAuthRequest(
                "at least one non-empty scope is required",
            ));
        }
        let state = random_urlsafe(32)?;
        let code_verifier = random_urlsafe(64)?;
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let mut authorization_url = self.config.authorization_endpoint.clone();
        {
            let mut query = authorization_url.query_pairs_mut();
            query
                .append_pair("client_id", &self.config.client_id)
                .append_pair("redirect_uri", self.config.redirect_uri.as_str())
                .append_pair("response_type", "code")
                .append_pair(
                    "scope",
                    &options.scopes.iter().cloned().collect::<Vec<_>>().join(" "),
                )
                .append_pair("access_type", "offline")
                .append_pair("include_granted_scopes", "true")
                .append_pair("state", &state)
                .append_pair("code_challenge", &code_challenge)
                .append_pair("code_challenge_method", "S256");
            if options.force_consent {
                query.append_pair("prompt", "consent");
            }
            if let Some(login_hint) = &options.login_hint {
                query.append_pair("login_hint", login_hint);
            }
        }
        Ok(AuthorizationSession {
            authorization_url,
            state: SecretString::from(state),
            code_verifier: SecretString::from(code_verifier),
        })
    }

    /// Validates callback state before exchanging an authorization code.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::OAuthStateMismatch`] without network access when
    /// state differs, or a typed OAuth/transport error for token exchange.
    pub async fn exchange_code(
        &self,
        session: &AuthorizationSession,
        returned_state: &str,
        authorization_code: &SecretString,
    ) -> Result<OAuthTokenSet, GoogleError> {
        if !constant_time_equal(session.state.expose_secret(), returned_state) {
            return Err(GoogleError::OAuthStateMismatch);
        }
        self.token_request(&[
            ("grant_type", "authorization_code".to_owned()),
            ("code", authorization_code.expose_secret().to_owned()),
            (
                "code_verifier",
                session.code_verifier.expose_secret().to_owned(),
            ),
            ("redirect_uri", self.config.redirect_uri.to_string()),
        ])
        .await
    }

    /// Exchanges an encrypted long-lived refresh credential for a short-lived
    /// access token.
    ///
    /// # Errors
    ///
    /// Returns typed OAuth, authorization, rate-limit, or transport errors.
    pub async fn refresh(
        &self,
        refresh_token: &SecretString,
    ) -> Result<OAuthTokenSet, GoogleError> {
        self.token_request(&[
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token.expose_secret().to_owned()),
        ])
        .await
    }

    /// Revokes a refresh or access token during disconnect/recovery.
    ///
    /// # Errors
    ///
    /// Returns a typed OAuth or transport error if Google rejects revocation.
    pub async fn revoke(&self, token: &SecretString) -> Result<(), GoogleError> {
        let response = self
            .http
            .post(self.config.revocation_endpoint.clone())
            .form(&[("token", token.expose_secret())])
            .send()
            .await
            .map_err(GoogleError::Transport)?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(oauth_response_error(response).await)
    }

    async fn token_request(
        &self,
        grant_fields: &[(&str, String)],
    ) -> Result<OAuthTokenSet, GoogleError> {
        let mut form = vec![
            ("client_id", self.config.client_id.clone()),
            (
                "client_secret",
                self.config.client_secret.expose_secret().to_owned(),
            ),
        ];
        form.extend(grant_fields.iter().cloned());
        let response = self
            .http
            .post(self.config.token_endpoint.clone())
            .form(&form)
            .send()
            .await
            .map_err(GoogleError::Transport)?;
        if !response.status().is_success() {
            return Err(oauth_response_error(response).await);
        }
        let wire: TokenResponse = response.json().await.map_err(GoogleError::Transport)?;
        if wire.access_token.is_empty() || wire.token_type.is_empty() {
            return Err(GoogleError::InvalidOAuthRequest(
                "token response omitted required fields",
            ));
        }
        Ok(OAuthTokenSet {
            access_token: SecretString::from(wire.access_token),
            refresh_token: wire.refresh_token.map(SecretString::from),
            expires_in_seconds: wire.expires_in,
            token_type: wire.token_type,
            granted_scopes: wire
                .scope
                .unwrap_or_default()
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect(),
            id_token: wire.id_token.map(SecretString::from),
        })
    }
}

fn random_urlsafe(bytes: usize) -> Result<String, GoogleError> {
    let mut value = vec![0_u8; bytes];
    getrandom::fill(&mut value).map_err(|_| GoogleError::Randomness)?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

fn constant_time_equal(expected: &str, actual: &str) -> bool {
    bool::from(expected.as_bytes().ct_eq(actual.as_bytes()))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
    token_type: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: String,
}

async fn oauth_response_error(response: reqwest::Response) -> GoogleError {
    let status = response.status();
    let parsed = response.json::<OAuthErrorResponse>().await.ok();
    if let Some(error) = parsed {
        return GoogleError::OAuthRejected { code: error.error };
    }
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => GoogleError::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => GoogleError::RateLimited {
            retry_after_seconds: None,
        },
        value if value.is_server_error() => GoogleError::Temporary {
            status: value.as_u16(),
        },
        value => GoogleError::Api {
            status: value.as_u16(),
        },
    }
}
