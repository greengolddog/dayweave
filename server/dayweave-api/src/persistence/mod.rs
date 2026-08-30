mod credential_auth_repository;
mod database;
mod execution_repository;
mod google_oauth_repository;
mod google_sync_repository;
mod idempotency;
mod item_repository;
mod outbox;
mod proposal_application_repository;
mod proposal_repository;

pub use credential_auth_repository::PostgresCredentialRepository;
pub use database::{Database, DatabaseScope, MIGRATOR, PersistenceError};
pub(crate) use database::{lock_canonical_item_space, lock_execution_and_canonical_item_space};
pub use execution_repository::PostgresExecutionRepository;
pub use google_oauth_repository::PostgresGoogleOAuthRepository;
pub(crate) use google_sync_repository::PostgresGoogleSyncRepository;
pub use idempotency::{IdempotencyDecision, IdempotencyError, PostgresIdempotencyRepository};
pub use item_repository::PostgresItemRepository;
pub(crate) use item_repository::{
    TransactionalItemCommand, TransactionalItemEffect, apply_item_command_tx, fetch_item_batch_tx,
    list_item_batch_tx, lock_execution_item_batch_tx, lock_item_batch_tx,
};
pub use outbox::{NewOutboxMessage, OutboxError, OutboxMessage, PostgresOutboxRepository};
pub use proposal_application_repository::{
    PostgresProposalApplicationRepository, ProposalApplicationError,
};
pub use proposal_repository::PostgresProposalRepository;
pub(crate) use proposal_repository::{insert_proposal_tx, proposal_from_row};
