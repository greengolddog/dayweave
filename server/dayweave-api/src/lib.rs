//! `DayWeave`'s private HTTP API.
//!
//! The crate deliberately separates HTTP, application services, repositories,
//! and external-service ports. The first milestone uses an in-memory proposal
//! repository for isolated tests and a durable `PostgreSQL` adapter in deployed
//! environments without changing the HTTP contract.

pub mod auth;
pub mod config;
pub mod credential_auth;
pub mod error;
pub mod execution;
pub mod google_oauth;
pub mod google_sync;
pub mod healthcheck;
pub mod http;
pub mod integrations;
pub mod items;
pub mod mcp;
pub mod mcp_oauth;
pub mod persistence;
pub mod proposals;
pub mod readiness;
pub mod scheduling;

use std::sync::Arc;

use auth::{Authenticator, RuntimeAuthenticator, StaticTokenAuthenticator};
use config::{AuthMode, Config};
use credential_auth::CredentialRepository;
use dayweave_google::oauth::{OAuthClient, OAuthConfig};
use execution::{ExecutionRepository, ExecutionService, InMemoryExecutionRepository};
use google_oauth::{
    GoogleOAuthRepository, GoogleOAuthService, InMemoryGoogleOAuthRepository, OAuthScope,
    ProductionGoogleOAuthTransport, SecretCipher,
};
use google_sync::{GoogleSyncRepository, GoogleSyncService, ProductionGoogleSyncProvider};
use items::{InMemoryItemRepository, ItemRepository, ItemService};
use mcp::McpService;
use mcp_oauth::McpOAuthVerifier;
use persistence::{
    Database, PersistenceError, PostgresCredentialRepository, PostgresExecutionRepository,
    PostgresGoogleOAuthRepository, PostgresGoogleSyncRepository, PostgresItemRepository,
    PostgresProposalApplicationRepository, PostgresProposalRepository,
};
use proposals::{
    Clock, InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock,
};
use readiness::Readiness;
use scheduling::{
    PlanningSimulationPort, PostgresSchedulingRepository, ScheduleQueryPort,
    UnavailableScheduleQueryPort, UnavailableSimulationPort,
};
use uuid::Uuid;

type Repositories = (
    Arc<dyn ProposalRepository>,
    Arc<dyn ItemRepository>,
    Arc<dyn ExecutionRepository>,
    Arc<dyn GoogleOAuthRepository>,
    Option<Arc<dyn GoogleSyncRepository>>,
    Option<Arc<dyn CredentialRepository>>,
    Option<Arc<PostgresProposalApplicationRepository>>,
    Option<Arc<PostgresSchedulingRepository>>,
    OAuthScope,
    Readiness,
);

async fn repositories(config: &Config) -> Result<Repositories, PersistenceError> {
    if let Some(database_config) = &config.database {
        let database = Database::connect(database_config).await?;
        let scheduling = Arc::new(PostgresSchedulingRepository::new(
            database.pool().clone(),
            database.scope(),
        ));
        scheduling
            .maintain_simulation_retention()
            .await
            .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
        let proposal_applications = Arc::new(PostgresProposalApplicationRepository::new(
            database.pool().clone(),
            database.scope(),
        ));
        proposal_applications
            .maintain_retention()
            .await
            .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
        return Ok((
            Arc::new(PostgresProposalRepository::new(
                database.pool().clone(),
                database.scope(),
            )),
            Arc::new(PostgresItemRepository::new(
                database.pool().clone(),
                database.scope(),
            )),
            Arc::new(PostgresExecutionRepository::new(
                database.pool().clone(),
                database.scope(),
            )),
            Arc::new(PostgresGoogleOAuthRepository::new(
                database.pool().clone(),
                database.scope(),
            )),
            Some(Arc::new(PostgresGoogleSyncRepository::new(
                database.pool().clone(),
                database.scope(),
            ))),
            Some(Arc::new(PostgresCredentialRepository::new(
                database.pool().clone(),
                database.scope(),
            ))),
            Some(proposal_applications),
            Some(scheduling),
            OAuthScope {
                workspace_id: database.scope().workspace_id,
                user_id: database.scope().user_id,
            },
            Readiness::with_database(
                database.pool().clone(),
                database.scope().workspace_id,
                database.scope().user_id,
            ),
        ));
    }
    Ok((
        Arc::new(InMemoryProposalRepository::default()),
        Arc::new(InMemoryItemRepository::default()),
        Arc::new(InMemoryExecutionRepository::default()),
        Arc::new(InMemoryGoogleOAuthRepository::default()),
        None,
        None,
        None,
        None,
        OAuthScope {
            workspace_id: Uuid::from_u128(2),
            user_id: Uuid::from_u128(1),
        },
        Readiness::default(),
    ))
}

/// Shared dependencies used by HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub proposals: Arc<ProposalService>,
    pub items: Arc<ItemService>,
    pub execution: Arc<ExecutionService>,
    pub authenticator: Arc<dyn Authenticator>,
    pub credential_repository: Option<Arc<dyn CredentialRepository>>,
    pub auth_mode: AuthMode,
    pub readiness: Readiness,
    pub mcp: Arc<McpService>,
    pub mcp_oauth: Option<Arc<McpOAuthVerifier>>,
    pub google_oauth: Option<Arc<GoogleOAuthService>>,
    pub(crate) google_sync: Option<Arc<GoogleSyncService>>,
    pub(crate) proposal_applications: Option<Arc<PostgresProposalApplicationRepository>>,
    pub(crate) scheduling: Option<Arc<PostgresSchedulingRepository>>,
    execution_repository: Arc<dyn ExecutionRepository>,
    pub(crate) clock: Arc<dyn Clock>,
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
    #[allow(clippy::too_many_lines)] // Constructs and recovers the complete fail-closed dependency graph.
    pub async fn from_config(config: &Config) -> Result<Self, PersistenceError> {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let (
            repository,
            item_repository,
            execution_repository,
            google_oauth_repository,
            google_sync_repository,
            credential_repository,
            proposal_applications,
            scheduling,
            oauth_scope,
            readiness,
        ): Repositories = repositories(config).await?;
        let proposals = Arc::new(ProposalService::new(
            repository,
            clock.clone(),
            config.proposal_ttl,
        ));
        let items = Arc::new(ItemService::new(item_repository, clock.clone()));
        let execution = Arc::new(ExecutionService::new(
            execution_repository.clone(),
            items.clone(),
            clock.clone(),
        ));
        let authenticator: Arc<dyn Authenticator> = match config.auth_mode {
            AuthMode::LegacyStatic => Arc::new(StaticTokenAuthenticator::from_hashes(
                config.api_token_hashes.clone(),
            )),
            AuthMode::Hybrid | AuthMode::CredentialOnly => Arc::new(RuntimeAuthenticator::new(
                (config.auth_mode == AuthMode::Hybrid).then(|| config.api_token_hashes.clone()),
                credential_repository
                    .clone()
                    .ok_or(PersistenceError::AuthenticationInitializationFailed)?,
                clock.clone(),
            )),
        };
        let mcp = if let Some(repository) = scheduling.as_ref() {
            Arc::new(McpService::new_with_submissions(
                repository.clone(),
                repository.clone(),
                proposals.clone(),
                repository.clone(),
                config.mcp_allowed_origins.clone(),
            ))
        } else {
            Arc::new(McpService::new(
                Arc::new(UnavailableScheduleQueryPort),
                Arc::new(UnavailableSimulationPort),
                proposals.clone(),
                config.mcp_allowed_origins.clone(),
            ))
        };
        let mcp_oauth = config
            .mcp_oauth
            .clone()
            .map(McpOAuthVerifier::production)
            .transpose()
            .map_err(|_| PersistenceError::AuthenticationInitializationFailed)?
            .map(Arc::new);
        let mut google_sync = None;
        let google_oauth = if let Some(google) = config.google_oauth.as_ref() {
            use secrecy::ExposeSecret as _;

            let oauth_config = OAuthConfig::production(
                google.client_id.clone(),
                google.client_secret.expose_secret().to_owned(),
                google.redirect_uri.as_str(),
            )
            .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
            let client = OAuthClient::new(oauth_config)
                .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
            let cipher = SecretCipher::new_with_identity(
                google.keys.clone(),
                google.active_key_version,
                google.identity_key_version,
            );
            let sync_repository =
                google_sync_repository.ok_or(PersistenceError::IntegrationInitializationFailed)?;
            if config.google_outbound_enabled {
                let (identity_key_version, root_verifier) = cipher
                    .identity_root_verifier(oauth_scope.workspace_id, oauth_scope.user_id)
                    .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
                sync_repository
                    .verify_or_initialize_identity_root(
                        identity_key_version,
                        root_verifier,
                        clock.now(),
                    )
                    .await
                    .map_err(|error| match error {
                        google_sync::GoogleSyncRepositoryError::IdentityRootMismatch => {
                            PersistenceError::GoogleIdentityRootMismatch
                        }
                        _ => PersistenceError::IntegrationInitializationFailed,
                    })?;
            }
            let service = Arc::new(
                GoogleOAuthService::new(
                    google_oauth_repository,
                    Arc::new(
                        ProductionGoogleOAuthTransport::new(client)
                            .map_err(|_| PersistenceError::IntegrationInitializationFailed)?,
                    ),
                    cipher.clone(),
                    oauth_scope,
                    clock.clone(),
                    google.session_ttl,
                )
                .with_readiness(readiness.clone()),
            );
            service
                .recover_startup()
                .await
                .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
            service.spawn_recovery_worker();
            let sync = Arc::new(GoogleSyncService::new(
                sync_repository,
                Arc::new(ProductionGoogleSyncProvider::new(service.clone())),
                service.clone(),
                items.clone(),
                cipher,
                oauth_scope,
                clock.clone(),
                config.google_outbound_enabled,
                config.google_outbound_approval_ttl,
            ));
            sync.recover_startup()
                .await
                .map_err(|_| PersistenceError::IntegrationInitializationFailed)?;
            sync.spawn_worker();
            google_sync = Some(sync);
            Some(service)
        } else {
            None
        };

        if let Some(repository) = proposal_applications.as_ref() {
            repository.spawn_maintenance_worker();
        }
        if let Some(repository) = scheduling.as_ref() {
            repository.spawn_simulation_maintenance_worker();
        }
        Ok(Self {
            proposals,
            items,
            execution,
            authenticator,
            credential_repository,
            auth_mode: config.auth_mode,
            readiness,
            mcp,
            mcp_oauth,
            google_oauth,
            google_sync,
            proposal_applications,
            scheduling,
            execution_repository,
            clock,
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
        let execution_repository: Arc<dyn ExecutionRepository> =
            Arc::new(InMemoryExecutionRepository::default());
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let items = Arc::new(ItemService::new(
            Arc::new(InMemoryItemRepository::default()),
            clock.clone(),
        ));
        let execution = Arc::new(ExecutionService::new(
            execution_repository.clone(),
            items.clone(),
            clock.clone(),
        ));
        Self {
            proposals,
            items,
            execution,
            authenticator,
            credential_repository: None,
            auth_mode: AuthMode::LegacyStatic,
            readiness,
            mcp,
            mcp_oauth: None,
            google_oauth: None,
            google_sync: None,
            proposal_applications: None,
            scheduling: None,
            execution_repository,
            clock,
        }
    }

    #[must_use]
    pub fn with_items(mut self, items: Arc<ItemService>) -> Self {
        self.items = items;
        self.execution = Arc::new(ExecutionService::new(
            self.execution_repository.clone(),
            self.items.clone(),
            self.clock.clone(),
        ));
        self
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

    /// Installs the durable schedule publication/query adapter in an explicitly
    /// assembled dependency graph (primarily integration tests and embedded
    /// deployments). The adapter itself still enforces its fixed DB scope.
    #[must_use]
    pub fn with_postgres_scheduling(
        mut self,
        repository: Arc<PostgresSchedulingRepository>,
        allowed_origins: Arc<Vec<String>>,
    ) -> Self {
        self.mcp = Arc::new(McpService::new_with_submissions(
            repository.clone(),
            repository.clone(),
            self.proposals.clone(),
            repository.clone(),
            allowed_origins,
        ));
        self.scheduling = Some(repository);
        self
    }

    /// Installs the durable proposal-application adapter in an explicitly
    /// assembled HTTP dependency graph.
    ///
    /// # Panics
    ///
    /// Panics when the repository uses its deterministic integration-test
    /// clock. HTTP application state must always use `PostgreSQL` time.
    #[must_use]
    pub fn with_proposal_applications(
        mut self,
        repository: Arc<PostgresProposalApplicationRepository>,
    ) -> Self {
        assert!(
            !repository.uses_test_clock(),
            "a deterministic test clock cannot be installed in an HTTP AppState"
        );
        self.proposal_applications = Some(repository);
        self
    }

    #[must_use]
    pub fn with_mcp_oauth(mut self, verifier: Arc<McpOAuthVerifier>) -> Self {
        self.mcp_oauth = Some(verifier);
        self
    }

    #[must_use]
    pub fn with_google_oauth(mut self, google_oauth: Arc<GoogleOAuthService>) -> Self {
        self.google_oauth = Some(google_oauth);
        self
    }

    #[must_use]
    pub fn with_credential_auth(
        mut self,
        repository: Arc<dyn CredentialRepository>,
        authenticator: Arc<dyn Authenticator>,
        mode: AuthMode,
    ) -> Self {
        self.credential_repository = Some(repository);
        self.authenticator = authenticator;
        self.auth_mode = mode;
        self
    }
}
