//! A narrow, fail-closed scaffold for the experimental Codex App Server protocol.
//!
//! Production process startup is deliberately disabled. It can be enabled only
//! after one exact executable and its complete generated schema bundle are
//! content-pinned and macOS descendant containment is proven. The internal test
//! transport exists only to exercise fail-closed protocol and cleanup behavior.

#[cfg(not(unix))]
compile_error!("dayweave-codex supports Unix process containment only");

mod client;
mod config;
mod error;
mod process;
mod protocol;
mod types;

pub use client::CodexAppServer;
pub use config::{AllowedEnvironment, CodexAppServerConfig, EnvironmentKey, ProtocolLimits};
pub use error::{Error, Result};
pub use types::{
    Account, AccountState, BedrockCredentialSource, BrowserLogin, DeviceCodeLogin, ServerInfo,
    StructuredTurn, StructuredTurnRequest, ThreadHandle, ThreadOptions,
};
