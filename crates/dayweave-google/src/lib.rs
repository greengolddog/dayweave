//! Typed, testable Google Calendar and Google Tasks transport adapters.
//!
//! This crate owns HTTP/JSON interoperability only. Canonical state, sync
//! conflict policy, encryption, and UI confirmation remain in `DayWeave`'s core
//! and service layers.

mod auth;
pub mod calendar;
mod error;
pub mod oauth;
pub mod recurrence;
pub mod tasks;

use std::sync::Arc;

use reqwest::{Method, RequestBuilder, Response};
use secrecy::ExposeSecret;
use serde::Serialize;
use url::Url;

pub use auth::{AccessTokenProvider, StaticAccessToken};
pub use error::GoogleError;

/// Shared authorized transport. It never logs access tokens or response bodies.
#[derive(Clone)]
pub struct GoogleClient {
    http: reqwest::Client,
    token_provider: Arc<dyn AccessTokenProvider>,
    api_base: Url,
}

impl GoogleClient {
    /// Uses Google's production API origin.
    ///
    /// # Errors
    ///
    /// Returns an error only if the constant production URL cannot be parsed.
    pub fn production(token_provider: Arc<dyn AccessTokenProvider>) -> Result<Self, GoogleError> {
        Self::with_base_url(token_provider, "https://www.googleapis.com/")
    }

    /// Supports an alternate origin for isolated contract tests.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::InvalidBaseUrl`] for a malformed or non-base URL.
    pub fn with_base_url(
        token_provider: Arc<dyn AccessTokenProvider>,
        base_url: &str,
    ) -> Result<Self, GoogleError> {
        let api_base = Url::parse(base_url).map_err(|_| GoogleError::InvalidBaseUrl)?;
        if api_base.cannot_be_a_base()
            || api_base.username() != ""
            || api_base.password().is_some()
            || api_base.query().is_some()
            || api_base.fragment().is_some()
        {
            return Err(GoogleError::InvalidBaseUrl);
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent("DayWeave/0.1")
                // Authorization headers must never be replayed to a redirect
                // target selected by a provider or test double.
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(GoogleError::Transport)?,
            token_provider,
            api_base,
        })
    }

    pub(crate) fn endpoint(&self, segments: &[&str]) -> Result<Url, GoogleError> {
        let mut url = self.api_base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| GoogleError::InvalidBaseUrl)?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        url: Url,
    ) -> Result<RequestBuilder, GoogleError> {
        let token = self.token_provider.access_token().await?;
        Ok(self
            .http
            .request(method, url)
            .bearer_auth(token.expose_secret()))
    }

    pub(crate) async fn json<T: serde::de::DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, GoogleError> {
        let response = request.send().await.map_err(GoogleError::Transport)?;
        let response = ensure_success(response)?;
        response.json().await.map_err(GoogleError::Transport)
    }

    pub(crate) async fn empty(&self, request: RequestBuilder) -> Result<(), GoogleError> {
        let response = request.send().await.map_err(GoogleError::Transport)?;
        ensure_success(response)?;
        Ok(())
    }

    pub(crate) fn body<T: Serialize + ?Sized>(request: RequestBuilder, body: &T) -> RequestBuilder {
        request.json(body)
    }
}

fn ensure_success(response: Response) -> Result<Response, GoogleError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let retry_after_seconds = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    match status.as_u16() {
        401 | 403 => Err(GoogleError::Unauthorized),
        410 => Err(GoogleError::SyncTokenExpired),
        412 => Err(GoogleError::PreconditionFailed),
        429 => Err(GoogleError::RateLimited {
            retry_after_seconds,
        }),
        code if status.is_server_error() => Err(GoogleError::Temporary { status: code }),
        code => Err(GoogleError::Api { status: code }),
    }
}
