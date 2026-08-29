use std::{fmt, path::PathBuf};

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

#[derive(Clone, Eq, PartialEq)]
pub struct ServerInfo {
    pub(crate) platform_family: String,
    pub(crate) platform_os: String,
}

impl ServerInfo {
    #[must_use]
    pub fn platform_family(&self) -> &str {
        &self.platform_family
    }

    #[must_use]
    pub fn platform_os(&self) -> &str {
        &self.platform_os
    }
}

impl fmt::Debug for ServerInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerInfo")
            .field("platform_family", &self.platform_family)
            .field("platform_os", &self.platform_os)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Account {
    ApiKey,
    ChatGpt {
        email: Option<String>,
        plan_type: Option<String>,
    },
    AmazonBedrock {
        credential_source: BedrockCredentialSource,
    },
    Other {
        account_type: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BedrockCredentialSource {
    CodexManaged,
    AwsManaged,
}

#[derive(Clone, Eq, PartialEq)]
pub struct AccountState {
    pub(crate) account: Option<Account>,
    pub(crate) requires_openai_auth: bool,
}

impl AccountState {
    #[must_use]
    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    #[must_use]
    pub const fn requires_openai_auth(&self) -> bool {
        self.requires_openai_auth
    }
}

#[derive(Clone)]
pub struct BrowserLogin {
    pub(crate) login_id: SecretString,
    pub(crate) auth_url: SecretString,
}

impl BrowserLogin {
    #[must_use]
    pub fn login_id(&self) -> &str {
        self.login_id.expose_secret()
    }

    #[must_use]
    pub fn auth_url(&self) -> &str {
        self.auth_url.expose_secret()
    }
}

impl fmt::Debug for BrowserLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserLogin")
            .field("login_id", &"<redacted>")
            .field("auth_url", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct DeviceCodeLogin {
    pub(crate) login_id: SecretString,
    pub(crate) verification_url: SecretString,
    pub(crate) user_code: SecretString,
}

impl DeviceCodeLogin {
    #[must_use]
    pub fn login_id(&self) -> &str {
        self.login_id.expose_secret()
    }

    #[must_use]
    pub fn verification_url(&self) -> &str {
        self.verification_url.expose_secret()
    }

    #[must_use]
    pub fn user_code(&self) -> &str {
        self.user_code.expose_secret()
    }
}

impl fmt::Debug for DeviceCodeLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCodeLogin")
            .field("login_id", &"<redacted>")
            .field("verification_url", &"<redacted>")
            .field("user_code", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct ThreadOptions {
    pub(crate) cwd: PathBuf,
    pub(crate) model: Option<String>,
}

impl ThreadOptions {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            model: None,
        }
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

impl fmt::Debug for ThreadOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ThreadOptions")
            .field("cwd", &"<absolute path>")
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ThreadHandle {
    pub(crate) id: String,
    pub(crate) cwd: PathBuf,
}

impl ThreadHandle {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl fmt::Debug for ThreadHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ThreadHandle")
            .field("id", &self.id)
            .field("cwd", &"<canonical restricted root>")
            .finish()
    }
}

pub struct StructuredTurnRequest {
    pub(crate) prompt: SecretString,
    pub(crate) output_schema: Value,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
}

impl StructuredTurnRequest {
    #[must_use]
    pub fn new(prompt: SecretString, output_schema: Value) -> Self {
        Self {
            prompt,
            output_schema,
            model: None,
            effort: None,
        }
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = Some(effort.into());
        self
    }
}

impl fmt::Debug for StructuredTurnRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StructuredTurnRequest")
            .field("prompt", &"<redacted>")
            .field("output_schema", &"<redacted>")
            .field("model", &self.model)
            .field("effort", &self.effort)
            .finish()
    }
}

pub struct StructuredTurn<T> {
    pub(crate) turn_id: String,
    pub(crate) output: T,
}

impl<T> StructuredTurn<T> {
    #[must_use]
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    #[must_use]
    pub fn output(&self) -> &T {
        &self.output
    }

    #[must_use]
    pub fn into_output(self) -> T {
        self.output
    }
}
