use thiserror::Error;

#[derive(Debug, Error)]
pub enum GoogleError {
    #[error("Google API base URL is invalid")]
    InvalidBaseUrl,
    #[error("Google transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("Google credentials are missing or no longer authorized")]
    Unauthorized,
    #[error("Google incremental sync token expired; a bounded full sync is required")]
    SyncTokenExpired,
    #[error("Google resource changed since it was read")]
    PreconditionFailed,
    #[error("Google mutation requires a non-empty last-seen ETag")]
    ConditionalWriteRequired,
    #[error("Google mutation initiation authorization expired before network dispatch")]
    DispatchInitiationExpired,
    #[error("Google API rate limit reached")]
    RateLimited { retry_after_seconds: Option<u64> },
    #[error("Google API is temporarily unavailable ({status})")]
    Temporary { status: u16 },
    #[error("Google API rejected the request ({status})")]
    Api { status: u16 },
    #[error("invalid sync request: {0}")]
    InvalidSyncRequest(&'static str),
    #[error("explicit approval is required for an external calendar mutation")]
    ApprovalRequired,
    #[error("Google OAuth authorization state did not match")]
    OAuthStateMismatch,
    #[error("Google OAuth request is invalid: {0}")]
    InvalidOAuthRequest(&'static str),
    #[error("Google OAuth rejected the request ({code})")]
    OAuthRejected { code: String },
    #[error("secure random generation failed")]
    Randomness,
}
