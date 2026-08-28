use async_trait::async_trait;
use secrecy::SecretString;

use crate::GoogleError;

/// Supplies short-lived access tokens. Refresh-token storage and rotation are
/// deliberately implemented by the encrypted server credential adapter.
#[async_trait]
pub trait AccessTokenProvider: Send + Sync {
    async fn access_token(&self) -> Result<SecretString, GoogleError>;
}

/// Useful for isolated tests and local OAuth wiring. The wrapped value's debug
/// representation remains redacted by `secrecy`.
#[derive(Clone)]
pub struct StaticAccessToken(SecretString);

impl StaticAccessToken {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }
}

#[async_trait]
impl AccessTokenProvider for StaticAccessToken {
    async fn access_token(&self) -> Result<SecretString, GoogleError> {
        Ok(self.0.clone())
    }
}
