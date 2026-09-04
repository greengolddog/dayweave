mod credential_auth_repository;
mod database;
mod execution_repository;
mod google_oauth_repository;
mod google_sync_repository;
mod habit_repository;
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
pub use habit_repository::PostgresHabitRepository;
pub(crate) use habit_repository::{
    AuthoritativeHabitRecurrence, PublishedHabitEvidenceError, authoritative_habit_recurrence_tx,
    record_published_habit_occurrences_tx,
};
pub use idempotency::{IdempotencyDecision, IdempotencyError, PostgresIdempotencyRepository};
pub use item_repository::PostgresItemRepository;
pub(crate) use item_repository::{
    TransactionalGraphMode, TransactionalItemCommand, TransactionalItemEffect,
    apply_item_command_tx, clear_dependency_edges_tx, fetch_item_batch_tx, list_item_batch_tx,
    lock_execution_item_batch_tx, lock_item_batch_tx, stage_item_create_tx, staged_item_shell,
    start_item_change_group_tx, validate_dependency_graph_batch_tx, validate_item_change_group_tx,
    validate_preview_item_change_group_tx,
};
pub use outbox::{NewOutboxMessage, OutboxError, OutboxMessage, PostgresOutboxRepository};
pub use proposal_application_repository::{
    PostgresProposalApplicationRepository, ProposalApplicationError,
};
pub use proposal_repository::PostgresProposalRepository;
pub(crate) use proposal_repository::{insert_proposal_tx, proposal_from_row};
