//! `DayWeave`'s private HTTP API.
//!
//! The crate deliberately separates HTTP, application services, repositories,
//! and external-service ports. The first milestone uses an in-memory proposal
//! repository; a durable `PostgreSQL` adapter can replace it without changing the
//! HTTP contract.

pub mod auth;
pub mod config;
pub mod error;
pub mod healthcheck;
pub mod http;
pub mod integrations;
pub mod proposals;
pub mod readiness;

use std::sync::Arc;

use auth::{Authenticator, StaticTokenAuthenticator};
use config::Config;
use proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock};
use readiness::Readiness;

/// Shared dependencies used by HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub proposals: Arc<ProposalService>,
    pub authenticator: Arc<dyn Authenticator>,
    pub readiness: Readiness,
}

impl AppState {
    /// Builds the current production dependency graph.
    ///
    /// The in-memory repository is intentionally temporary. Durable adapters
    /// will be selected here once `PostgreSQL` configuration is introduced.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let repository: Arc<dyn ProposalRepository> =
            Arc::new(InMemoryProposalRepository::default());
        let clock = Arc::new(SystemClock);
        let proposals = Arc::new(ProposalService::new(repository, clock, config.proposal_ttl));
        let authenticator = Arc::new(StaticTokenAuthenticator::from_hashes(
            config.api_token_hashes.clone(),
        ));

        Self {
            proposals,
            authenticator,
            readiness: Readiness::default(),
        }
    }

    #[must_use]
    pub fn new(
        proposals: Arc<ProposalService>,
        authenticator: Arc<dyn Authenticator>,
        readiness: Readiness,
    ) -> Self {
        Self {
            proposals,
            authenticator,
            readiness,
        }
    }
}
