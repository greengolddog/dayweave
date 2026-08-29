use std::io::ErrorKind;

/// Errors deliberately omit server-provided text and request contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid Codex App Server configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("failed to spawn the Codex App Server ({kind:?})")]
    Spawn { kind: ErrorKind },
    #[error("Codex App Server transport failed ({kind:?})")]
    Transport { kind: ErrorKind },
    #[error("Codex App Server operation timed out")]
    Timeout,
    #[error("outbound App Server message exceeds the configured limit")]
    RequestTooLarge,
    #[error("inbound App Server line exceeds the configured limit")]
    ResponseTooLarge,
    #[error("prompt exceeds the configured limit")]
    PromptTooLarge,
    #[error("invalid App Server message")]
    InvalidMessage,
    #[error("App Server response did not match the outstanding request")]
    UnexpectedResponseId,
    #[error("App Server rejected the request with code {code}")]
    Rpc { code: i64 },
    #[error("Codex App Server exited (code {code:?})")]
    ProcessExited { code: Option<i32> },
    #[error("the App Server connection is no longer usable")]
    ConnectionUnusable,
    #[error(
        "no supported Codex runtime is available: exact schema/content pinning and descendant containment are not established"
    )]
    NoSupportedRuntime,
    #[error("too many pending App Server notifications")]
    NotificationOverflow,
    #[error("queued App Server data exceeds the configured aggregate limit")]
    QueuedDataOverflow,
    #[error("the App Server reported a different CODEX_HOME")]
    CodexHomeMismatch,
    #[error("Codex authentication failed")]
    AuthenticationFailed,
    #[error("the structured turn was interrupted")]
    TurnInterrupted,
    #[error("the structured turn failed")]
    TurnFailed,
    #[error("the completed turn did not contain a final agent message")]
    MissingStructuredOutput,
    #[error("the final agent message did not match the requested output type")]
    InvalidStructuredOutput,
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn transport_error(error: &std::io::Error) -> Error {
    Error::Transport { kind: error.kind() }
}
