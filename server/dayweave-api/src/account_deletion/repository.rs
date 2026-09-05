use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::credential_auth::OpaqueCredential;

use super::{
    AccountDeletionFenceConfirmation, AccountDeletionFenceSafetyEvidence, AccountDeletionLifecycle,
    AccountDeletionMutation, AccountDeletionPreparation, AccountDeletionPreparationSafetyEvidence,
    AccountDeletionPrincipalPseudonym, AccountDeletionTransition,
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

    /// Atomically installs the hard scope fence and advances the lifecycle to
    /// `fence_committing`. Once this succeeds cancellation is forbidden.
    async fn begin_fence(
        &self,
        confirmation: AccountDeletionFenceConfirmation,
    ) -> Result<AccountDeletionMutation, AccountDeletionRepositoryError>;

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
