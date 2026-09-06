use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::{credential_auth::OpaqueCredential, provider_admission::DrainedProviderAdmission};

use super::{
    AccountDeletionFenceConfirmation, AccountDeletionFenceSafetyEvidence, AccountDeletionLifecycle,
    AccountDeletionMutation, AccountDeletionPreparation, AccountDeletionPreparationSafetyEvidence,
    AccountDeletionPrincipalPseudonym, AccountDeletionProviderCleanupClaim,
    AccountDeletionProviderCleanupCompletion, AccountDeletionProviderCleanupMutation,
    AccountDeletionProviderCleanupSummary, AccountDeletionProviderReadiness,
    AccountDeletionTransition,
};

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AccountDeletionSafetyGateError {
    #[error("account deletion is disabled until external safety gates are configured")]
    Disabled,
    #[error("account deletion safety gate is temporarily unavailable")]
    Unavailable,
}

#[async_trait]
pub trait AccountDeletionSafetyGate: Send + Sync {
    /// Consumes the external per-principal destructive-action allowance.
    /// Calls must be exactly idempotent by deletion id. This is not a restore
    /// fence or tombstone; a still-prepared lifecycle remains cancellable.
    async fn authorize_preparation(
        &self,
        principal: AccountDeletionPrincipalPseudonym,
        deletion_id: Uuid,
    ) -> Result<AccountDeletionPreparationSafetyEvidence, AccountDeletionSafetyGateError>;

    /// Commits the permanent external anti-resurrection tombstone after the
    /// local hard fence exists. Calls at this lower layer must be exactly
    /// idempotent by deletion id. Activation additionally requires an
    /// exclusive external permit held for the runtime's admission lifetime;
    /// a one-shot restore lookup is racy. The database's local unkeyed fence
    /// digest is never acceptable as the external principal.
    async fn commit_tombstone(
        &self,
        principal: AccountDeletionPrincipalPseudonym,
        deletion_id: Uuid,
    ) -> Result<AccountDeletionFenceSafetyEvidence, AccountDeletionSafetyGateError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DisabledAccountDeletionSafetyGate;

#[async_trait]
impl AccountDeletionSafetyGate for DisabledAccountDeletionSafetyGate {
    async fn authorize_preparation(
        &self,
        _principal: AccountDeletionPrincipalPseudonym,
        _deletion_id: Uuid,
    ) -> Result<AccountDeletionPreparationSafetyEvidence, AccountDeletionSafetyGateError> {
        Err(AccountDeletionSafetyGateError::Disabled)
    }

    async fn commit_tombstone(
        &self,
        _principal: AccountDeletionPrincipalPseudonym,
        _deletion_id: Uuid,
    ) -> Result<AccountDeletionFenceSafetyEvidence, AccountDeletionSafetyGateError> {
        Err(AccountDeletionSafetyGateError::Disabled)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AccountDeletionRepositoryError {
    #[error("account deletion is disabled")]
    Disabled,
    #[error("account deletion input is invalid")]
    InvalidInput,
    #[error("only a fresh full-owner v2 native device session may delete the account")]
    InvalidAuthority,
    #[error("account deletion is still in its mandatory cooling-off period")]
    CooldownPending,
    #[error("account deletion is limited to an unshared personal scope")]
    UnsupportedScope,
    #[error("account deletion state conflicts with the request")]
    Conflict,
    #[error("provider activity must be reconciled before account deletion can continue")]
    ProviderCleanupBlocked,
    #[error("account deletion repository operation failed")]
    Internal,
}

#[async_trait]
/// Low-level persistence workflow, not an HTTP authentication boundary. It is
/// default-disabled and has no route. Before a service exposes it, that layer
/// must require `credential_only` mode and an authenticated full-owner Device
/// principal whose scope, user, workspace, and credential id exactly match the
/// requested session; legacy/static, hybrid, MCP, and OAuth principals are not
/// deletion authorities.
pub trait AccountDeletionRepository: Send + Sync {
    async fn lifecycle(
        &self,
        deletion_id: Uuid,
    ) -> Result<Option<AccountDeletionLifecycle>, AccountDeletionRepositoryError>;

    async fn prepare(
        &self,
        preparation: AccountDeletionPreparation,
        recovery_code: &OpaqueCredential<'_>,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError>;

    /// Revalidates the prepared lifecycle, fresh confirming owner, cooldown,
    /// recovery continuity, personal scope, and configured durable controller
    /// before any pending authorization is cancelled or recovery is attempted.
    /// This does not install a fence or authorize an external grant revocation.
    async fn authorize_provider_preparation(
        &self,
        confirmation: &AccountDeletionFenceConfirmation,
    ) -> Result<(), AccountDeletionRepositoryError>;

    /// Revalidates authority and atomically snapshots all provider blockers and
    /// overlapping operations before conditional durable closure. A blocked
    /// result leaves open admission open; a ready result has committed exact
    /// closure. No provider call or provider wait occurs under database locks.
    async fn close_provider_admission_if_ready(
        &self,
        confirmation: &AccountDeletionFenceConfirmation,
    ) -> Result<AccountDeletionProviderReadiness, AccountDeletionRepositoryError>;

    /// Atomically installs the hard scope fence and advances the lifecycle to
    /// `fence_committing`. Once this succeeds cancellation is forbidden. The
    /// caller must first close and drain the exact configured Google runtime
    /// controller, without holding database mutation locks. It must have a
    /// durable registration backend; closure and zero unsettled operations are
    /// rechecked inside the fence transaction. This is not a restore permit.
    async fn begin_fence(
        &self,
        confirmation: AccountDeletionFenceConfirmation,
        drained: &DrainedProviderAdmission,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError>;

    /// Atomically seals the supported provider credential source coordinates
    /// and hashes in a content-free manifest and enters `provider_cleanup`.
    /// No provider call is made by this persistence boundary.
    async fn seal_provider_cleanup(
        &self,
        transition: AccountDeletionTransition,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError>;

    /// Claims one due provider target using the database clock. The returned
    /// encrypted envelope exists only in memory and is bound to the immutable
    /// target revision/key/ciphertext commitment. None means no eligible claim;
    /// consult `provider_cleanup_status` to distinguish waiting/intervention
    /// from definitive outcomes. A resolved or expired claim id cannot be reused.
    async fn claim_provider_cleanup(
        &self,
        deletion_id: Uuid,
        claim_id: Uuid,
    ) -> Result<Option<AccountDeletionProviderCleanupClaim>, AccountDeletionRepositoryError>;

    /// Records one exact provider result. Retry timing, exhaustion, and the
    /// 24-hour intervention deadline are calculated by the repository.
    /// Replay returns the original attempt result, not the current state.
    /// Evidence must be supplied only by a trusted provider worker; this method
    /// must never be exposed as a client-controlled evidence-submission route.
    async fn resolve_provider_cleanup(
        &self,
        completion: AccountDeletionProviderCleanupCompletion,
    ) -> Result<AccountDeletionProviderCleanupMutation, AccountDeletionRepositoryError>;

    /// Reads current counts, manifest integrity, retry/lease wakeup time, and
    /// fixed intervention reasons in one database snapshot.
    async fn provider_cleanup_status(
        &self,
        deletion_id: Uuid,
    ) -> Result<Option<AccountDeletionProviderCleanupSummary>, AccountDeletionRepositoryError>;

    /// Advances an exact lifecycle edge only through `provider_cleanup` intent.
    /// This foundation deliberately exposes no provider-cleanup-to-purge edge:
    /// durable per-provider revocation outcomes/retries, a bounded policy, and
    /// an exclusive runtime-held external restore permit must exist first.
    /// Completion is likewise unavailable until backup-erasure evidence is
    /// real.
    async fn advance(
        &self,
        transition: AccountDeletionTransition,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError>;
}
