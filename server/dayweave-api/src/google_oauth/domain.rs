use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use super::crypto::SealedSecret;

pub type SecretHash = [u8; 32];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoogleAccountStatus {
    Active,
    Paused,
    ReauthorizationRequired,
    Disconnecting,
    RevocationFailed,
    Revoked,
}

impl GoogleAccountStatus {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::ReauthorizationRequired => "reauthorization_required",
            Self::Disconnecting => "disconnecting",
            Self::RevocationFailed => "revocation_failed",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleAccount {
    pub id: Uuid,
    pub external_account_id: String,
    pub display_label: String,
    pub status: GoogleAccountStatus,
    pub sync_enabled: bool,
    pub is_default: bool,
    pub granted_scopes: BTreeSet<String>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct StoredCredentials {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    /// Older refresh credentials are retained only so disconnect can revoke
    /// every credential `DayWeave` has ever accepted for this account.
    pub retired_refresh_tokens: Vec<SecretString>,
    pub token_type: String,
    pub access_expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for StoredCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCredentials")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field(
                "retired_refresh_tokens",
                &format_args!("[REDACTED; {}]", self.retired_refresh_tokens.len()),
            )
            .field("token_type", &self.token_type)
            .field("access_expires_at", &self.access_expires_at)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct EncryptedCredentials {
    pub sealed: SealedSecret,
}

#[derive(Clone, Debug)]
pub struct AccountSecretSnapshot {
    pub account: GoogleAccount,
    pub credentials: EncryptedCredentials,
}

#[derive(Clone, Debug)]
pub struct NewOAuthSession {
    pub id: Uuid,
    pub owner_subject_hash: SecretHash,
    pub state_hash: SecretHash,
    pub encrypted_verifier: SealedSecret,
    pub encrypted_authorization_url: SealedSecret,
    pub requested_scopes: BTreeSet<String>,
    pub expected_account_id: Option<Uuid>,
    pub expected_account_revision: Option<u64>,
    pub make_default: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct OAuthSessionStart {
    pub id: Uuid,
    pub encrypted_authorization_url: SealedSecret,
    pub expires_at: DateTime<Utc>,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub enum CallbackClaim {
    Exchange(Box<ClaimedOAuthSession>),
    Staged { session_id: Uuid },
}

#[derive(Clone, Debug)]
pub enum AuthorizationResolution {
    NeverStaged,
    Staged,
    Consumed(GoogleAccount),
}

#[derive(Clone, Debug)]
pub struct CleanupClaim {
    pub session_id: Uuid,
    pub claim_id: Uuid,
    pub encrypted_refresh_token: SealedSecret,
    /// Verified Google subject for the newly exchanged credential. `None`
    /// means identity lookup did not complete and revocation must be treated
    /// as unsafe whenever another Google grant is retained locally.
    pub external_account_id: Option<String>,
    /// Snapshot used to prevent grant-wide Google revocation from invalidating
    /// a retained credential for the same identity.
    pub protected_accounts: Vec<AccountSecretSnapshot>,
    pub credential_generation: u64,
    pub attempt: u32,
}

#[derive(Clone, Debug)]
pub struct RevocationFenceClaim {
    pub owner_id: Uuid,
    pub claim_id: Uuid,
    pub protected_accounts: Vec<AccountSecretSnapshot>,
    pub credential_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleOAuthCleanupStatus {
    pub held: u64,
    pub pending: u64,
    pub retrying: u64,
    pub exhausted: u64,
    pub volatile_guardians: u64,
    pub durability_degraded: bool,
    pub revocation_fenced: bool,
    pub operator_recovery_required: bool,
    pub uncertain_authorizations: u64,
    pub legacy_recovery_required: u64,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OperatorRecoveryResult {
    pub accounts_marked_reauthorization_required: u64,
    pub legacy_accounts_finalized: u64,
}

#[derive(Clone, Debug)]
pub struct GoogleAccountMutation {
    pub account: GoogleAccount,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub enum DisconnectMutation {
    Execute(DisconnectClaim),
    Replay(GoogleAccount),
}

#[derive(Clone, Debug)]
pub struct OAuthIdempotency {
    pub namespace: &'static str,
    pub key_hash: SecretHash,
    pub request_fingerprint: SecretHash,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ClaimedOAuthSession {
    pub id: Uuid,
    pub owner_subject_hash: SecretHash,
    pub encrypted_verifier: SealedSecret,
    pub requested_scopes: BTreeSet<String>,
    pub make_default: bool,
    pub existing_account: Option<AccountSecretSnapshot>,
}

#[derive(Clone, Debug)]
pub struct AuthorizationCompletion {
    pub session_id: Uuid,
    pub owner_subject_hash: SecretHash,
    pub expected_account_revision: Option<u64>,
    pub account_id: Uuid,
    pub make_default: bool,
    pub external_account_id: String,
    pub display_label: String,
    pub credentials: EncryptedCredentials,
    pub granted_scopes: BTreeSet<String>,
    pub token_expires_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct DisconnectClaim {
    pub claim_id: Uuid,
    pub account: GoogleAccount,
    pub credentials: EncryptedCredentials,
    pub protected_accounts: Vec<AccountSecretSnapshot>,
    pub credential_generation: u64,
}

#[must_use]
pub fn hash_secret(value: &str) -> SecretHash {
    Sha256::digest(value.as_bytes()).into()
}

#[must_use]
pub fn oauth_session_aad(workspace_id: Uuid, user_id: Uuid, session_id: Uuid) -> Vec<u8> {
    format!("dayweave:v1:google:oauth-session:{workspace_id}:{user_id}:{session_id}:pkce")
        .into_bytes()
}

#[must_use]
pub fn oauth_authorization_url_aad(workspace_id: Uuid, user_id: Uuid, session_id: Uuid) -> Vec<u8> {
    format!(
        "dayweave:v1:google:oauth-session:{workspace_id}:{user_id}:{session_id}:authorization-url"
    )
    .into_bytes()
}

#[must_use]
pub fn oauth_cleanup_token_aad(workspace_id: Uuid, user_id: Uuid, session_id: Uuid) -> Vec<u8> {
    format!(
        "dayweave:v1:google:oauth-session:{workspace_id}:{user_id}:{session_id}:cleanup-refresh"
    )
    .into_bytes()
}

#[must_use]
pub fn account_credentials_aad(workspace_id: Uuid, user_id: Uuid, account_id: Uuid) -> Vec<u8> {
    format!("dayweave:v1:google:provider-account:{workspace_id}:{user_id}:{account_id}:credentials")
        .into_bytes()
}

#[must_use]
pub fn sync_cursor_aad(
    workspace_id: Uuid,
    user_id: Uuid,
    account_id: Uuid,
    collection_key: &str,
) -> Vec<u8> {
    format!("dayweave:v1:google:provider-account:{workspace_id}:{user_id}:{account_id}:cursor:{collection_key}")
        .into_bytes()
}
