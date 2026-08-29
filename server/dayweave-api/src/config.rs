use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use secrecy::SecretString;
use thiserror::Error;
use url::{Host, Url};
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
const DEFAULT_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES: u64 = 10;
const MAX_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES: u64 = 30;
const MAX_MCP_OAUTH_CLIENT_IDS: usize = 32;
const MAX_MCP_OAUTH_VALUE_LENGTH: usize = 2_048;

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
    /// Pinned HMAC root for provider identities. Unlike the active encryption
    /// key, this version must be retained for the lifetime of published items.
    pub identity_key_version: u32,
    pub session_ttl: Duration,
}

/// Disabled-by-default Auth0 resource-server policy for published MCP clients.
///
/// This contains only public identifiers and allowlists. Auth0 remains the
/// authorization server; `DayWeave` never receives a client secret through this
/// configuration surface.
#[derive(Clone)]
pub struct McpOAuthConfig {
    pub resource: Url,
    pub issuer: Url,
    pub jwks_uri: Url,
    pub resource_metadata_uri: Url,
    pub owner_subject: String,
    pub allowed_client_ids: Arc<Vec<String>>,
    pub allowed_origins: Arc<Vec<String>>,
    pub user_id: Uuid,
    pub workspace_id: Uuid,
}

impl std::fmt::Debug for McpOAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthConfig")
            .field("resource", &self.resource.as_str())
            .field("issuer", &self.issuer.as_str())
            .field("jwks_uri", &self.jwks_uri.as_str())
            .field(
                "resource_metadata_uri",
                &self.resource_metadata_uri.as_str(),
            )
            .field("owner_subject", &"[REDACTED]")
            .field("allowed_client_ids_count", &self.allowed_client_ids.len())
            .field("allowed_origins_count", &self.allowed_origins.len())
            .field("user_id", &"[REDACTED]")
            .field("workspace_id", &"[REDACTED]")
            .finish()
    }
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
            .field("identity_key_version", &self.identity_key_version)
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
    pub mcp_oauth: Option<McpOAuthConfig>,
    pub database: Option<DatabaseConfig>,
    pub google_oauth: Option<GoogleOAuthConfig>,
    /// External Google writes require both this explicit deployment opt-in and
    /// a consumed, content-bound approval capability. The safe default is off.
    pub google_outbound_enabled: bool,
    pub google_outbound_approval_ttl: Duration,
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
        let mcp_oauth = mcp_oauth_config(
            values,
            auth_mode,
            database.as_ref(),
            mcp_allowed_origins.as_ref(),
        )?;
        if auth_mode != AuthMode::LegacyStatic && database.is_none() {
            return Err(ConfigError::AuthModeRequiresDatabase);
        }
        let google_oauth = google_oauth_config(values)?;
        if google_oauth.is_some() && database.is_none() {
            return Err(ConfigError::MissingGoogleOAuthDatabase);
        }
        let google_outbound_enabled = match values
            .get("DAYWEAVE_GOOGLE_OUTBOUND_ENABLED")
            .map(String::as_str)
        {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(ConfigError::InvalidGoogleOutboundEnabled),
        };
        if google_outbound_enabled && google_oauth.is_none() {
            return Err(ConfigError::GoogleOutboundRequiresOAuth);
        }
        let google_outbound_approval_ttl_minutes = values
            .get("DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES")
            .map_or(Ok(DEFAULT_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES), |value| {
                value
                    .parse::<u64>()
                    .map_err(|_| ConfigError::InvalidGoogleOutboundApprovalTtl)
            })?;
        if google_outbound_approval_ttl_minutes == 0
            || google_outbound_approval_ttl_minutes > MAX_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES
        {
            return Err(ConfigError::InvalidGoogleOutboundApprovalTtl);
        }

        Ok(Self {
            bind_address,
            environment,
            auth_mode,
            api_token_hashes,
            proposal_ttl: Duration::from_secs(ttl_seconds),
            mcp_allowed_origins,
            mcp_oauth,
            database,
            google_oauth,
            google_outbound_enabled,
            google_outbound_approval_ttl: Duration::from_secs(
                google_outbound_approval_ttl_minutes * 60,
            ),
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
    #[error("DAYWEAVE_MCP_OAUTH_ENABLED must be true or false")]
    InvalidMcpOAuthEnabled,
    #[error("MCP OAuth settings were supplied while DAYWEAVE_MCP_OAUTH_ENABLED is not true")]
    DisabledMcpOAuthConfiguration,
    #[error("MCP OAuth requires credential_only authentication")]
    McpOAuthRequiresCredentialOnly,
    #[error("MCP OAuth requires DAYWEAVE_DATABASE_URL")]
    McpOAuthRequiresDatabase,
    #[error("MCP OAuth is enabled but a required setting is missing: {0}")]
    MissingMcpOAuthSetting(&'static str),
    #[error("DAYWEAVE_MCP_OAUTH_RESOURCE must be the exact public HTTPS /mcp URL")]
    InvalidMcpOAuthResource,
    #[error("DAYWEAVE_MCP_OAUTH_ISSUER must be an exact HTTPS Auth0 issuer ending in '/'")]
    InvalidMcpOAuthIssuer,
    #[error("DAYWEAVE_MCP_OAUTH_OWNER_SUBJECT is invalid")]
    InvalidMcpOAuthOwnerSubject,
    #[error("DAYWEAVE_MCP_OAUTH_CLIENT_IDS must contain unique bounded client identifiers")]
    InvalidMcpOAuthClientIds,
    #[error(
        "DAYWEAVE_MCP_OAUTH_ALLOWED_ORIGINS must be exact HTTPS origins from DAYWEAVE_MCP_ALLOWED_ORIGINS"
    )]
    InvalidMcpOAuthOrigins,
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
    #[error(
        "DAYWEAVE_GOOGLE_IDENTITY_KEY_VERSION must name a permanently retained configured key as vN"
    )]
    InvalidGoogleIdentityKeyVersion,
    #[error("invalid DAYWEAVE_GOOGLE_OAUTH_SESSION_TTL_MINUTES")]
    InvalidGoogleOAuthSessionTtl,
    #[error("DAYWEAVE_GOOGLE_OUTBOUND_ENABLED must be true or false")]
    InvalidGoogleOutboundEnabled,
    #[error("Google outbound publication requires Google OAuth")]
    GoogleOutboundRequiresOAuth,
    #[error("invalid DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES")]
    InvalidGoogleOutboundApprovalTtl,
}

fn mcp_oauth_config(
    values: &HashMap<String, String>,
    auth_mode: AuthMode,
    database: Option<&DatabaseConfig>,
    global_allowed_origins: &[String],
) -> Result<Option<McpOAuthConfig>, ConfigError> {
    let enabled = match values.get("DAYWEAVE_MCP_OAUTH_ENABLED").map(String::as_str) {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(ConfigError::InvalidMcpOAuthEnabled),
    };
    let oauth_settings_present = values
        .keys()
        .any(|key| key.starts_with("DAYWEAVE_MCP_OAUTH_") && key != "DAYWEAVE_MCP_OAUTH_ENABLED");
    if !enabled {
        if oauth_settings_present {
            return Err(ConfigError::DisabledMcpOAuthConfiguration);
        }
        return Ok(None);
    }
    if auth_mode != AuthMode::CredentialOnly {
        return Err(ConfigError::McpOAuthRequiresCredentialOnly);
    }
    let database = database.ok_or(ConfigError::McpOAuthRequiresDatabase)?;
    let required = |key: &'static str| {
        values
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or(ConfigError::MissingMcpOAuthSetting(key))
    };

    let resource = parse_mcp_oauth_resource(required("DAYWEAVE_MCP_OAUTH_RESOURCE")?)?;
    let issuer = parse_mcp_oauth_issuer(required("DAYWEAVE_MCP_OAUTH_ISSUER")?)?;
    let jwks_uri = issuer
        .join(".well-known/jwks.json")
        .map_err(|_| ConfigError::InvalidMcpOAuthIssuer)?;
    let mut resource_metadata_uri = resource.clone();
    resource_metadata_uri.set_path("/.well-known/oauth-protected-resource/mcp");

    let owner_subject = required("DAYWEAVE_MCP_OAUTH_OWNER_SUBJECT")?;
    if owner_subject.len() > 255
        || !owner_subject.is_ascii()
        || owner_subject.chars().any(char::is_whitespace)
        || owner_subject.chars().any(char::is_control)
    {
        return Err(ConfigError::InvalidMcpOAuthOwnerSubject);
    }

    let allowed_client_ids = parse_mcp_oauth_list(
        required("DAYWEAVE_MCP_OAUTH_CLIENT_IDS")?,
        MAX_MCP_OAUTH_CLIENT_IDS,
        MAX_MCP_OAUTH_VALUE_LENGTH,
    )
    .map_err(|()| ConfigError::InvalidMcpOAuthClientIds)?;
    let allowed_origins =
        values
            .get("DAYWEAVE_MCP_OAUTH_ALLOWED_ORIGINS")
            .map_or(Ok(Vec::new()), |raw| {
                if raw.trim().is_empty() {
                    return Ok(Vec::new());
                }
                let origins = parse_mcp_oauth_list(raw, MAX_MCP_OAUTH_CLIENT_IDS, 512)
                    .map_err(|()| ConfigError::InvalidMcpOAuthOrigins)?;
                if origins.iter().any(|origin| {
                    !is_exact_https_origin(origin) || !global_allowed_origins.contains(origin)
                }) {
                    return Err(ConfigError::InvalidMcpOAuthOrigins);
                }
                Ok(origins)
            })?;

    Ok(Some(McpOAuthConfig {
        resource,
        issuer,
        jwks_uri,
        resource_metadata_uri,
        owner_subject: owner_subject.to_owned(),
        allowed_client_ids: Arc::new(allowed_client_ids),
        allowed_origins: Arc::new(allowed_origins),
        user_id: database.user_id,
        workspace_id: database.workspace_id,
    }))
}

fn parse_mcp_oauth_resource(value: &str) -> Result<Url, ConfigError> {
    let resource = Url::parse(value).map_err(|_| ConfigError::InvalidMcpOAuthResource)?;
    if resource.as_str() != value
        || resource.scheme() != "https"
        || resource.username() != ""
        || resource.password().is_some()
        || resource.host_str().is_none()
        || resource.query().is_some()
        || resource.fragment().is_some()
        || resource.path() != "/mcp"
    {
        return Err(ConfigError::InvalidMcpOAuthResource);
    }
    Ok(resource)
}

fn parse_mcp_oauth_issuer(value: &str) -> Result<Url, ConfigError> {
    let issuer = Url::parse(value).map_err(|_| ConfigError::InvalidMcpOAuthIssuer)?;
    let public_domain = match issuer.host() {
        Some(Host::Domain(host)) => {
            host.contains('.')
                && !host.split('.').next_back().is_some_and(|label| {
                    label.eq_ignore_ascii_case("localhost") || label.eq_ignore_ascii_case("local")
                })
        }
        Some(Host::Ipv4(_) | Host::Ipv6(_)) | None => false,
    };
    if issuer.as_str() != value
        || !value.ends_with('/')
        || issuer.scheme() != "https"
        || issuer.username() != ""
        || issuer.password().is_some()
        || issuer.host_str().is_none()
        || issuer.query().is_some()
        || issuer.fragment().is_some()
        || issuer.path() != "/"
        || issuer.port().is_some()
        || !public_domain
    {
        return Err(ConfigError::InvalidMcpOAuthIssuer);
    }
    Ok(issuer)
}

fn parse_mcp_oauth_list(
    value: &str,
    maximum_entries: usize,
    maximum_entry_length: usize,
) -> Result<Vec<String>, ()> {
    let entries = value.split(',').map(str::trim).collect::<Vec<_>>();
    if entries.is_empty() || entries.len() > maximum_entries {
        return Err(());
    }
    let mut unique = BTreeSet::new();
    for entry in entries {
        if entry.is_empty()
            || entry.len() > maximum_entry_length
            || !entry.is_ascii()
            || entry.chars().any(char::is_whitespace)
            || entry.chars().any(char::is_control)
            || !unique.insert(entry.to_owned())
        {
            return Err(());
        }
    }
    Ok(unique.into_iter().collect())
}

fn is_exact_https_origin(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.as_str() == format!("{value}/")
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
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
    let google_settings_present = values.keys().any(|key| {
        key.starts_with("DAYWEAVE_GOOGLE_")
            && !matches!(
                key.as_str(),
                "DAYWEAVE_GOOGLE_OAUTH_ENABLED"
                    | "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED"
                    | "DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES"
            )
    });
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
    let identity_key_version = parse_key_version(required("DAYWEAVE_GOOGLE_IDENTITY_KEY_VERSION")?)
        .ok_or(ConfigError::InvalidGoogleIdentityKeyVersion)?;
    if !keys.contains_key(&identity_key_version) {
        return Err(ConfigError::InvalidGoogleIdentityKeyVersion);
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
        identity_key_version,
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
        assert!(config.mcp_oauth.is_none());
        assert!(config.google_oauth.is_none());
        assert!(!config.google_outbound_enabled);
        assert_eq!(config.google_outbound_approval_ttl, Duration::from_mins(10));
    }

    fn valid_mcp_oauth_values() -> HashMap<String, String> {
        HashMap::from([
            (
                "DAYWEAVE_AUTH_MODE".to_owned(),
                "credential_only".to_owned(),
            ),
            (
                "DAYWEAVE_DATABASE_URL".to_owned(),
                "postgres://dayweave:redacted@database/dayweave".to_owned(),
            ),
            ("DAYWEAVE_MCP_OAUTH_ENABLED".to_owned(), "true".to_owned()),
            (
                "DAYWEAVE_MCP_OAUTH_RESOURCE".to_owned(),
                "https://api.example.test/mcp".to_owned(),
            ),
            (
                "DAYWEAVE_MCP_OAUTH_ISSUER".to_owned(),
                "https://tenant.eu.auth0.com/".to_owned(),
            ),
            (
                "DAYWEAVE_MCP_OAUTH_OWNER_SUBJECT".to_owned(),
                "auth0|personal-owner".to_owned(),
            ),
            (
                "DAYWEAVE_MCP_OAUTH_CLIENT_IDS".to_owned(),
                "https://chatgpt.com/oauth/client.json".to_owned(),
            ),
            (
                "DAYWEAVE_MCP_ALLOWED_ORIGINS".to_owned(),
                "https://chatgpt.com".to_owned(),
            ),
            (
                "DAYWEAVE_MCP_OAUTH_ALLOWED_ORIGINS".to_owned(),
                "https://chatgpt.com".to_owned(),
            ),
        ])
    }

    #[test]
    fn mcp_oauth_is_disabled_by_default_and_rejects_latent_configuration() {
        let mut disabled = valid_values();
        disabled.insert(
            "DAYWEAVE_MCP_OAUTH_RESOURCE".to_owned(),
            "https://api.example.test/mcp".to_owned(),
        );
        assert_eq!(
            Config::from_map(&disabled).expect_err("disabled settings must not be latent"),
            ConfigError::DisabledMcpOAuthConfiguration
        );

        let mut ambiguous = valid_values();
        ambiguous.insert("DAYWEAVE_MCP_OAUTH_ENABLED".to_owned(), "yes".to_owned());
        assert_eq!(
            Config::from_map(&ambiguous).expect_err("switch must be exact"),
            ConfigError::InvalidMcpOAuthEnabled
        );
    }

    #[test]
    fn mcp_oauth_requires_credential_only_and_durable_scope() {
        let mut legacy = valid_mcp_oauth_values();
        legacy.insert("DAYWEAVE_AUTH_MODE".to_owned(), "legacy_static".to_owned());
        legacy.insert(
            "DAYWEAVE_API_TOKEN".to_owned(),
            "a-secure-development-token-123".to_owned(),
        );
        assert_eq!(
            Config::from_map(&legacy).expect_err("legacy fallback must be impossible"),
            ConfigError::McpOAuthRequiresCredentialOnly
        );

        let mut no_database = valid_mcp_oauth_values();
        no_database.remove("DAYWEAVE_DATABASE_URL");
        assert_eq!(
            Config::from_map(&no_database).expect_err("durable identity is mandatory"),
            ConfigError::McpOAuthRequiresDatabase
        );
    }

    #[test]
    fn mcp_oauth_pins_exact_resource_issuer_clients_and_origin_intersection() {
        let config = Config::from_map(&valid_mcp_oauth_values()).expect("valid OAuth policy");
        let oauth = config.mcp_oauth.expect("OAuth enabled");
        assert_eq!(oauth.resource.as_str(), "https://api.example.test/mcp");
        assert_eq!(
            oauth.jwks_uri.as_str(),
            "https://tenant.eu.auth0.com/.well-known/jwks.json"
        );
        assert_eq!(
            oauth.resource_metadata_uri.as_str(),
            "https://api.example.test/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(
            oauth.allowed_client_ids.as_slice(),
            ["https://chatgpt.com/oauth/client.json"]
        );
        assert_eq!(oauth.allowed_origins.as_slice(), ["https://chatgpt.com"]);
        let debug = format!("{oauth:?}");
        assert!(debug.contains("https://api.example.test/mcp"));
        assert!(debug.contains("https://tenant.eu.auth0.com/"));
        assert!(debug.contains("allowed_client_ids_count: 1"));
        assert!(debug.contains("allowed_origins_count: 1"));
        for private_identifier in [
            "auth0|personal-owner",
            "https://chatgpt.com/oauth/client.json",
            "https://chatgpt.com\"",
            &oauth.user_id.to_string(),
            &oauth.workspace_id.to_string(),
        ] {
            assert!(
                !debug.contains(private_identifier),
                "OAuth Debug must redact identity and allowlist values"
            );
        }

        let mut bad_resource = valid_mcp_oauth_values();
        bad_resource.insert(
            "DAYWEAVE_MCP_OAUTH_RESOURCE".to_owned(),
            "https://api.example.test/mcp/".to_owned(),
        );
        assert_eq!(
            Config::from_map(&bad_resource).expect_err("resource is exact"),
            ConfigError::InvalidMcpOAuthResource
        );

        let mut bad_issuer = valid_mcp_oauth_values();
        bad_issuer.insert(
            "DAYWEAVE_MCP_OAUTH_ISSUER".to_owned(),
            "https://tenant.eu.auth0.com/oauth/".to_owned(),
        );
        assert_eq!(
            Config::from_map(&bad_issuer).expect_err("issuer is exact root"),
            ConfigError::InvalidMcpOAuthIssuer
        );

        for issuer in [
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "https://auth.local/",
            "https://single-label/",
            "https://tenant.eu.auth0.com:8443/",
        ] {
            let mut local_issuer = valid_mcp_oauth_values();
            local_issuer.insert("DAYWEAVE_MCP_OAUTH_ISSUER".to_owned(), issuer.to_owned());
            assert_eq!(
                Config::from_map(&local_issuer).expect_err("issuer must be a public HTTPS origin"),
                ConfigError::InvalidMcpOAuthIssuer,
                "{issuer}"
            );
        }

        let mut duplicate_client = valid_mcp_oauth_values();
        duplicate_client.insert(
            "DAYWEAVE_MCP_OAUTH_CLIENT_IDS".to_owned(),
            "client-a,client-a".to_owned(),
        );
        assert_eq!(
            Config::from_map(&duplicate_client).expect_err("client allowlist is strict"),
            ConfigError::InvalidMcpOAuthClientIds
        );

        let mut disjoint_origin = valid_mcp_oauth_values();
        disjoint_origin.insert(
            "DAYWEAVE_MCP_OAUTH_ALLOWED_ORIGINS".to_owned(),
            "https://evil.example".to_owned(),
        );
        assert_eq!(
            Config::from_map(&disjoint_origin).expect_err("both origin policies must allow"),
            ConfigError::InvalidMcpOAuthOrigins
        );
    }

    #[test]
    fn google_outbound_opt_in_and_approval_ttl_fail_closed() {
        let mut values = valid_values();
        values.insert(
            "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED".to_owned(),
            "yes".to_owned(),
        );
        assert_eq!(
            Config::from_map(&values).expect_err("ambiguous outbound switch must fail"),
            ConfigError::InvalidGoogleOutboundEnabled
        );

        values.insert(
            "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED".to_owned(),
            "true".to_owned(),
        );
        assert_eq!(
            Config::from_map(&values).expect_err("outbound requires configured OAuth"),
            ConfigError::GoogleOutboundRequiresOAuth
        );

        values.insert(
            "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED".to_owned(),
            "false".to_owned(),
        );
        for ttl in ["0", "31", "not-a-number"] {
            values.insert(
                "DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES".to_owned(),
                ttl.to_owned(),
            );
            assert_eq!(
                Config::from_map(&values).expect_err("approval TTL is bounded"),
                ConfigError::InvalidGoogleOutboundApprovalTtl
            );
        }
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
            (
                "DAYWEAVE_GOOGLE_IDENTITY_KEY_VERSION".to_owned(),
                "v1".to_owned(),
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
        enabled.insert(
            "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED".to_owned(),
            "true".to_owned(),
        );
        enabled.insert(
            "DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES".to_owned(),
            "30".to_owned(),
        );
        let config = Config::from_map(&enabled).expect("complete OAuth config");
        assert!(config.google_outbound_enabled);
        assert_eq!(config.google_outbound_approval_ttl, Duration::from_mins(30));
        let google = config.google_oauth.expect("enabled");
        assert_eq!(google.active_key_version, 2);
        assert_eq!(google.identity_key_version, 1);
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
                (
                    "DAYWEAVE_GOOGLE_IDENTITY_KEY_VERSION".to_owned(),
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

        let mut missing_identity =
            base("https://api.example.test/v1/integrations/google/oauth/callback");
        missing_identity.insert(
            "DAYWEAVE_GOOGLE_IDENTITY_KEY_VERSION".to_owned(),
            "v2".to_owned(),
        );
        assert_eq!(
            Config::from_map(&missing_identity)
                .expect_err("identity key must remain in the configured keyring"),
            ConfigError::InvalidGoogleIdentityKeyVersion
        );

        assert_eq!(parse_key_version("v2147483647"), Some(2_147_483_647));
        for invalid in ["v2147483648", "v4294967295", "v+1", "v01", "v0", "v", "1"] {
            assert_eq!(parse_key_version(invalid), None, "{invalid}");
        }
    }
}
