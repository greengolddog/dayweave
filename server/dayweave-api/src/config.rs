use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use thiserror::Error;
use uuid::Uuid;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Development,
    Test,
    Staging,
    Production,
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
    pub api_token_hashes: Arc<Vec<TokenHash>>,
    pub proposal_ttl: Duration,
    pub mcp_allowed_origins: Arc<Vec<String>>,
    pub database: Option<DatabaseConfig>,
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
    pub fn from_map(values: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let environment = values
            .get("DAYWEAVE_ENVIRONMENT")
            .map_or(Ok(Environment::Development), |value| value.parse())?;

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
            .or_else(|| values.get("DAYWEAVE_API_TOKEN"))
            .ok_or(ConfigError::MissingApiTokens)?;
        let tokens: Vec<_> = raw_tokens
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect();
        if tokens.is_empty() {
            return Err(ConfigError::MissingApiTokens);
        }
        if tokens
            .iter()
            .any(|token| token.len() < MINIMUM_TOKEN_LENGTH)
        {
            return Err(ConfigError::ApiTokenTooShort(MINIMUM_TOKEN_LENGTH));
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

        Ok(Self {
            bind_address,
            environment,
            api_token_hashes,
            proposal_ttl: Duration::from_secs(ttl_seconds),
            mcp_allowed_origins,
            database,
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
        assert_eq!(config.proposal_ttl, Duration::from_hours(7 * 24));
        assert_eq!(config.api_token_hashes.len(), 1);
        assert!(config.mcp_allowed_origins.is_empty());
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
}
