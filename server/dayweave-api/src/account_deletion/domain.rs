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
const ACCOUNT_DELETION_PROVIDER_MANIFEST_DOMAIN: &[u8] =
    b"dayweave/account-deletion-provider-cleanup-manifest/v1\0";

pub const ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_ATTEMPTS: u32 = 12;
pub const ACCOUNT_DELETION_PROVIDER_CLEANUP_MAX_TARGETS: usize = 64;

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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AccountDeletionProvider {
    Google,
}

impl AccountDeletionProvider {
    #[must_use]
    pub const fn as_storage_name(self) -> &'static str {
        match self {
            Self::Google => "google",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Self::Google),
            _ => None,
        }
    }

    const fn manifest_tag(self) -> u8 {
        match self {
            Self::Google => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountDeletionProviderCleanupStatus {
    Pending,
    Claimed,
    RetryWait,
    Revoked,
    OperatorRequired,
}

impl AccountDeletionProviderCleanupStatus {
    #[must_use]
    pub const fn as_storage_name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::RetryWait => "retry_wait",
            Self::Revoked => "revoked",
            Self::OperatorRequired => "operator_required",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "retry_wait" => Some(Self::RetryWait),
            "revoked" => Some(Self::Revoked),
            "operator_required" => Some(Self::OperatorRequired),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountDeletionProviderCleanupFailure {
    ProviderUnavailable,
    ProviderRejected,
    CredentialUnavailable,
    CredentialDrift,
    ClaimLeaseExpired,
    RetryExhausted,
    DeadlineExceeded,
}

impl AccountDeletionProviderCleanupFailure {
    #[must_use]
    pub const fn as_storage_name(self) -> &'static str {
        match self {
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderRejected => "provider_rejected",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::CredentialDrift => "credential_drift",
            Self::ClaimLeaseExpired => "claim_lease_expired",
            Self::RetryExhausted => "retry_exhausted",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "provider_unavailable" => Some(Self::ProviderUnavailable),
            "provider_rejected" => Some(Self::ProviderRejected),
            "credential_unavailable" => Some(Self::CredentialUnavailable),
            "credential_drift" => Some(Self::CredentialDrift),
            "claim_lease_expired" => Some(Self::ClaimLeaseExpired),
            "retry_exhausted" => Some(Self::RetryExhausted),
            "deadline_exceeded" => Some(Self::DeadlineExceeded),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountDeletionProviderCleanupOutcome {
    Revoked { evidence_hash: [u8; 32] },
    AlreadyAbsent { evidence_hash: [u8; 32] },
    RetryableFailure,
    OperatorRequired(AccountDeletionProviderCleanupFailure),
}

pub(crate) struct AccountDeletionProviderCredentialEnvelope {
    key_version: u32,
    ciphertext: Vec<u8>,
}

impl AccountDeletionProviderCredentialEnvelope {
    pub(crate) fn new(key_version: u32, ciphertext: Vec<u8>) -> Self {
        Self {
            key_version,
            ciphertext,
        }
    }

    pub(crate) const fn key_version(&self) -> u32 {
        self.key_version
    }

    pub(crate) fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }
}

impl std::fmt::Debug for AccountDeletionProviderCredentialEnvelope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountDeletionProviderCredentialEnvelope")
            .field("key_version", &self.key_version)
            .field("ciphertext", &"[REDACTED]")
            .finish()
    }
}

impl Drop for AccountDeletionProviderCredentialEnvelope {
    fn drop(&mut self) {
        self.ciphertext.zeroize();
    }
}

pub struct AccountDeletionProviderCleanupClaim {
    pub deletion_id: Uuid,
    pub provider_account_id: Uuid,
    pub provider: AccountDeletionProvider,
    pub provider_account_revision: u64,
    pub credential_generation: u64,
    pub encrypted_credentials_hash: [u8; 32],
    pub claim_id: Uuid,
    pub attempt: u32,
    pub lease_expires_at: DateTime<Utc>,
    pub replayed: bool,
    credential: AccountDeletionProviderCredentialEnvelope,
}

impl AccountDeletionProviderCleanupClaim {
    pub(crate) const fn credential(&self) -> &AccountDeletionProviderCredentialEnvelope {
        &self.credential
    }

    pub(crate) fn new(
        deletion_id: Uuid,
        target: AccountDeletionProviderCleanupTargetBinding,
        claim_id: Uuid,
        attempt: u32,
        lease_expires_at: DateTime<Utc>,
        replayed: bool,
        credential: AccountDeletionProviderCredentialEnvelope,
    ) -> Self {
        Self {
            deletion_id,
            provider_account_id: target.provider_account_id,
            provider: target.provider,
            provider_account_revision: target.provider_account_revision,
            credential_generation: target.credential_generation,
            encrypted_credentials_hash: target.encrypted_credentials_hash,
            claim_id,
            attempt,
            lease_expires_at,
            replayed,
            credential,
        }
    }
}

impl std::fmt::Debug for AccountDeletionProviderCleanupClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountDeletionProviderCleanupClaim")
            .field("deletion_id", &self.deletion_id)
            .field("provider_account_id", &self.provider_account_id)
            .field("provider", &self.provider)
            .field("provider_account_revision", &self.provider_account_revision)
            .field("credential_generation", &self.credential_generation)
            .field("encrypted_credentials_hash", &"[REDACTED]")
            .field("claim_id", &self.claim_id)
            .field("attempt", &self.attempt)
            .field("lease_expires_at", &self.lease_expires_at)
            .field("replayed", &self.replayed)
            .field("credential_key_version", &self.credential().key_version())
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountDeletionProviderCleanupCompletion {
    pub deletion_id: Uuid,
    pub provider_account_id: Uuid,
    pub claim_id: Uuid,
    pub attempt: u32,
    pub outcome: AccountDeletionProviderCleanupOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The recorded result of one attempt. A replay is historical: use the cleanup
/// summary, not this result, to determine the target's current state.
pub struct AccountDeletionProviderCleanupMutation {
    pub deletion_id: Uuid,
    pub provider_account_id: Uuid,
    pub status: AccountDeletionProviderCleanupStatus,
    pub attempt: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub replayed: bool,
}

/// One consistent, content-free view of cleanup progress. Definitive provider
/// outcomes do not authorize local purge or prove account deletion complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountDeletionProviderCleanupSummary {
    pub deletion_id: Uuid,
    pub lifecycle_status: AccountDeletionStatus,
    pub manifest_sealed: bool,
    pub target_count: u32,
    pub pending_count: u32,
    pub claimed_count: u32,
    pub retry_wait_count: u32,
    pub revoked_count: u32,
    pub operator_required_count: u32,
    /// Earliest pending/retry time or current claim lease expiry. This can be
    /// in the past when work is due. None alone never means completion.
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub operator_reasons: Vec<AccountDeletionProviderCleanupFailure>,
}

impl AccountDeletionProviderCleanupSummary {
    #[must_use]
    pub const fn all_provider_outcomes_recorded(&self) -> bool {
        self.manifest_sealed
            && self.revoked_count == self.target_count
            && self.pending_count == 0
            && self.claimed_count == 0
            && self.retry_wait_count == 0
            && self.operator_required_count == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AccountDeletionProviderCleanupTargetBinding {
    pub provider: AccountDeletionProvider,
    pub provider_account_id: Uuid,
    pub provider_account_revision: u64,
    pub credential_generation: u64,
    pub credential_key_version: u32,
    pub encrypted_credentials_hash: [u8; 32],
}

pub(crate) fn account_deletion_provider_cleanup_manifest_digest(
    deletion_id: Uuid,
    targets: &[AccountDeletionProviderCleanupTargetBinding],
) -> [u8; 32] {
    let mut ordered = targets.to_vec();
    ordered.sort_unstable();
    let mut digest = Sha256::new();
    digest.update((ACCOUNT_DELETION_PROVIDER_MANIFEST_DOMAIN.len() as u64).to_be_bytes());
    digest.update(ACCOUNT_DELETION_PROVIDER_MANIFEST_DOMAIN);
    digest.update(deletion_id.as_bytes());
    digest.update((ordered.len() as u64).to_be_bytes());
    for target in ordered {
        digest.update([target.provider.manifest_tag()]);
        digest.update(target.provider_account_id.as_bytes());
        digest.update(target.provider_account_revision.to_be_bytes());
        digest.update(target.credential_generation.to_be_bytes());
        digest.update(target.credential_key_version.to_be_bytes());
        digest.update(target.encrypted_credentials_hash);
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::{
        AccountDeletionPrincipalKey, AccountDeletionProvider,
        AccountDeletionProviderCleanupTargetBinding, AccountDeletionPseudonymError,
        account_deletion_provider_cleanup_manifest_digest,
    };
    use uuid::Uuid;

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

    #[test]
    fn provider_cleanup_manifest_is_stable_order_independent_and_bound_to_credentials() {
        let deletion_id = Uuid::from_u128(1);
        let first = AccountDeletionProviderCleanupTargetBinding {
            provider: AccountDeletionProvider::Google,
            provider_account_id: Uuid::from_u128(2),
            provider_account_revision: 3,
            credential_generation: 4,
            credential_key_version: 5,
            encrypted_credentials_hash: [0x51; 32],
        };
        let second = AccountDeletionProviderCleanupTargetBinding {
            provider: AccountDeletionProvider::Google,
            provider_account_id: Uuid::from_u128(6),
            provider_account_revision: 7,
            credential_generation: 8,
            credential_key_version: 9,
            encrypted_credentials_hash: [0x52; 32],
        };
        let expected =
            account_deletion_provider_cleanup_manifest_digest(deletion_id, &[first, second]);
        assert_eq!(
            expected,
            account_deletion_provider_cleanup_manifest_digest(deletion_id, &[second, first])
        );
        assert_ne!(
            expected,
            account_deletion_provider_cleanup_manifest_digest(
                deletion_id,
                &[
                    AccountDeletionProviderCleanupTargetBinding {
                        encrypted_credentials_hash: [0x53; 32],
                        ..first
                    },
                    second
                ]
            )
        );
        assert_eq!(
            expected,
            [
                0x95, 0xfe, 0xde, 0x0f, 0x05, 0xa1, 0x6b, 0x1e, 0x5d, 0x86, 0x97, 0x14, 0xdd, 0xb1,
                0x02, 0x5e, 0xb5, 0xea, 0x04, 0xaa, 0xd8, 0x53, 0x72, 0x3d, 0xd3, 0x5a, 0x9c, 0x88,
                0xc3, 0x4b, 0xf2, 0x2a,
            ],
            "the sealed provider manifest encoding is a permanent contract"
        );
    }
}
