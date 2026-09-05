use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

pub const ACCOUNT_DELETION_APPROVAL_PHRASE: &str = "DELETE MY DAYWEAVE ACCOUNT";
const ACCOUNT_DELETION_APPROVAL_DOMAIN: &[u8] = b"dayweave/account-deletion-approval/v1\0";
const ACCOUNT_DELETION_PRINCIPAL_DOMAIN: &[u8] =
    b"dayweave/account-deletion-external-principal/v1\0";

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

/// A deployment-owned root used only to derive the stable, one-way identifier
/// understood by the external account-deletion authority.
///
/// This key is deliberately distinct from Google credential keys and from the
/// database's unkeyed local subject digest. Its version and bytes must remain
/// available for as long as a deletion tombstone can exist.
pub struct AccountDeletionPrincipalKey {
    version: u32,
    bytes: [u8; 32],
}

impl AccountDeletionPrincipalKey {
    /// Creates a pinned pseudonym root.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero version or an all-zero key.
    pub fn new(version: u32, bytes: [u8; 32]) -> Result<Self, AccountDeletionPseudonymError> {
        if version == 0 || i32::try_from(version).is_err() {
            return Err(AccountDeletionPseudonymError::InvalidKeyVersion);
        }
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(AccountDeletionPseudonymError::InvalidKey);
        }
        Ok(Self { version, bytes })
    }

    /// Derives the non-reversible external principal identifier from the
    /// configured canonical owner subject. A future restore coordinator must
    /// do this before opening or trusting a restored database.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured owner subject is not in the same
    /// canonical form accepted by `DayWeave` configuration.
    pub fn bind(
        &self,
        owner_subject: &str,
    ) -> Result<AccountDeletionPrincipalBinding, AccountDeletionPseudonymError> {
        if owner_subject.is_empty()
            || owner_subject.trim() != owner_subject
            || owner_subject.chars().count() > 500
        {
            return Err(AccountDeletionPseudonymError::InvalidOwnerSubject);
        }
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.bytes)
            .map_err(|_| AccountDeletionPseudonymError::InvalidKey)?;
        mac.update(&(ACCOUNT_DELETION_PRINCIPAL_DOMAIN.len() as u64).to_be_bytes());
        mac.update(ACCOUNT_DELETION_PRINCIPAL_DOMAIN);
        mac.update(&self.version.to_be_bytes());
        mac.update(&(owner_subject.len() as u64).to_be_bytes());
        mac.update(owner_subject.as_bytes());
        let pseudonym = AccountDeletionPrincipalPseudonym {
            key_version: self.version,
            digest: mac.finalize().into_bytes().into(),
        };
        Ok(AccountDeletionPrincipalBinding {
            pseudonym,
            local_subject_hash: Sha256::digest(owner_subject.as_bytes()).into(),
        })
    }
}

impl Clone for AccountDeletionPrincipalKey {
    fn clone(&self) -> Self {
        Self {
            version: self.version,
            bytes: self.bytes,
        }
    }
}

impl std::fmt::Debug for AccountDeletionPrincipalKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountDeletionPrincipalKey")
            .field("version", &self.version)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

impl Drop for AccountDeletionPrincipalKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AccountDeletionPseudonymError {
    #[error("account deletion principal key version is invalid")]
    InvalidKeyVersion,
    #[error("account deletion principal key is invalid")]
    InvalidKey,
    #[error("account deletion owner subject is invalid")]
    InvalidOwnerSubject,
}

/// A deployment-keyed, non-reversible external account identity.
///
/// The digest is safe to place in a content-free external tombstone index, but
/// is intentionally not serializable by default and its debug output is
/// redacted. Key version is public rotation metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct AccountDeletionPrincipalPseudonym {
    key_version: u32,
    digest: [u8; 32],
}

impl AccountDeletionPrincipalPseudonym {
    #[must_use]
    pub const fn key_version(self) -> u32 {
        self.key_version
    }

    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }
}

impl std::fmt::Debug for AccountDeletionPrincipalPseudonym {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountDeletionPrincipalPseudonym")
            .field("key_version", &self.key_version)
            .field("digest", &"[REDACTED]")
            .finish()
    }
}

/// Binds the external keyed identity to the exact subject digest `PostgreSQL`
/// uses for its local fence. Only the derivation root can construct this value,
/// preventing callers from pairing an arbitrary external identity with a
/// different local owner.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct AccountDeletionPrincipalBinding {
    pseudonym: AccountDeletionPrincipalPseudonym,
    local_subject_hash: [u8; 32],
}

impl AccountDeletionPrincipalBinding {
    #[must_use]
    pub const fn pseudonym(self) -> AccountDeletionPrincipalPseudonym {
        self.pseudonym
    }

    #[must_use]
    pub(crate) fn matches_local_subject_hash(self, subject_hash: &[u8]) -> bool {
        self.local_subject_hash.as_slice() == subject_hash
    }
}

impl std::fmt::Debug for AccountDeletionPrincipalBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountDeletionPrincipalBinding")
            .field("pseudonym", &self.pseudonym)
            .field("local_subject_hash", &"[REDACTED]")
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::{AccountDeletionPrincipalKey, AccountDeletionPseudonymError};

    #[test]
    fn external_principal_is_stable_domain_separated_and_redacted() {
        let key = AccountDeletionPrincipalKey::new(7, [0x41; 32]).unwrap();
        let same_binding = key.bind("issuer|owner").unwrap();
        let same = same_binding.pseudonym();
        let repeated = key.bind("issuer|owner").unwrap().pseudonym();
        let other_subject = key.bind("issuer|other").unwrap().pseudonym();
        let other_version = AccountDeletionPrincipalKey::new(8, [0x41; 32])
            .unwrap()
            .bind("issuer|owner")
            .unwrap()
            .pseudonym();
        let other_key = AccountDeletionPrincipalKey::new(7, [0x42; 32])
            .unwrap()
            .bind("issuer|owner")
            .unwrap()
            .pseudonym();

        assert_eq!(same, repeated);
        assert_ne!(same, other_subject);
        assert_ne!(same, other_version);
        assert_ne!(same, other_key);
        assert_eq!(same.key_version(), 7);
        assert_eq!(
            same.digest(),
            [
                0x37, 0xa0, 0xdb, 0x75, 0xa7, 0x5c, 0x3d, 0xcf, 0xe1, 0xbc, 0x1a, 0x10, 0x3f, 0x24,
                0xc6, 0x54, 0xbb, 0xa7, 0x66, 0x3f, 0x52, 0xda, 0x31, 0x8e, 0x46, 0x1f, 0xb2, 0xce,
                0x66, 0x8e, 0x88, 0xe1,
            ],
            "the external tombstone identity encoding is a permanent contract"
        );
        let debug = format!("{same:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("issuer|owner"));
        assert!(!format!("{key:?}").contains("414141"));
        assert!(format!("{same_binding:?}").contains("[REDACTED]"));
    }

    #[test]
    fn external_principal_rejects_unsafe_roots_and_noncanonical_subjects() {
        assert_eq!(
            AccountDeletionPrincipalKey::new(0, [0x41; 32]).unwrap_err(),
            AccountDeletionPseudonymError::InvalidKeyVersion
        );
        assert_eq!(
            AccountDeletionPrincipalKey::new(1, [0; 32]).unwrap_err(),
            AccountDeletionPseudonymError::InvalidKey
        );
        let key = AccountDeletionPrincipalKey::new(1, [0x41; 32]).unwrap();
        for subject in ["", " owner", "owner "] {
            assert_eq!(
                key.bind(subject).unwrap_err(),
                AccountDeletionPseudonymError::InvalidOwnerSubject
            );
        }
        assert_eq!(
            key.bind(&"x".repeat(501)).unwrap_err(),
            AccountDeletionPseudonymError::InvalidOwnerSubject
        );
    }
}
