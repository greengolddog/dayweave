use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const ACCOUNT_DELETION_APPROVAL_PHRASE: &str = "DELETE MY DAYWEAVE ACCOUNT";
const ACCOUNT_DELETION_APPROVAL_DOMAIN: &[u8] = b"dayweave/account-deletion-approval/v1\0";

/// Binds the exact destructive phrase and v1 policy to this owner and request.
/// An HTTP layer must compare the user-supplied phrase exactly before calling
/// this helper; retaining only this digest avoids persisting the phrase.
#[must_use]
pub fn account_deletion_approval_digest(
    deletion_id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(ACCOUNT_DELETION_APPROVAL_DOMAIN);
    digest.update(deletion_id.as_bytes());
    digest.update(workspace_id.as_bytes());
    digest.update(user_id.as_bytes());
    digest.update(ACCOUNT_DELETION_APPROVAL_PHRASE.as_bytes());
    digest.finalize().into()
}

/// A deletion is intentionally a durable workflow rather than a synchronous
/// `DELETE` request. `Complete` is reserved for a future backup-erasure gate;
/// the current local purge stops at `BackupWait`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountDeletionStatus {
    Prepared,
    FenceCommitting,
    Fenced,
    ProviderCleanup,
    Purge,
    BackupWait,
    Complete,
    Cancelled,
    Failed,
}

impl AccountDeletionStatus {
    #[must_use]
    pub const fn as_storage_name(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::FenceCommitting => "fence_committing",
            Self::Fenced => "fenced",
            Self::ProviderCleanup => "provider_cleanup",
            Self::Purge => "purge",
            Self::BackupWait => "backup_wait",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "fence_committing" => Some(Self::FenceCommitting),
            "fenced" => Some(Self::Fenced),
            "provider_cleanup" => Some(Self::ProviderCleanup),
            "purge" => Some(Self::Purge),
            "backup_wait" => Some(Self::BackupWait),
            "complete" => Some(Self::Complete),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountDeletionLifecycle {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub status: AccountDeletionStatus,
    pub revision: u64,
    pub prepared_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub local_purge_completed_at: Option<DateTime<Utc>>,
}

/// The app must create this only after an explicit destructive-action
/// confirmation. The digest binds the exact confirmation policy without
/// retaining the phrase or any recovery credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountDeletionPreparation {
    pub id: Uuid,
    pub request_hash: [u8; 32],
    pub explicit_approval_digest: [u8; 32],
    pub authorizing_session_id: Uuid,
    pub authorizing_session_revision: u64,
    pub authorizing_recovery_code_id: Uuid,
    pub authorizing_recovery_code_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountDeletionTransition {
    pub deletion_id: Uuid,
    pub request_hash: [u8; 32],
    pub expected_revision: u64,
    pub from: AccountDeletionStatus,
    pub to: AccountDeletionStatus,
    pub failure_code: Option<String>,
}

/// Second destructive confirmation after the mandatory cooling-off period.
/// It must name a currently authenticated, freshly issued full-owner native
/// device credential; it cannot reuse the stale preparation revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountDeletionFenceConfirmation {
    pub transition: AccountDeletionTransition,
    pub confirming_session_id: Uuid,
    pub confirming_session_revision: u64,
    pub explicit_approval_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountDeletionMutation {
    pub deletion_id: Uuid,
    pub status: AccountDeletionStatus,
    pub revision: u64,
    pub replayed: bool,
}

/// Opaque, content-free evidence returned by deployment integrations. There is
/// deliberately no built-in implementation: production stays disabled until
/// a real external tombstone writer and per-principal limiter are wired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountDeletionPreparationSafetyEvidence {
    /// Opaque evidence that the external per-principal destructive-action
    /// limiter accepted this exact deletion preparation.
    pub principal_rate_limit_hash: [u8; 32],
}

/// Evidence that an external permanent anti-resurrection tombstone was
/// committed. The operation must be exactly idempotent by deletion id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountDeletionFenceSafetyEvidence {
    pub external_tombstone_hash: [u8; 32],
}
