//! Bounded pre-close recovery, deliberately without an HTTP activation path.

use std::sync::Arc;

use thiserror::Error;

use crate::{
    google_oauth::GoogleOAuthService,
    persistence::PostgresAccountDeletionRepository,
    provider_admission::{DrainedProviderAdmission, ProviderAdmission},
};

use super::{
    AccountDeletionFenceConfirmation, AccountDeletionProviderReadiness, AccountDeletionRepository,
    AccountDeletionRepositoryError,
};

const MAX_RECOVERY_PASSES: usize = 4;

#[cfg(test)]
#[path = "provider_preparation_tests.rs"]
mod tests;

#[derive(Debug)]
pub enum AccountDeletionProviderPreparationResult {
    Waiting {
        readiness: AccountDeletionProviderReadiness,
        cancelled_authorizations: u64,
    },
    Drained {
        proof: DrainedProviderAdmission,
        cancelled_authorizations: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AccountDeletionProviderPreparationError {
    #[error(transparent)]
    Repository(#[from] AccountDeletionRepositoryError),
    #[error("Google authorization recovery remains unavailable")]
    RecoveryUnavailable,
    #[error("provider admission preparation is unavailable")]
    AdmissionUnavailable,
}

/// Internal orchestration for an already prepared and freshly confirmed
/// deletion. This is not an HTTP authentication boundary or a deletion switch;
/// external restore/grant authority and the final fence checks remain required.
pub struct AccountDeletionProviderPreparationService {
    repository: Arc<PostgresAccountDeletionRepository>,
    oauth: Arc<GoogleOAuthService>,
}

impl AccountDeletionProviderPreparationService {
    /// Binds the exact production OAuth/sync controller, not just matching IDs.
    ///
    /// # Errors
    /// Rejects absent, local-only, or mismatched admission controllers.
    pub fn new(
        repository: Arc<PostgresAccountDeletionRepository>,
        oauth: Arc<GoogleOAuthService>,
    ) -> Result<Self, AccountDeletionProviderPreparationError> {
        if !repository.matches_provider_admission(oauth.admission()) {
            return Err(AccountDeletionRepositoryError::Disabled.into());
        }
        Ok(Self { repository, oauth })
    }

    /// Cancels only pending authorizations and runs at most four existing
    /// recovery passes while admission remains open. Before each pass and once
    /// afterward, closure is attempted atomically with readiness verification.
    /// Busy work is reported, never reaped or waited on under a database lock.
    ///
    /// The initial close attempt lets a retry observe the previous pass's exact
    /// asynchronous settlement without starting another provider operation.
    /// On ambiguous closure, persisted admission remains authoritative; retries
    /// cannot reopen it. This does not advance the deletion lifecycle.
    ///
    /// # Errors
    /// Rejects invalid/stale authority, reentrant preparation from provider
    /// work, or unavailable recovery/admission. Provider errors are not exposed.
    pub async fn prepare(
        &self,
        confirmation: &AccountDeletionFenceConfirmation,
    ) -> Result<AccountDeletionProviderPreparationResult, AccountDeletionProviderPreparationError>
    {
        ProviderAdmission::ensure_outside_operation()
            .map_err(|_| AccountDeletionProviderPreparationError::AdmissionUnavailable)?;
        self.repository
            .authorize_provider_preparation(confirmation)
            .await?;
        let mut cancelled_authorizations = 0_u64;
        let mut pass = 0;
        loop {
            let readiness = self
                .repository
                .close_provider_admission_if_ready(confirmation)
                .await?;
            if readiness.is_ready() && readiness.admission_closed {
                let proof = self
                    .oauth
                    .admission()
                    .close_and_drain(confirmation.transition.deletion_id)
                    .await
                    .map_err(|_| AccountDeletionProviderPreparationError::AdmissionUnavailable)?;
                return Ok(AccountDeletionProviderPreparationResult::Drained {
                    proof,
                    cancelled_authorizations,
                });
            }
            if pass == MAX_RECOVERY_PASSES || readiness.admission_closed {
                return Ok(AccountDeletionProviderPreparationResult::Waiting {
                    readiness,
                    cancelled_authorizations,
                });
            }
            cancelled_authorizations = cancelled_authorizations.saturating_add(
                self.oauth
                    .prepare_for_account_deletion()
                    .await
                    .map_err(|_| AccountDeletionProviderPreparationError::RecoveryUnavailable)?,
            );
            pass += 1;
        }
    }
}
