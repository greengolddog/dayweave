use std::{
    collections::{BTreeMap, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use secrecy::SecretString;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::auth::{TokenHash, hash_token};

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_PROPOSAL_TTL_HOURS: u64 = 7 * 24;
const MAX_PROPOSAL_TTL_HOURS: u64 = 365 * 24;
const MINIMUM_TOKEN_LENGTH: usize = 24;
const DEFAULT_DATABASE_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_DATABASE_MIN_CONNECTIONS: u32 = 1;
const DEFAULT_DATABASE_ACQUIRE_TIMEOUT_SECONDS: u32 = 10;
const DEFAULT_USER_ID: &str = "00000000-0000-4000-8000-000000000001";
const DEFAULT_WORKSPACE_ID: &str = "00000000-0000-4000-8000-000000000002";
const DEFAULT_GOOGLE_OAUTH_SESSION_TTL_MINUTES: u64 = 10;
const MIN_GOOGLE_OAUTH_SESSION_TTL_MINUTES: u64 = 5;
const MAX_GOOGLE_OAUTH_SESSION_TTL_MINUTES: u64 = 30;

pub const GOOGLE_CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";
pub const GOOGLE_TASKS_SCOPE: &str = "https://www.googleapis.com/auth/tasks";
pub const GOOGLE_CALENDAR_READONLY_SCOPE: &str =
    "https://www.googleapis.com/auth/calendar.readonly";
pub const GOOGLE_TASKS_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/tasks.readonly";
pub const GOOGLE_OPENID_SCOPE: &str = "openid";
pub const GOOGLE_EMAIL_SCOPE: &str = "email";

pub struct CredentialKey([u8; 32]);

impl CredentialKey {
    pub(crate) const fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_test_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Clone for CredentialKey {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl std::fmt::Debug for CredentialKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialKey([REDACTED])")
    }
}

impl Drop for CredentialKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: SecretString,
    pub redirect_uri: Url,
    pub keys: Arc<BTreeMap<u32, CredentialKey>>,
    pub active_key_version: u32,
    pub session_ttl: Duration,
}

impl std::fmt::Debug for GoogleOAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleOAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("redirect_uri", &self.redirect_uri)
            .field("key_versions", &self.keys.keys().collect::<Vec<_>>())
            .field("active_key_version", &self.active_key_version)
            .field("session_ttl", &self.session_ttl)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Development,
    Test,
    Staging,
    Production,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMode {
    LegacyStatic,
    Hybrid,
    CredentialOnly,
}

impl FromStr for AuthMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "legacy_static" => Ok(Self::LegacyStatic),
            "hybrid" => Ok(Self::Hybrid),
            "credential_only" => Ok(Self::CredentialOnly),
            _ => Err(ConfigError::InvalidAuthMode(value.to_owned())),
        }
    }
}

impl FromStr for Environment {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "staging" => Ok(Self::Staging),
            "production" | "prod" => Ok(Self::Production),
            _ => Err(ConfigError::InvalidEnvironment(value.to_owned())),
        }
    }
}

#[derive(Clone)]
pub struct DatabaseUrl(String);

impl DatabaseUrl {
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DatabaseUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DatabaseUrl([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    pub url: DatabaseUrl,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub user_id: Uuid,
    pub workspace_id: Uuid,
    pub owner_subject: String,
    pub timezone_name: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_address: SocketAddr,
    pub environment: Environment,
    pub auth_mode: AuthMode,
    pub api_token_hashes: Arc<Vec<TokenHash>>,
    pub proposal_ttl: Duration,
    pub mcp_allowed_origins: Arc<Vec<String>>,
    pub database: Option<DatabaseConfig>,
    pub google_oauth: Option<GoogleOAuthConfig>,
    pub log_filter: String,
    pub json_logs: bool,
}

impl Config {
    /// Loads and validates configuration from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when required credentials are absent or a value
    /// cannot be parsed or fails the security constraints.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_map(&std::env::vars().collect())
    }

    /// Parses configuration from a key-value map.
    ///
    /// This entry point keeps configuration tests isolated from global process
    /// environment mutation.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when required credentials are absent or a value
    /// cannot be parsed or fails the security constraints.
    #[allow(clippy::too_many_lines)] // Keeps cross-field security validation in one parse boundary.
    pub fn from_map(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let environment = values
            .get("DAYWEAVE_ENVIRONMENT")
            .map_or(Ok(Environment::Development), |value| value.parse())?;
        let auth_mode = values
            .get("DAYWEAVE_AUTH_MODE")
            .map_or(Ok(AuthMode::LegacyStatic), |value| value.parse())?;

        let bind_address = values
            .get("DAYWEAVE_BIND_ADDRESS")
            .or_else(|| values.get("DAYWEAVE_BIND"))
            .map_or_else(
                || {
                    Ok(SocketAddr::new(
                        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                        DEFAULT_PORT,
                    ))
                },
                |value| {
                    value
                        .parse()
                        .map_err(|_| ConfigError::InvalidBindAddress(value.clone()))
                },
            )?;

        let raw_tokens = values
            .get("DAYWEAVE_API_TOKENS")
            .or_else(|| values.get("DAYWEAVE_API_TOKEN"));
        if auth_mode == AuthMode::CredentialOnly
            && ["DAYWEAVE_API_TOKENS", "DAYWEAVE_API_TOKEN"]
                .iter()
                .filter_map(|key| values.get(*key))
                .any(|value| !value.trim().is_empty())
        {
            return Err(ConfigError::StaticTokensForbidden);
        }
        let tokens: Vec<_> = raw_tokens.map_or_else(Vec::new, |raw_tokens| {
            raw_tokens
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect()
        });
        if auth_mode != AuthMode::CredentialOnly && tokens.is_empty() {
            return Err(ConfigError::MissingApiTokens);
        }
        if tokens
            .iter()
            .any(|token| token.len() < MINIMUM_TOKEN_LENGTH)
        {
            return Err(ConfigError::ApiTokenTooShort(MINIMUM_TOKEN_LENGTH));
        }
        if tokens.iter().any(|token| token.starts_with("dw_")) {
            return Err(ConfigError::ReservedApiTokenPrefix);
        }
        let api_token_hashes = Arc::new(tokens.into_iter().map(hash_token).collect());

        let ttl_hours = values.get("DAYWEAVE_PROPOSAL_TTL_HOURS").map_or(
            Ok(DEFAULT_PROPOSAL_TTL_HOURS),
            |value| {
                value
                    .parse::<u64>()
                    .map_err(|_| ConfigError::InvalidProposalTtl(value.clone()))
            },
        )?;
        if ttl_hours == 0 || ttl_hours > MAX_PROPOSAL_TTL_HOURS {
            return Err(ConfigError::InvalidProposalTtl(ttl_hours.to_string()));
        }
        let ttl_seconds = ttl_hours
            .checked_mul(60 * 60)
            .ok_or_else(|| ConfigError::InvalidProposalTtl(ttl_hours.to_string()))?;

        let log_filter = values
            .get("RUST_LOG")
            .cloned()
            .unwrap_or_else(|| "dayweave_api=info,tower_http=info".to_owned());
        let json_logs = values
            .get("DAYWEAVE_JSON_LOGS")
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
        let mcp_allowed_origins = Arc::new(values.get("DAYWEAVE_MCP_ALLOWED_ORIGINS").map_or(
            Ok(Vec::new()),
            |value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|origin| !origin.is_empty())
                    .map(validate_origin)
                    .collect::<Result<Vec<_>, _>>()
            },
        )?);
        let database = database_config(values)?;
        if matches!(environment, Environment::Staging | Environment::Production)
            && database.is_none()
        {
            return Err(ConfigError::MissingDatabaseUrl);
        }
        if auth_mode != AuthMode::LegacyStatic && database.is_none() {
            return Err(ConfigError::AuthModeRequiresDatabase);
        }
        let google_oauth = google_oauth_config(values)?;
        if google_oauth.is_some() && database.is_none() {
            return Err(ConfigError::MissingGoogleOAuthDatabase);
        }

        Ok(Self {
            bind_address,
            environment,
            auth_mode,
            api_token_hashes,
            proposal_ttl: Duration::from_secs(ttl_seconds),
            mcp_allowed_origins,
            database,
            google_oauth,
            log_filter,
            json_logs,
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ConfigError {
    #[error("DAYWEAVE_API_TOKENS or DAYWEAVE_API_TOKEN must contain an API token")]
    MissingApiTokens,
    #[error("each API token must contain at least {0} characters")]
    ApiTokenTooShort(usize),
    #[error("static API tokens cannot use the reserved durable-credential prefix")]
    ReservedApiTokenPrefix,
    #[error("invalid DAYWEAVE_AUTH_MODE: {0}")]
    InvalidAuthMode(String),
    #[error("hybrid and credential_only authentication require DAYWEAVE_DATABASE_URL")]
    AuthModeRequiresDatabase,
    #[error("static API token configuration is forbidden in credential_only mode")]
    StaticTokensForbidden,
    #[error("invalid DAYWEAVE_BIND_ADDRESS: {0}")]
    InvalidBindAddress(String),
    #[error("invalid DAYWEAVE_ENVIRONMENT: {0}")]
    InvalidEnvironment(String),
    #[error("invalid DAYWEAVE_PROPOSAL_TTL_HOURS: {0}")]
    InvalidProposalTtl(String),
    #[error("invalid DAYWEAVE_MCP_ALLOWED_ORIGINS entry: {0}")]
    InvalidMcpOrigin(String),
    #[error("DAYWEAVE_DATABASE_URL is required in staging and production")]
    MissingDatabaseUrl,
    #[error("DAYWEAVE_DATABASE_URL must be a non-empty PostgreSQL URL")]
    InvalidDatabaseUrl,
    #[error("invalid database pool configuration")]
    InvalidDatabasePool,
    #[error("invalid DAYWEAVE_DEFAULT_USER_ID or DAYWEAVE_DEFAULT_WORKSPACE_ID")]
    InvalidDatabaseScope,
    #[error("invalid DAYWEAVE_DEFAULT_TIMEZONE")]
    InvalidTimezone,
    #[error("invalid DAYWEAVE_OWNER_SUBJECT")]
    InvalidOwnerSubject,
    #[error("DAYWEAVE_GOOGLE_OAUTH_ENABLED must be true or false")]
    InvalidGoogleOAuthEnabled,
    #[error("Google OAuth settings were supplied while DAYWEAVE_GOOGLE_OAUTH_ENABLED is not true")]
    DisabledGoogleOAuthConfiguration,
    #[error("Google OAuth is enabled but a required setting is missing: {0}")]
    MissingGoogleOAuthSetting(&'static str),
    #[error("DAYWEAVE_DATABASE_URL is required whenever Google OAuth is enabled")]
    MissingGoogleOAuthDatabase,
    #[error("DAYWEAVE_GOOGLE_REDIRECT_URI must be an exact HTTPS callback URI")]
    InvalidGoogleRedirectUri,
    #[error(
        "DAYWEAVE_GOOGLE_CREDENTIAL_KEYS must contain unique entries formatted vN:<64 lowercase hex characters>"
    )]
    InvalidGoogleCredentialKeys,
    #[error("DAYWEAVE_GOOGLE_ACTIVE_CREDENTIAL_KEY_VERSION must name a configured key as vN")]
    InvalidGoogleActiveCredentialKeyVersion,
    #[error("invalid DAYWEAVE_GOOGLE_OAUTH_SESSION_TTL_MINUTES")]
    InvalidGoogleOAuthSessionTtl,
}

fn google_oauth_config(
    values: &HashMap<String, String>,
) -> Result<Option<GoogleOAuthConfig>, ConfigError> {
    let enabled = match values
        .get("DAYWEAVE_GOOGLE_OAUTH_ENABLED")
        .map(String::as_str)
    {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(ConfigError::InvalidGoogleOAuthEnabled),
    };
    let google_settings_present = values
        .keys()
        .any(|key| key.starts_with("DAYWEAVE_GOOGLE_") && key != "DAYWEAVE_GOOGLE_OAUTH_ENABLED");
    if !enabled {
        if google_settings_present {
            return Err(ConfigError::DisabledGoogleOAuthConfiguration);
        }
        return Ok(None);
    }

    let required = |key: &'static str| {
        values
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or(ConfigError::MissingGoogleOAuthSetting(key))
    };
    let client_id = required("DAYWEAVE_GOOGLE_CLIENT_ID")?.to_owned();
    let client_secret = SecretString::from(required("DAYWEAVE_GOOGLE_CLIENT_SECRET")?.to_owned());
    let redirect_uri = parse_google_redirect_uri(required("DAYWEAVE_GOOGLE_REDIRECT_URI")?)?;
    let keys = parse_credential_keys(required("DAYWEAVE_GOOGLE_CREDENTIAL_KEYS")?)?;
    let active_key_version =
        parse_key_version(required("DAYWEAVE_GOOGLE_ACTIVE_CREDENTIAL_KEY_VERSION")?)
            .ok_or(ConfigError::InvalidGoogleActiveCredentialKeyVersion)?;
    if !keys.contains_key(&active_key_version) {
        return Err(ConfigError::InvalidGoogleActiveCredentialKeyVersion);
    }
    let ttl_minutes = values
        .get("DAYWEAVE_GOOGLE_OAUTH_SESSION_TTL_MINUTES")
        .map_or(Ok(DEFAULT_GOOGLE_OAUTH_SESSION_TTL_MINUTES), |value| {
            value
                .parse::<u64>()
                .map_err(|_| ConfigError::InvalidGoogleOAuthSessionTtl)
        })?;
    if !(MIN_GOOGLE_OAUTH_SESSION_TTL_MINUTES..=MAX_GOOGLE_OAUTH_SESSION_TTL_MINUTES)
        .contains(&ttl_minutes)
    {
        return Err(ConfigError::InvalidGoogleOAuthSessionTtl);
    }

    Ok(Some(GoogleOAuthConfig {
        client_id,
        client_secret,
        redirect_uri,
        keys: Arc::new(keys),
        active_key_version,
        session_ttl: Duration::from_secs(ttl_minutes * 60),
    }))
}

fn parse_google_redirect_uri(value: &str) -> Result<Url, ConfigError> {
    let uri = Url::parse(value).map_err(|_| ConfigError::InvalidGoogleRedirectUri)?;
    if uri.username() != ""
        || uri.password().is_some()
        || uri.query().is_some()
        || uri.fragment().is_some()
        || uri.host().is_none()
        || uri.path() != "/v1/integrations/google/oauth/callback"
    {
        return Err(ConfigError::InvalidGoogleRedirectUri);
    }
    if uri.scheme() == "https" {
        return Ok(uri);
    }
    Err(ConfigError::InvalidGoogleRedirectUri)
}

fn parse_credential_keys(value: &str) -> Result<BTreeMap<u32, CredentialKey>, ConfigError> {
    let mut keys = BTreeMap::new();
    for entry in value.split(',').map(str::trim) {
        let (raw_version, raw_key) = entry
            .split_once(':')
            .ok_or(ConfigError::InvalidGoogleCredentialKeys)?;
        let version =
            parse_key_version(raw_version).ok_or(ConfigError::InvalidGoogleCredentialKeys)?;
        if raw_key.len() != 64
            || !raw_key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ConfigError::InvalidGoogleCredentialKeys);
        }
        let mut decoded = [0_u8; 32];
        for (index, pair) in raw_key.as_bytes().chunks_exact(2).enumerate() {
            let pair =
                std::str::from_utf8(pair).map_err(|_| ConfigError::InvalidGoogleCredentialKeys)?;
            decoded[index] = u8::from_str_radix(pair, 16)
                .map_err(|_| ConfigError::InvalidGoogleCredentialKeys)?;
        }
        if keys.insert(version, CredentialKey(decoded)).is_some() {
            return Err(ConfigError::InvalidGoogleCredentialKeys);
        }
    }
    if keys.is_empty() {
        return Err(ConfigError::InvalidGoogleCredentialKeys);
    }
    Ok(keys)
}

fn parse_key_version(value: &str) -> Option<u32> {
    let digits = value.strip_prefix('v')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let version = digits.parse().ok()?;
    (version > 0 && i32::try_from(version).is_ok() && !digits.starts_with('0')).then_some(version)
}

fn database_config(
    values: &HashMap<String, String>,
) -> Result<Option<DatabaseConfig>, ConfigError> {
    let Some(raw_url) = values.get("DAYWEAVE_DATABASE_URL") else {
        return Ok(None);
    };
    if raw_url.trim().is_empty()
        || !(raw_url.starts_with("postgres://") || raw_url.starts_with("postgresql://"))
    {
        return Err(ConfigError::InvalidDatabaseUrl);
    }
    let max_connections = parse_database_value(
        values,
        "DAYWEAVE_DATABASE_MAX_CONNECTIONS",
        DEFAULT_DATABASE_MAX_CONNECTIONS,
    )?;
    let min_connections = parse_database_value(
        values,
        "DAYWEAVE_DATABASE_MIN_CONNECTIONS",
        DEFAULT_DATABASE_MIN_CONNECTIONS,
    )?;
    let acquire_timeout_seconds = parse_database_value(
        values,
        "DAYWEAVE_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
        DEFAULT_DATABASE_ACQUIRE_TIMEOUT_SECONDS,
    )?;
    if max_connections == 0
        || max_connections > 100
        || min_connections > max_connections
        || acquire_timeout_seconds == 0
        || acquire_timeout_seconds > 300
    {
        return Err(ConfigError::InvalidDatabasePool);
    }

    let user_id = parse_uuid(values, "DAYWEAVE_DEFAULT_USER_ID", DEFAULT_USER_ID)?;
    let workspace_id = parse_uuid(
        values,
        "DAYWEAVE_DEFAULT_WORKSPACE_ID",
        DEFAULT_WORKSPACE_ID,
    )?;
    if user_id == workspace_id {
        return Err(ConfigError::InvalidDatabaseScope);
    }
    let timezone_name = values
        .get("DAYWEAVE_DEFAULT_TIMEZONE")
        .map_or("UTC", String::as_str);
    timezone_name
        .parse::<chrono_tz::Tz>()
        .map_err(|_| ConfigError::InvalidTimezone)?;
    let owner_subject = values
        .get("DAYWEAVE_OWNER_SUBJECT")
        .map_or("personal-owner", String::as_str)
        .trim();
    if owner_subject.is_empty() || owner_subject.chars().count() > 500 {
        return Err(ConfigError::InvalidOwnerSubject);
    }

    Ok(Some(DatabaseConfig {
        url: DatabaseUrl(raw_url.clone()),
        max_connections,
        min_connections,
        acquire_timeout: Duration::from_secs(u64::from(acquire_timeout_seconds)),
        user_id,
        workspace_id,
        owner_subject: owner_subject.to_owned(),
        timezone_name: timezone_name.to_owned(),
    }))
}

fn parse_database_value(
    values: &HashMap<String, String>,
    key: &str,
    default: u32,
) -> Result<u32, ConfigError> {
    values.get(key).map_or(Ok(default), |value| {
        value.parse().map_err(|_| ConfigError::InvalidDatabasePool)
    })
}

fn parse_uuid(
    values: &HashMap<String, String>,
    key: &str,
    default: &str,
) -> Result<Uuid, ConfigError> {
    Uuid::parse_str(values.get(key).map_or(default, String::as_str))
        .map_err(|_| ConfigError::InvalidDatabaseScope)
}

fn validate_origin(origin: &str) -> Result<String, ConfigError> {
    let valid = origin.split_once("://").is_some_and(|(scheme, authority)| {
        matches!(scheme, "http" | "https")
            && !authority.is_empty()
            && !authority.contains('/')
            && !authority.chars().any(char::is_whitespace)
    });
    if valid {
        Ok(origin.to_owned())
    } else {
        Err(ConfigError::InvalidMcpOrigin(origin.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_values() -> HashMap<String, String> {
        HashMap::from([(
            "DAYWEAVE_API_TOKENS".to_owned(),
            "a-secure-development-token-123".to_owned(),
        )])
    }

    #[test]
    fn defaults_are_safe_and_predictable() {
        let config = Config::from_map(&valid_values()).expect("valid config");

        assert_eq!(config.bind_address.port(), 8080);
        assert_eq!(config.environment, Environment::Development);
        assert_eq!(config.auth_mode, AuthMode::LegacyStatic);
        assert_eq!(config.proposal_ttl, Duration::from_hours(7 * 24));
        assert_eq!(config.api_token_hashes.len(), 1);
        assert!(config.mcp_allowed_origins.is_empty());
        assert!(config.google_oauth.is_none());
    }

    #[test]
    fn rejects_missing_and_short_tokens() {
        assert_eq!(
            Config::from_map(&HashMap::new()).expect_err("missing token must fail"),
            ConfigError::MissingApiTokens
        );
        assert_eq!(
            Config::from_map(&HashMap::from([(
                "DAYWEAVE_API_TOKENS".to_owned(),
                "short".to_owned()
            )]))
            .expect_err("short token must fail"),
            ConfigError::ApiTokenTooShort(MINIMUM_TOKEN_LENGTH)
        );
        assert_eq!(
            Config::from_map(&HashMap::from([(
                "DAYWEAVE_API_TOKEN".to_owned(),
                "dw_reserved-static-token-that-is-long-enough".to_owned(),
            )]))
            .expect_err("reserved prefixes cannot enter static authority"),
            ConfigError::ReservedApiTokenPrefix
        );
    }

    #[test]
    fn validates_authentication_rollout_modes() {
        let mut hybrid = valid_values();
        hybrid.insert("DAYWEAVE_AUTH_MODE".to_owned(), "hybrid".to_owned());
        assert_eq!(
            Config::from_map(&hybrid).expect_err("hybrid requires durable storage"),
            ConfigError::AuthModeRequiresDatabase
        );
        hybrid.insert(
            "DAYWEAVE_DATABASE_URL".to_owned(),
            "postgres://dayweave:redacted@database/dayweave".to_owned(),
        );
        assert_eq!(
            Config::from_map(&hybrid).expect("hybrid config").auth_mode,
            AuthMode::Hybrid
        );

        let credential_only = HashMap::from([
            (
                "DAYWEAVE_AUTH_MODE".to_owned(),
                "credential_only".to_owned(),
            ),
            (
                "DAYWEAVE_DATABASE_URL".to_owned(),
                "postgres://dayweave:redacted@database/dayweave".to_owned(),
            ),
        ]);
        let config = Config::from_map(&credential_only).expect("credential-only config");
        assert_eq!(config.auth_mode, AuthMode::CredentialOnly);
        assert!(config.api_token_hashes.is_empty());

        let mut forbidden = credential_only;
        forbidden.insert(
            "DAYWEAVE_API_TOKEN".to_owned(),
            "a-static-token-that-must-not-remain".to_owned(),
        );
        assert_eq!(
            Config::from_map(&forbidden).expect_err("credential-only rejects fallback"),
            ConfigError::StaticTokensForbidden
        );
        forbidden.insert("DAYWEAVE_API_TOKENS".to_owned(), String::new());
        assert_eq!(
            Config::from_map(&forbidden)
                .expect_err("an empty preferred variable cannot hide a legacy token"),
            ConfigError::StaticTokensForbidden
        );
    }

    #[test]
    fn parses_explicit_settings_and_multiple_tokens() {
        let mut values = valid_values();
        values.insert("DAYWEAVE_ENVIRONMENT".to_owned(), "production".to_owned());
        values.insert(
            "DAYWEAVE_DATABASE_URL".to_owned(),
            "postgres://dayweave:secret@database/dayweave".to_owned(),
        );
        values.insert(
            "DAYWEAVE_BIND_ADDRESS".to_owned(),
            "127.0.0.1:9123".to_owned(),
        );
        values.insert(
            "DAYWEAVE_API_TOKENS".to_owned(),
            "first-development-token-12345, second-development-token-1234".to_owned(),
        );
        values.insert("DAYWEAVE_PROPOSAL_TTL_HOURS".to_owned(), "24".to_owned());
        values.insert("DAYWEAVE_JSON_LOGS".to_owned(), "true".to_owned());
        values.insert(
            "DAYWEAVE_MCP_ALLOWED_ORIGINS".to_owned(),
            "https://chatgpt.com,https://example.test:8443".to_owned(),
        );

        let config = Config::from_map(&values).expect("valid config");

        assert_eq!(config.environment, Environment::Production);
        assert_eq!(config.bind_address, "127.0.0.1:9123".parse().unwrap());
        assert_eq!(config.api_token_hashes.len(), 2);
        assert_eq!(config.proposal_ttl, Duration::from_hours(24));
        assert!(config.json_logs);
        assert_eq!(config.mcp_allowed_origins.len(), 2);
        let database = config.database.expect("database config");
        assert_eq!(database.max_connections, 10);
        assert_eq!(database.timezone_name, "UTC");
        assert!(!format!("{database:?}").contains("secret"));
    }

    #[test]
    fn accepts_deployment_variable_aliases_and_bounds_ttl() {
        let values = HashMap::from([
            (
                "DAYWEAVE_API_TOKEN".to_owned(),
                "a-secure-deployment-token-123".to_owned(),
            ),
            ("DAYWEAVE_BIND".to_owned(), "0.0.0.0:8787".to_owned()),
        ]);
        let config = Config::from_map(&values).expect("deployment aliases");
        assert_eq!(config.bind_address, "0.0.0.0:8787".parse().unwrap());

        let mut invalid = values;
        invalid.insert(
            "DAYWEAVE_PROPOSAL_TTL_HOURS".to_owned(),
            (MAX_PROPOSAL_TTL_HOURS + 1).to_string(),
        );
        assert!(matches!(
            Config::from_map(&invalid),
            Err(ConfigError::InvalidProposalTtl(_))
        ));
    }

    #[test]
    fn requires_database_in_deployed_environments_and_validates_scope() {
        let mut production = valid_values();
        production.insert("DAYWEAVE_ENVIRONMENT".to_owned(), "production".to_owned());
        assert_eq!(
            Config::from_map(&production).expect_err("database is mandatory"),
            ConfigError::MissingDatabaseUrl
        );

        production.insert(
            "DAYWEAVE_DATABASE_URL".to_owned(),
            "postgres://dayweave:do-not-print@database/dayweave".to_owned(),
        );
        production.insert(
            "DAYWEAVE_DEFAULT_TIMEZONE".to_owned(),
            "Europe/Madrid".to_owned(),
        );
        let config = Config::from_map(&production).expect("valid database config");
        let debug = format!("{config:?}");
        assert!(!debug.contains("do-not-print"));
        assert_eq!(config.database.unwrap().timezone_name, "Europe/Madrid");

        production.insert(
            "DAYWEAVE_DEFAULT_TIMEZONE".to_owned(),
            "not/a/timezone".to_owned(),
        );
        assert_eq!(
            Config::from_map(&production).expect_err("invalid timezone"),
            ConfigError::InvalidTimezone
        );
    }

    #[test]
    fn google_oauth_is_explicit_strict_and_redacted() {
        let mut disabled = valid_values();
        disabled.insert(
            "DAYWEAVE_GOOGLE_CLIENT_ID".to_owned(),
            "ignored-client-id".to_owned(),
        );
        assert_eq!(
            Config::from_map(&disabled).expect_err("partial disabled settings must fail"),
            ConfigError::DisabledGoogleOAuthConfiguration
        );

        let mut enabled = valid_values();
        enabled.extend([
            (
                "DAYWEAVE_GOOGLE_OAUTH_ENABLED".to_owned(),
                "true".to_owned(),
            ),
            (
                "DAYWEAVE_GOOGLE_CLIENT_ID".to_owned(),
                "client.apps.googleusercontent.com".to_owned(),
            ),
            (
                "DAYWEAVE_GOOGLE_CLIENT_SECRET".to_owned(),
                "never-print-this-client-secret".to_owned(),
            ),
            (
                "DAYWEAVE_GOOGLE_REDIRECT_URI".to_owned(),
                "https://api.example.test/v1/integrations/google/oauth/callback".to_owned(),
            ),
            (
                "DAYWEAVE_GOOGLE_CREDENTIAL_KEYS".to_owned(),
                format!("v1:{},v2:{}", "11".repeat(32), "22".repeat(32)),
            ),
            (
                "DAYWEAVE_GOOGLE_ACTIVE_CREDENTIAL_KEY_VERSION".to_owned(),
                "v2".to_owned(),
            ),
        ]);
        assert_eq!(
            Config::from_map(&enabled).expect_err("OAuth credentials require durable storage"),
            ConfigError::MissingGoogleOAuthDatabase
        );
        enabled.insert(
            "DAYWEAVE_DATABASE_URL".to_owned(),
            "postgres://dayweave:redacted@db/dayweave".to_owned(),
        );
        let config = Config::from_map(&enabled).expect("complete OAuth config");
        let google = config.google_oauth.expect("enabled");
        assert_eq!(google.active_key_version, 2);
        assert_eq!(google.keys.len(), 2);
        let debug = format!("{google:?}");
        assert!(!debug.contains("never-print-this-client-secret"));
        assert!(!debug.contains(&"11".repeat(32)));
    }

    #[test]
    fn google_redirect_and_key_formats_fail_closed() {
        let base = |redirect: &str| {
            let mut values = valid_values();
            values.extend([
                (
                    "DAYWEAVE_DATABASE_URL".to_owned(),
                    "postgres://dayweave:redacted@db/dayweave".to_owned(),
                ),
                (
                    "DAYWEAVE_GOOGLE_OAUTH_ENABLED".to_owned(),
                    "true".to_owned(),
                ),
                ("DAYWEAVE_GOOGLE_CLIENT_ID".to_owned(), "client".to_owned()),
                (
                    "DAYWEAVE_GOOGLE_CLIENT_SECRET".to_owned(),
                    "secret".to_owned(),
                ),
                (
                    "DAYWEAVE_GOOGLE_REDIRECT_URI".to_owned(),
                    redirect.to_owned(),
                ),
                (
                    "DAYWEAVE_GOOGLE_CREDENTIAL_KEYS".to_owned(),
                    format!("v1:{}", "ab".repeat(32)),
                ),
                (
                    "DAYWEAVE_GOOGLE_ACTIVE_CREDENTIAL_KEY_VERSION".to_owned(),
                    "v1".to_owned(),
                ),
            ]);
            values
        };

        assert_eq!(
            Config::from_map(&base(
                "http://127.0.0.1:8080/v1/integrations/google/oauth/callback",
            ))
            .expect_err("loopback HTTP must fail in development"),
            ConfigError::InvalidGoogleRedirectUri
        );
        let mut production = base("http://localhost:8080/v1/integrations/google/oauth/callback");
        production.insert("DAYWEAVE_ENVIRONMENT".to_owned(), "production".to_owned());
        production.insert(
            "DAYWEAVE_DATABASE_URL".to_owned(),
            "postgres://dayweave:redacted@db/dayweave".to_owned(),
        );
        assert_eq!(
            Config::from_map(&production).expect_err("production HTTP must fail"),
            ConfigError::InvalidGoogleRedirectUri
        );
        production.insert(
            "DAYWEAVE_GOOGLE_REDIRECT_URI".to_owned(),
            "https://localhost/v1/integrations/google/oauth/callback".to_owned(),
        );
        Config::from_map(&production).expect("HTTPS loopback remains encrypted");
        assert_eq!(
            Config::from_map(&base(
                "http://localhost.evil.test/v1/integrations/google/oauth/callback",
            ))
            .expect_err("lookalike loopback must fail"),
            ConfigError::InvalidGoogleRedirectUri
        );

        let mut ambiguous = base("https://api.example.test/v1/integrations/google/oauth/callback");
        ambiguous.insert(
            "DAYWEAVE_GOOGLE_CREDENTIAL_KEYS".to_owned(),
            "v1:YWJjZA==".to_owned(),
        );
        assert_eq!(
            Config::from_map(&ambiguous).expect_err("base64 is not accepted as hex"),
            ConfigError::InvalidGoogleCredentialKeys
        );

        assert_eq!(parse_key_version("v2147483647"), Some(2_147_483_647));
        for invalid in ["v2147483648", "v4294967295", "v+1", "v01", "v0", "v", "1"] {
            assert_eq!(parse_key_version(invalid), None, "{invalid}");
        }
    }
}
