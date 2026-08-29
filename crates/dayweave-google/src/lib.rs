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

use std::{sync::Arc, time::SystemTime};

use reqwest::{Method, RequestBuilder, Response};
use secrecy::ExposeSecret;
use serde::{Serialize, de::DeserializeOwned};
use url::Url;

pub use auth::{AccessTokenProvider, StaticAccessToken};
pub use error::GoogleError;

/// A fully authorized and serialized provider request that has not touched the
/// network yet. Services can finish their durable authorization fence after
/// OAuth refresh/request construction, then consume this value exactly once.
///
/// Deliberately not `Debug`: its request contains an Authorization header.
pub struct PreparedGoogleRequest {
    http: reqwest::Client,
    request: reqwest::Request,
}

impl PreparedGoogleRequest {
    fn ensure_initiation_deadline(
        initiation_deadline: Option<SystemTime>,
    ) -> Result<(), GoogleError> {
        if initiation_deadline.is_some_and(|deadline| SystemTime::now() >= deadline) {
            return Err(GoogleError::DispatchInitiationExpired);
        }
        Ok(())
    }

    /// Sends and parses a prepared JSON request. The deadline is checked at
    /// the last local instruction before `reqwest` starts provider I/O; it is
    /// an initiation deadline, not a response-completion timeout.
    ///
    /// # Errors
    ///
    /// Returns an initiation-expired, transport, provider, or JSON error.
    pub async fn send_json<T: serde::de::DeserializeOwned>(
        self,
        initiation_deadline: Option<SystemTime>,
    ) -> Result<T, GoogleError> {
        Self::ensure_initiation_deadline(initiation_deadline)?;
        let response = self
            .http
            .execute(self.request)
            .await
            .map_err(GoogleError::Transport)?;
        let response = ensure_success(response)?;
        response.json().await.map_err(GoogleError::Transport)
    }

    /// Sends a prepared request whose successful response body is ignored.
    ///
    /// # Errors
    ///
    /// Returns an initiation-expired, transport, or provider error.
    pub async fn send_empty(
        self,
        initiation_deadline: Option<SystemTime>,
    ) -> Result<(), GoogleError> {
        Self::ensure_initiation_deadline(initiation_deadline)?;
        let response = self
            .http
            .execute(self.request)
            .await
            .map_err(GoogleError::Transport)?;
        ensure_success(response)?;
        Ok(())
    }
}

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

    /// Reads a successful JSON response incrementally and never accumulates
    /// more than `max_bytes` of its encoded body before deserialization. A
    /// `Content-Length` check provides an early exit, while the chunk loop is
    /// authoritative for HTTP/2 and chunked responses without a declared size.
    pub(crate) async fn json_limited<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        max_bytes: usize,
    ) -> Result<T, GoogleError> {
        let response = request.send().await.map_err(GoogleError::Transport)?;
        let mut response = ensure_success(response)?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(GoogleError::ResponseTooLarge);
        }
        let initial_capacity = response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(max_bytes);
        let mut body = Vec::with_capacity(initial_capacity);
        while let Some(chunk) = response.chunk().await.map_err(GoogleError::Transport)? {
            if chunk.len() > max_bytes.saturating_sub(body.len()) {
                return Err(GoogleError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| GoogleError::InvalidResponse)
    }

    pub(crate) fn prepare(
        &self,
        request: RequestBuilder,
    ) -> Result<PreparedGoogleRequest, GoogleError> {
        Ok(PreparedGoogleRequest {
            http: self.http.clone(),
            request: request.build().map_err(GoogleError::Transport)?,
        })
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use reqwest::Method;
    use serde_json::Value;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{GoogleClient, GoogleError, StaticAccessToken};

    async fn serve_once(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("request");
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("request headers");
            socket.write_all(response).await.expect("response");
        });
        format!("http://{address}/")
    }

    async fn limited_request(base_url: &str, limit: usize) -> Result<Value, GoogleError> {
        let client = GoogleClient::with_base_url(
            Arc::new(StaticAccessToken::new("test-access-token")),
            base_url,
        )
        .expect("test base URL");
        let url = client.endpoint(&["limited"]).expect("test endpoint");
        let request = client
            .request(Method::GET, url)
            .await
            .expect("authorized request");
        client.json_limited(request, limit).await
    }

    #[tokio::test]
    async fn limited_json_rejects_declared_oversize_before_deserialization() {
        let base_url = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 33\r\nConnection: close\r\n\r\n{\"padding\":\"01234567890123456789\"}",
        )
        .await;
        let error = limited_request(&base_url, 16)
            .await
            .expect_err("declared oversize response");
        assert!(matches!(error, GoogleError::ResponseTooLarge));
    }

    #[tokio::test]
    async fn limited_json_rejects_chunked_oversize_during_streaming() {
        let base_url = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nA\r\n{\"padding\"\r\n17\r\n:\"01234567890123456789\"}\r\n0\r\n\r\n",
        )
        .await;
        let error = limited_request(&base_url, 16)
            .await
            .expect_err("streamed oversize response");
        assert!(matches!(error, GoogleError::ResponseTooLarge));
    }
}
