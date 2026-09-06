mod domain;
mod repository;

pub use domain::{
    ACCOUNT_DELETION_APPROVAL_PHRASE, ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS,
    ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_TARGETS, AccountDeletionFenceConfirmation,
    AccountDeletionFenceSafetyEvidence, AccountDeletionLifecycle, AccountDeletionMutation,
    AccountDeletionPreparation, AccountDeletionPreparationSafetyEvidence,
    AccountDeletionPrincipalBinding, AccountDeletionPrincipalKey,
    AccountDeletionPrincipalPseudonym, AccountDeletionProvider,
    AccountDeletionProviderCleanupClaim, AccountDeletionProviderCleanupCompletion,
    AccountDeletionProviderCleanupFailure, AccountDeletionProviderCleanupMutation,
    AccountDeletionProviderCleanupOutcome, AccountDeletionProviderCleanupStatus,
    AccountDeletionProviderCleanupSummary, AccountDeletionPseudonymError, AccountDeletionStatus,
    AccountDeletionTransition, account_deletion_approval_digest,
};
pub(crate) use domain::{
    AccountDeletionProviderCleanupTargetBinding, AccountDeletionProviderCredentialEnvelope,
    account_deletion_provider_cleanup_manifest_digest,
};
pub use repository::{
    AccountDeletionRepository, AccountDeletionRepositoryError, AccountDeletionSafetyGate,
    AccountDeletionSafetyGateError, DisabledAccountDeletionSafetyGate,
};
