//! `DayWeave`'s private HTTP API.
//!
//! The crate deliberately separates HTTP, application services, repositories,
//! and external-service ports. The first milestone uses an in-memory proposal
//! repository for isolated tests and a durable `PostgreSQL` adapter in deployed
//! environments without changing the HTTP contract.

pub mod auth;
pub mod config;
pub mod error;
pub mod healthcheck;
pub mod http;
pub mod integrations;
pub mod mcp;
pub mod persistence;
pub mod proposals;
pub mod readiness;
pub mod scheduling;

use std::sync::Arc;

use auth::{Authenticator, StaticTokenAuthenticator};
use config::Config;
use mcp::McpService;
use persistence::{Database, PersistenceError, PostgresProposalRepository};
use proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock};
use readiness::Readiness;
use scheduling::{
    PlanningSimulationPort, ScheduleQueryPort, UnavailableScheduleQueryPort,
    UnavailableSimulationPort,
};

/// Shared dependencies used by HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub proposals: Arc<ProposalService>,
    pub authenticator: Arc<dyn Authenticator>,
    pub readiness: Readiness,
    pub mcp: Arc<McpService>,
}

impl AppState {
    /// Builds the current production dependency graph.
    ///
    /// `PostgreSQL` is mandatory in deployed environments and optional for local
    /// development. The in-memory adapter remains available for isolated tests.
    ///
    /// # Errors
    ///
    /// Returns a redacted persistence error if the configured database cannot
    /// connect, migrate, or initialize its personal workspace scope.
    pub async fn from_config(config: &Config) -> Result<Self, PersistenceError> {
        let (repository, readiness): (Arc<dyn ProposalRepository>, Readiness) =
            if let Some(database_config) = &config.database {
                let database = Database::connect(database_config).await?;
                (
                    Arc::new(PostgresProposalRepository::new(
                        database.pool().clone(),
                        database.scope(),
                    )),
                    Readiness::with_database(database.pool().clone()),
                )
            } else {
                (
                    Arc::new(InMemoryProposalRepository::default()),
                    Readiness::default(),
                )
            };
        let clock = Arc::new(SystemClock);
        let proposals = Arc::new(ProposalService::new(repository, clock, config.proposal_ttl));
        let authenticator = Arc::new(StaticTokenAuthenticator::from_hashes(
            config.api_token_hashes.clone(),
        ));
        let mcp = Arc::new(McpService::new(
            Arc::new(UnavailableScheduleQueryPort),
            Arc::new(UnavailableSimulationPort),
            proposals.clone(),
            config.mcp_allowed_origins.clone(),
        ));

        Ok(Self {
            proposals,
            authenticator,
            readiness,
            mcp,
        })
    }

    #[must_use]
    pub fn new(
        proposals: Arc<ProposalService>,
        authenticator: Arc<dyn Authenticator>,
        readiness: Readiness,
    ) -> Self {
        let mcp = Arc::new(McpService::new(
            Arc::new(UnavailableScheduleQueryPort),
            Arc::new(UnavailableSimulationPort),
            proposals.clone(),
            Arc::new(Vec::new()),
        ));
        Self {
            proposals,
            authenticator,
            readiness,
            mcp,
        }
    }

    #[must_use]
    pub fn with_mcp_ports(
        mut self,
        schedule: Arc<dyn ScheduleQueryPort>,
        simulations: Arc<dyn PlanningSimulationPort>,
        allowed_origins: Arc<Vec<String>>,
    ) -> Self {
        self.mcp = Arc::new(McpService::new(
            schedule,
            simulations,
            self.proposals.clone(),
            allowed_origins,
        ));
        self
    }
}
