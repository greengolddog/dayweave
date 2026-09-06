use chrono::{DateTime, Utc};

/// A content-free snapshot of work that must settle before provider admission
/// can close for account deletion. Counts may overlap: for example, exhausted
/// cleanup tokens are also included in their custody-state count and total.
///
/// Readiness is not evidence of provider revocation, external grant ownership,
/// interrupted-operation settlement, or a runtime-held restore permit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccountDeletionProviderReadiness {
    pub pending_authorizations: u64,
    pub exchanging_authorizations: u64,
    pub staged_authorizations: u64,
    /// Every retained cleanup token, including any future custody states.
    pub cleanup_tokens: u64,
    pub held_cleanup_tokens: u64,
    pub pending_cleanup_tokens: u64,
    pub revoking_cleanup_tokens: u64,
    pub operator_required_cleanup_tokens: u64,
    pub exhausted_cleanup_tokens: u64,
    pub unresolved_legacy_credentials: u64,
    pub revocation_fences: u64,
    /// Non-revoked accounts with a nil ID or an unsupported provider.
    pub unsupported_provider_accounts: u64,
    pub disconnecting_provider_accounts: u64,
    pub revocation_failed_provider_accounts: u64,
    pub operator_recovery_provider_accounts: u64,
    /// Number of non-revoked accounts above the supported manifest limit.
    pub excess_provider_accounts: u64,
    pub provider_accounts_missing_oauth_scope: u64,
    pub running_sync_runs: u64,
    pub delivering_sync_outbox: u64,
    pub delivering_schedule_outbox: u64,
    /// Batches in the delivering state or with a positive delivering count.
    pub delivering_schedule_batches: u64,
    /// Unsettled operations sharing either the workspace or the user. Old or
    /// interrupted registrations remain blockers; their age is irrelevant.
    pub unsettled_provider_operations: u64,
    /// Earliest stored retry time of a pending, non-exhausted cleanup token.
    /// Other fences may still prevent that attempt; this is not a deadline or
    /// permission to revoke a grant, reclaim custody, or bypass backoff.
    pub next_cleanup_attempt_at: Option<DateTime<Utc>>,
    /// The exact scope is durably closed for the requested deletion already.
    /// This says nothing about the process-local gate or a final account fence.
    pub admission_closed: bool,
}

impl AccountDeletionProviderReadiness {
    /// Whether the snapshot contains no provider-preparation blockers. Closure
    /// is reported separately, and a snapshot alone never authorizes closure.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        [
            self.pending_authorizations,
            self.exchanging_authorizations,
            self.staged_authorizations,
            self.cleanup_tokens,
            self.held_cleanup_tokens,
            self.pending_cleanup_tokens,
            self.revoking_cleanup_tokens,
            self.operator_required_cleanup_tokens,
            self.exhausted_cleanup_tokens,
            self.unresolved_legacy_credentials,
            self.revocation_fences,
            self.unsupported_provider_accounts,
            self.disconnecting_provider_accounts,
            self.revocation_failed_provider_accounts,
            self.operator_recovery_provider_accounts,
            self.excess_provider_accounts,
            self.provider_accounts_missing_oauth_scope,
            self.running_sync_runs,
            self.delivering_sync_outbox,
            self.delivering_schedule_outbox,
            self.delivering_schedule_batches,
            self.unsettled_provider_operations,
        ]
        .into_iter()
        .all(|count| count == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::AccountDeletionProviderReadiness;

    #[test]
    fn every_blocker_prevents_readiness_even_after_closure() {
        macro_rules! assert_blockers {
            ($($field:ident),+ $(,)?) => {
                $(
                    for admission_closed in [false, true] {
                        let readiness = AccountDeletionProviderReadiness {
                            $field: 1,
                            admission_closed,
                            ..AccountDeletionProviderReadiness::default()
                        };
                        assert!(!readiness.is_ready(), stringify!($field));
                    }
                )+
            };
        }

        assert_blockers!(
            pending_authorizations,
            exchanging_authorizations,
            staged_authorizations,
            cleanup_tokens,
            held_cleanup_tokens,
            pending_cleanup_tokens,
            revoking_cleanup_tokens,
            operator_required_cleanup_tokens,
            exhausted_cleanup_tokens,
            unresolved_legacy_credentials,
            revocation_fences,
            unsupported_provider_accounts,
            disconnecting_provider_accounts,
            revocation_failed_provider_accounts,
            operator_recovery_provider_accounts,
            excess_provider_accounts,
            provider_accounts_missing_oauth_scope,
            running_sync_runs,
            delivering_sync_outbox,
            delivering_schedule_outbox,
            delivering_schedule_batches,
            unsettled_provider_operations,
        );
    }

    #[test]
    fn closure_and_retry_metadata_are_not_readiness_proofs() {
        for admission_closed in [false, true] {
            let readiness = AccountDeletionProviderReadiness {
                admission_closed,
                next_cleanup_attempt_at: Some(chrono::DateTime::UNIX_EPOCH),
                ..AccountDeletionProviderReadiness::default()
            };
            assert!(readiness.is_ready());
        }
    }
}
