use std::{fmt::Write, sync::Arc};

use async_trait::async_trait;
use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::{Choice, ConstantTimeEq};
use thiserror::Error;

use crate::{AppState, error::ApiError};

pub type TokenHash = [u8; 32];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    SuggestionsRead,
    SuggestionsWrite,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Principal {
    pub subject: String,
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Error)]
pub enum AuthenticationError {
    #[error("invalid credentials")]
    InvalidCredentials,
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError>;
}

#[derive(Clone, Debug)]
pub struct StaticTokenAuthenticator {
    token_hashes: Arc<Vec<TokenHash>>,
}

impl StaticTokenAuthenticator {
    #[must_use]
    pub fn from_hashes(token_hashes: Arc<Vec<TokenHash>>) -> Self {
        Self { token_hashes }
    }

    #[must_use]
    pub fn from_plaintext(tokens: &[&str]) -> Self {
        Self {
            token_hashes: Arc::new(tokens.iter().map(|token| hash_token(token)).collect()),
        }
    }
}

#[async_trait]
impl Authenticator for StaticTokenAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<Principal, AuthenticationError> {
        let candidate = hash_token(token);
        let mut matched = Choice::from(0);
        for expected in self.token_hashes.iter() {
            matched |= candidate.ct_eq(expected);
        }
        if !bool::from(matched) {
            return Err(AuthenticationError::InvalidCredentials);
        }

        Ok(Principal {
            subject: token_fingerprint(&candidate),
            scopes: vec![Scope::SuggestionsRead, Scope::SuggestionsWrite],
        })
    }
}

#[must_use]
pub fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

fn token_fingerprint(hash: &TokenHash) -> String {
    let prefix = hash[..6]
        .iter()
        .fold(String::with_capacity(12), |mut fingerprint, byte| {
            let _ = write!(fingerprint, "{byte:02x}");
            fingerprint
        });
    format!("token:{prefix}")
}

/// Authenticates a protected request and attaches its [`Principal`].
///
/// # Errors
///
/// Returns an unauthorized API error when the authorization header is missing,
/// malformed, or rejected by the configured authenticator.
pub async fn require_authentication(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer_token)
        .ok_or_else(ApiError::unauthorized)?;

    let principal = state
        .authenticator
        .authenticate(token)
        .await
        .map_err(|_| ApiError::unauthorized())?;
    request.extensions_mut().insert(principal);

    Ok(next.run(request).await)
}

fn parse_bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() || token.contains(' ') {
        return None;
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authenticates_known_token_without_storing_plaintext() {
        let authenticator = StaticTokenAuthenticator::from_plaintext(&["known-secret"]);

        let principal = authenticator
            .authenticate("known-secret")
            .await
            .expect("known token");

        assert!(principal.subject.starts_with("token:"));
        assert_eq!(principal.scopes.len(), 2);
        assert!(authenticator.authenticate("wrong-secret").await.is_err());
    }

    #[test]
    fn bearer_parser_is_strict_but_case_insensitive() {
        assert_eq!(parse_bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(parse_bearer_token("bearer abc"), Some("abc"));
        assert_eq!(parse_bearer_token("Basic abc"), None);
        assert_eq!(parse_bearer_token("Bearer"), None);
        assert_eq!(parse_bearer_token("Bearer two tokens"), None);
    }
}
