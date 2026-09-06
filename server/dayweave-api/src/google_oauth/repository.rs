use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroize;

use super::domain::{
    AccountSecretSnapshot, AuthorizationCompletion, AuthorizationResolution, CallbackClaim,
    ClaimedOAuthSession, CleanupClaim, DisconnectClaim, DisconnectMutation, EncryptedCredentials,
    GoogleAccount, GoogleAccountMutation, GoogleAccountStatus, GoogleOAuthCleanupStatus,
    NewOAuthSession, OAuthIdempotency, OAuthSessionStart, OperatorRecoveryResult,
    RevocationFenceClaim, SecretHash,
};

pub(crate) const MAX_CLEANUP_ATTEMPTS: u32 = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OAuthScope {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GoogleOAuthRepositoryError {
    #[error("OAuth callback state is invalid, expired, or already used")]
    InvalidCallbackState,
    #[error("Google account was not found")]
    AccountNotFound,
    #[error("Google account revision conflict: expected {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("Google OAuth state already exists")]
    DuplicateState,
    #[error("another Google connection replaced this authorization attempt")]
    AuthorizationConflict,
    #[error("another Google authorization is currently exchanging credentials")]
    AuthorizationInProgress,
    #[error("Google credential cleanup revocation is in progress")]
    RevocationInProgress,
    #[error("Google account disconnect is already in progress")]
    DisconnectInProgress,
    #[error("Google account state does not allow this operation")]
    AccountStateConflict,
    #[error("Idempotency-Key was already used for different Google integration content")]
    IdempotencyConflict,
    #[error("matching Google integration request is still in progress")]
    IdempotencyInProgress,
    #[error("Google cleanup revocation lease was lost")]
    CleanupClaimLost,
    #[error("Google OAuth operator recovery is not currently required")]
    OperatorRecoveryNotRequired,
    #[error("Google OAuth repository operation failed")]
    Internal,
}

#[async_trait]
pub trait GoogleOAuthRepository: Send + Sync {
    async fn preflight_cleanup_storage(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn create_session(
        &self,
        session: NewOAuthSession,
        idempotency: OAuthIdempotency,
        exchange_stale_before: DateTime<Utc>,
    ) -> Result<OAuthSessionStart, GoogleOAuthRepositoryError>;

    /// Cancels only authorizations that have not started exchanging credentials.
    /// The deletion service must validate its owner confirmation and retain
    /// provider admission before calling this internal preparation primitive.
    /// Existing exchange/staging and cleanup custody are never discarded.
    async fn cancel_pending_authorizations_for_deletion(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, GoogleOAuthRepositoryError>;

    async fn claim_callback(
        &self,
        state_hash: SecretHash,
        now: DateTime<Utc>,
        _exchange_stale_before: DateTime<Utc>,
    ) -> Result<CallbackClaim, GoogleOAuthRepositoryError>;

    async fn stage_authorization(
        &self,
        completion: AuthorizationCompletion,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn hold_cleanup_token(
        &self,
        session_id: Uuid,
        encrypted_refresh_token: super::crypto::SealedSecret,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn identify_cleanup_token(
        &self,
        session_id: Uuid,
        external_account_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn resolve_authorization(
        &self,
        session_id: Uuid,
    ) -> Result<AuthorizationResolution, GoogleOAuthRepositoryError>;

    async fn complete_staged_authorization(
        &self,
        session_id: Uuid,
    ) -> Result<GoogleAccount, GoogleOAuthRepositoryError>;

    async fn reconcile_staged(&self) -> Result<Option<GoogleAccount>, GoogleOAuthRepositoryError>;

    async fn abandon_authorization(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn claim_cleanup(
        &self,
        claim_id: Uuid,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        only_session_id: Option<Uuid>,
    ) -> Result<Option<CleanupClaim>, GoogleOAuthRepositoryError>;

    async fn complete_cleanup(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn fail_cleanup(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn defer_cleanup_for_operator(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn cleanup_status(&self) -> Result<GoogleOAuthCleanupStatus, GoogleOAuthRepositoryError>;

    async fn claim_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<RevocationFenceClaim, GoogleOAuthRepositoryError>;

    async fn complete_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn release_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn recover_startup(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn acknowledge_operator_recovery(
        &self,
        now: DateTime<Utc>,
    ) -> Result<OperatorRecoveryResult, GoogleOAuthRepositoryError>;

    async fn fail_authorization(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;

    async fn account(&self) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError>;

    async fn account_by_id(
        &self,
        account_id: Uuid,
    ) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError>;

    async fn accounts(&self) -> Result<Vec<AccountSecretSnapshot>, GoogleOAuthRepositoryError>;

    async fn update_access_credentials(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        credentials: EncryptedCredentials,
        granted_scopes: std::collections::BTreeSet<String>,
        token_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<AccountSecretSnapshot, GoogleOAuthRepositoryError>;

    async fn set_paused(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        paused: bool,
        now: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError>;

    #[allow(clippy::too_many_arguments)]
    async fn claim_disconnect(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        claim_id: Uuid,
        now: DateTime<Utc>,
        disconnect_stale_before: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<DisconnectMutation, GoogleOAuthRepositoryError>;

    async fn complete_disconnect(
        &self,
        account_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError>;

    async fn fail_disconnect(
        &self,
        account_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError>;
}

#[derive(Clone, Debug)]
pub struct InMemoryGoogleOAuthRepository {
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryState {
    sessions: HashMap<SecretHash, MemorySession>,
    accounts: HashMap<Uuid, MemoryAccount>,
    cleanup: HashMap<Uuid, MemoryCleanup>,
    idempotency: HashMap<(String, SecretHash), MemoryIdempotency>,
    credential_generation: u64,
    revocation_fence: Option<MemoryRevocationFence>,
    guardian_resolutions: HashMap<(Uuid, Uuid), MemoryGuardianResolution>,
    #[cfg(test)]
    preflight_failures_remaining: usize,
    #[cfg(test)]
    hold_failures_remaining: usize,
    #[cfg(test)]
    volatile_claim_failures_remaining: usize,
}

#[derive(Clone, Copy, Debug)]
struct MemoryRevocationFence {
    kind: MemoryRevocationKind,
    owner_id: Uuid,
    claim_id: Uuid,
    credential_generation: u64,
    claimed_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryRevocationKind {
    Cleanup,
    Disconnect,
    Guardian,
    Recovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryGuardianOutcome {
    Revoked,
    Released,
}

#[derive(Clone, Copy, Debug)]
struct MemoryGuardianResolution {
    credential_generation: u64,
    outcome: MemoryGuardianOutcome,
}

#[derive(Clone, Debug)]
struct MemorySession {
    value: NewOAuthSession,
    status: MemorySessionStatus,
    exchange_started_at: Option<DateTime<Utc>>,
    staged: Option<AuthorizationCompletion>,
    consumed_account_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemorySessionStatus {
    Pending,
    Exchanging,
    Staged,
    Consumed,
    Failed,
}

#[derive(Clone, Debug)]
struct MemoryAccount {
    value: GoogleAccount,
    credentials: Option<EncryptedCredentials>,
    disconnect_claim: Option<(Uuid, DateTime<Utc>)>,
    disconnect_operation_hash: Option<SecretHash>,
}

#[derive(Clone, Debug)]
struct MemoryCleanup {
    encrypted_refresh_token: super::crypto::SealedSecret,
    external_account_id: Option<String>,
    status: MemoryCleanupStatus,
    attempt_count: u32,
    created_at: DateTime<Utc>,
    last_failure_at: Option<DateTime<Utc>>,
    next_attempt_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug)]
enum MemoryCleanupStatus {
    Held,
    Pending,
    OperatorRequired,
    Revoking {
        claim_id: Uuid,
        claimed_at: DateTime<Utc>,
    },
}

#[derive(Clone, Debug)]
struct MemoryIdempotency {
    fingerprint: SecretHash,
    expires_at: DateTime<Utc>,
    response: Option<MemoryReplay>,
}

#[derive(Clone, Debug)]
enum MemoryReplay {
    Session(OAuthSessionStart),
    CancelledSession,
    Account(GoogleAccount),
    DisconnectPending { account_id: Uuid },
}

impl Default for InMemoryGoogleOAuthRepository {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState::default())),
        }
    }
}

#[cfg(test)]
impl InMemoryGoogleOAuthRepository {
    pub(crate) async fn fail_next_preflights(&self, count: usize) {
        self.state.lock().await.preflight_failures_remaining = count;
    }

    pub(crate) async fn fail_next_holds(&self, count: usize) {
        self.state.lock().await.hold_failures_remaining = count;
    }

    pub(crate) async fn fail_next_volatile_claims(&self, count: usize) {
        self.state.lock().await.volatile_claim_failures_remaining = count;
    }
}

#[async_trait]
impl GoogleOAuthRepository for InMemoryGoogleOAuthRepository {
    async fn preflight_cleanup_storage(
        &self,
        session_id: Uuid,
        _now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        #[allow(unused_mut)] // mutated only by the test failure injector
        let mut state = self.state.lock().await;
        #[cfg(test)]
        if state.preflight_failures_remaining > 0 {
            state.preflight_failures_remaining -= 1;
            return Err(GoogleOAuthRepositoryError::Internal);
        }
        ensure_no_revocation_fence(&state)?;
        if state.sessions.values().any(|session| {
            session.value.id == session_id && session.status == MemorySessionStatus::Exchanging
        }) {
            Ok(())
        } else {
            Err(GoogleOAuthRepositoryError::InvalidCallbackState)
        }
    }

    async fn create_session(
        &self,
        session: NewOAuthSession,
        idempotency: OAuthIdempotency,
        exchange_stale_before: DateTime<Utc>,
    ) -> Result<OAuthSessionStart, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_no_revocation_fence(&state)?;
        if let Some(replay) = replay(&mut state, &idempotency, session.created_at)? {
            return match replay {
                MemoryReplay::Session(mut started) => {
                    started.replayed = true;
                    Ok(started)
                }
                MemoryReplay::CancelledSession => {
                    Err(GoogleOAuthRepositoryError::InvalidCallbackState)
                }
                _ => Err(GoogleOAuthRepositoryError::IdempotencyConflict),
            };
        }
        cleanup_sessions(&mut state, session.created_at, exchange_stale_before);
        if state.sessions.values().any(|existing| {
            matches!(
                existing.status,
                MemorySessionStatus::Exchanging | MemorySessionStatus::Staged
            )
        }) {
            return Err(GoogleOAuthRepositoryError::AuthorizationInProgress);
        }
        ensure_expected_account(
            &state,
            session.expected_account_id,
            session.expected_account_revision,
        )?;
        if session.expected_account_id.is_some_and(|id| {
            account_snapshot(&state, id).is_some_and(|snapshot| {
                snapshot.account.status == GoogleAccountStatus::Disconnecting
            })
        }) {
            return Err(GoogleOAuthRepositoryError::AccountStateConflict);
        }
        for existing in state
            .sessions
            .values_mut()
            .filter(|existing| existing.status == MemorySessionStatus::Pending)
        {
            fail_memory_session(existing);
        }
        if state.sessions.contains_key(&session.state_hash) {
            return Err(GoogleOAuthRepositoryError::DuplicateState);
        }
        let started = OAuthSessionStart {
            id: session.id,
            encrypted_authorization_url: session.encrypted_authorization_url.clone(),
            expires_at: session.expires_at,
            replayed: false,
        };
        state.sessions.insert(
            session.state_hash,
            MemorySession {
                value: session,
                status: MemorySessionStatus::Pending,
                exchange_started_at: None,
                staged: None,
                consumed_account_id: None,
            },
        );
        remember(
            &mut state,
            &idempotency,
            Some(MemoryReplay::Session(started.clone())),
        );
        Ok(started)
    }

    async fn cancel_pending_authorizations_for_deletion(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        let cancelled: Vec<Uuid> = state
            .sessions
            .values_mut()
            .filter(|session| session.status == MemorySessionStatus::Pending)
            .map(|session| {
                fail_memory_session(session);
                session.value.encrypted_verifier.key_version = 0;
                session
                    .value
                    .encrypted_authorization_url
                    .ciphertext
                    .zeroize();
                session.value.encrypted_authorization_url.ciphertext.clear();
                session.value.encrypted_authorization_url.key_version = 0;
                session.value.id
            })
            .collect();
        for entry in state.idempotency.values_mut() {
            if let Some(MemoryReplay::Session(started)) = entry.response.as_mut()
                && cancelled.contains(&started.id)
            {
                // The in-memory adapter retains a response copy, unlike the
                // PostgreSQL adapter's resource-only idempotency receipt.
                started.encrypted_authorization_url.ciphertext.zeroize();
                entry.response = Some(MemoryReplay::CancelledSession);
            }
        }
        for session_id in &cancelled {
            promote_memory_cleanup(&mut state, *session_id, now);
        }
        u64::try_from(cancelled.len()).map_err(|_| GoogleOAuthRepositoryError::Internal)
    }

    async fn claim_callback(
        &self,
        state_hash: SecretHash,
        now: DateTime<Utc>,
        _exchange_stale_before: DateTime<Utc>,
    ) -> Result<CallbackClaim, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_no_revocation_fence(&state)?;
        let Some(session) = state.sessions.get_mut(&state_hash) else {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        };
        if session.status == MemorySessionStatus::Staged {
            return Ok(CallbackClaim::Staged {
                session_id: session.value.id,
            });
        }
        if session.status == MemorySessionStatus::Exchanging {
            // A crashed or cancelled token exchange is ambiguous: Google may
            // have minted a grant even though no response was installed. Keep
            // the durable exchange marker for operator recovery.
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        if session.status != MemorySessionStatus::Pending || session.value.expires_at <= now {
            if session.status == MemorySessionStatus::Pending {
                let session_id = session.value.id;
                fail_memory_session(session);
                promote_memory_cleanup(&mut state, session_id, now);
            }
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        let expected_id = session.value.expected_account_id;
        let expected_revision = session.value.expected_account_revision;
        ensure_expected_account(&state, expected_id, expected_revision)?;
        let existing_account = expected_id.and_then(|id| account_snapshot(&state, id));
        let session = state
            .sessions
            .get_mut(&state_hash)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        session.status = MemorySessionStatus::Exchanging;
        session.exchange_started_at = Some(now);
        let value = session.value.clone();
        Ok(CallbackClaim::Exchange(Box::new(ClaimedOAuthSession {
            id: value.id,
            owner_subject_hash: value.owner_subject_hash,
            encrypted_verifier: value.encrypted_verifier,
            requested_scopes: value.requested_scopes,
            make_default: value.make_default,
            existing_account,
        })))
    }

    async fn stage_authorization(
        &self,
        completion: AuthorizationCompletion,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_no_revocation_fence(&state)?;
        if state.accounts.values().any(|account| {
            account.value.id != completion.account_id
                && account.value.status != GoogleAccountStatus::Revoked
                && account.value.external_account_id == completion.external_account_id
        }) {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        let session = state
            .sessions
            .values()
            .find(|session| session.value.id == completion.session_id)
            .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        if session.status != MemorySessionStatus::Exchanging
            || session.value.owner_subject_hash != completion.owner_subject_hash
            || session.value.make_default != completion.make_default
        {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        if let Some(expected_account_id) = session.value.expected_account_id
            && !state
                .accounts
                .get(&expected_account_id)
                .is_some_and(|account| {
                    account.value.status != GoogleAccountStatus::Revoked
                        && account.value.external_account_id == completion.external_account_id
                })
        {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        let session = state
            .sessions
            .values_mut()
            .find(|session| session.value.id == completion.session_id)
            .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        session.value.encrypted_verifier.ciphertext.zeroize();
        session.value.encrypted_verifier.ciphertext.clear();
        session.status = MemorySessionStatus::Staged;
        session.staged = Some(completion);
        Ok(())
    }

    async fn hold_cleanup_token(
        &self,
        session_id: Uuid,
        encrypted_refresh_token: super::crypto::SealedSecret,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_cleanup_hold_allowed(&state, session_id)?;
        #[cfg(test)]
        if state.hold_failures_remaining > 0 {
            state.hold_failures_remaining -= 1;
            return Err(GoogleOAuthRepositoryError::Internal);
        }
        if !state.sessions.values().any(|session| {
            session.value.id == session_id && session.status == MemorySessionStatus::Exchanging
        }) {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        if let Some(existing) = state.cleanup.get(&session_id) {
            return if existing.encrypted_refresh_token == encrypted_refresh_token {
                Ok(())
            } else {
                Err(GoogleOAuthRepositoryError::AuthorizationConflict)
            };
        }
        state.cleanup.insert(
            session_id,
            MemoryCleanup {
                encrypted_refresh_token,
                external_account_id: None,
                status: MemoryCleanupStatus::Held,
                attempt_count: 0,
                created_at: now,
                last_failure_at: None,
                next_attempt_at: now,
            },
        );
        Ok(())
    }

    async fn identify_cleanup_token(
        &self,
        session_id: Uuid,
        external_account_id: &str,
        _now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        if external_account_id.is_empty() || external_account_id.len() > 500 {
            return Err(GoogleOAuthRepositoryError::Internal);
        }
        let mut state = self.state.lock().await;
        let cleanup = state
            .cleanup
            .get_mut(&session_id)
            .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        match cleanup.external_account_id.as_deref() {
            Some(existing) if existing != external_account_id => {
                Err(GoogleOAuthRepositoryError::AuthorizationConflict)
            }
            Some(_) => Ok(()),
            None => {
                cleanup.external_account_id = Some(external_account_id.to_owned());
                Ok(())
            }
        }
    }

    async fn resolve_authorization(
        &self,
        session_id: Uuid,
    ) -> Result<AuthorizationResolution, GoogleOAuthRepositoryError> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .values()
            .find(|session| session.value.id == session_id)
            .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        match session.status {
            MemorySessionStatus::Staged => Ok(AuthorizationResolution::Staged),
            MemorySessionStatus::Consumed => session
                .consumed_account_id
                .and_then(|account_id| state.accounts.get(&account_id))
                .map(|account| AuthorizationResolution::Consumed(account.value.clone()))
                .ok_or(GoogleOAuthRepositoryError::Internal),
            MemorySessionStatus::Pending
            | MemorySessionStatus::Exchanging
            | MemorySessionStatus::Failed => Ok(AuthorizationResolution::NeverStaged),
        }
    }

    async fn complete_staged_authorization(
        &self,
        session_id: Uuid,
    ) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_no_revocation_fence(&state)?;
        complete_memory_staged(&mut state, session_id)
    }

    async fn reconcile_staged(&self) -> Result<Option<GoogleAccount>, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_no_revocation_fence(&state)?;
        let session_id = state
            .sessions
            .values()
            .find(|session| session.status == MemorySessionStatus::Staged)
            .map(|session| session.value.id);
        session_id
            .map(|id| complete_memory_staged(&mut state, id))
            .transpose()
    }

    async fn abandon_authorization(
        &self,
        session_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        let session = state
            .sessions
            .values_mut()
            .find(|session| session.value.id == session_id)
            .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
        if matches!(
            session.status,
            MemorySessionStatus::Exchanging | MemorySessionStatus::Staged
        ) {
            fail_memory_session(session);
        }
        if session.status == MemorySessionStatus::Failed {
            promote_memory_cleanup(&mut state, session_id, now);
        }
        Ok(())
    }

    async fn claim_cleanup(
        &self,
        claim_id: Uuid,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        only_session_id: Option<Uuid>,
    ) -> Result<Option<CleanupClaim>, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        cleanup_sessions(&mut state, now, exchange_stale_before);
        if state
            .revocation_fence
            .is_some_and(|fence| fence.kind != MemoryRevocationKind::Cleanup)
        {
            return Ok(None);
        }
        let fenced_session = state.revocation_fence.map(|fence| fence.owner_id);
        let session_id = state
            .cleanup
            .iter()
            .filter(|(session_id, cleanup)| {
                only_session_id.is_none_or(|only| only == **session_id)
                    && fenced_session.is_none_or(|fenced| fenced == **session_id)
                    && cleanup.attempt_count < MAX_CLEANUP_ATTEMPTS
                    && match cleanup.status {
                        MemoryCleanupStatus::Pending => cleanup.next_attempt_at <= now,
                        MemoryCleanupStatus::Revoking { claimed_at, .. } => {
                            claimed_at <= stale_before
                        }
                        MemoryCleanupStatus::Held | MemoryCleanupStatus::OperatorRequired => false,
                    }
            })
            .min_by_key(|(session_id, cleanup)| (cleanup.created_at, **session_id))
            .map(|(session_id, _)| *session_id);
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let protected_accounts = state
            .accounts
            .values()
            .filter_map(memory_snapshot)
            .collect();
        let credential_generation = state.credential_generation;
        let (encrypted_refresh_token, external_account_id, attempt) = {
            let cleanup = state
                .cleanup
                .get_mut(&session_id)
                .ok_or(GoogleOAuthRepositoryError::Internal)?;
            cleanup.attempt_count = cleanup
                .attempt_count
                .checked_add(1)
                .ok_or(GoogleOAuthRepositoryError::Internal)?;
            cleanup.status = MemoryCleanupStatus::Revoking {
                claim_id,
                claimed_at: now,
            };
            (
                cleanup.encrypted_refresh_token.clone(),
                cleanup.external_account_id.clone(),
                cleanup.attempt_count,
            )
        };
        state.revocation_fence = Some(MemoryRevocationFence {
            kind: MemoryRevocationKind::Cleanup,
            owner_id: session_id,
            claim_id,
            credential_generation,
            claimed_at: now,
        });
        Ok(Some(CleanupClaim {
            session_id,
            claim_id,
            encrypted_refresh_token,
            external_account_id,
            protected_accounts,
            credential_generation,
            attempt,
        }))
    }

    async fn complete_cleanup(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        _now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        let fence_matches = state.revocation_fence.is_some_and(|fence| {
            fence.kind == MemoryRevocationKind::Cleanup
                && fence.owner_id == session_id
                && fence.claim_id == claim_id
                && fence.credential_generation == credential_generation
                && state.credential_generation == credential_generation
        });
        let matches = fence_matches
            && state.cleanup.get(&session_id).is_some_and(|cleanup| {
                matches!(
                    cleanup.status,
                    MemoryCleanupStatus::Revoking {
                        claim_id: current,
                        ..
                    } if current == claim_id
                )
            });
        if !matches {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        state.cleanup.remove(&session_id);
        state.revocation_fence = None;
        Ok(())
    }

    async fn fail_cleanup(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        let fence_matches = state.revocation_fence.is_some_and(|fence| {
            fence.kind == MemoryRevocationKind::Cleanup
                && fence.owner_id == session_id
                && fence.claim_id == claim_id
                && fence.credential_generation == credential_generation
                && state.credential_generation == credential_generation
        });
        if !fence_matches {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        let cleanup = state
            .cleanup
            .get_mut(&session_id)
            .ok_or(GoogleOAuthRepositoryError::CleanupClaimLost)?;
        if !matches!(
            cleanup.status,
            MemoryCleanupStatus::Revoking {
                claim_id: current,
                ..
            } if current == claim_id
        ) {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        cleanup.status = MemoryCleanupStatus::Pending;
        cleanup.last_failure_at = Some(now);
        cleanup.next_attempt_at = next_attempt_at;
        Ok(())
    }

    async fn defer_cleanup_for_operator(
        &self,
        session_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_memory_fence(
            &state,
            MemoryRevocationKind::Cleanup,
            session_id,
            claim_id,
            credential_generation,
        )?;
        let cleanup = state
            .cleanup
            .get_mut(&session_id)
            .ok_or(GoogleOAuthRepositoryError::CleanupClaimLost)?;
        if !matches!(
            cleanup.status,
            MemoryCleanupStatus::Revoking {
                claim_id: current,
                ..
            } if current == claim_id
        ) {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        cleanup.status = MemoryCleanupStatus::OperatorRequired;
        cleanup.last_failure_at = Some(now);
        state.revocation_fence = Some(MemoryRevocationFence {
            kind: MemoryRevocationKind::Recovery,
            owner_id: session_id,
            claim_id,
            credential_generation,
            claimed_at: now,
        });
        Ok(())
    }

    async fn cleanup_status(&self) -> Result<GoogleOAuthCleanupStatus, GoogleOAuthRepositoryError> {
        let state = self.state.lock().await;
        let mut result = GoogleOAuthCleanupStatus {
            held: 0,
            pending: 0,
            retrying: 0,
            exhausted: 0,
            volatile_guardians: 0,
            durability_degraded: false,
            revocation_fenced: state.revocation_fence.is_some(),
            operator_recovery_required: state
                .revocation_fence
                .is_some_and(|fence| fence.kind == MemoryRevocationKind::Recovery),
            uncertain_authorizations: u64::try_from(
                state
                    .sessions
                    .values()
                    .filter(|session| session.status == MemorySessionStatus::Exchanging)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            legacy_recovery_required: 0,
            next_attempt_at: None,
            last_failure_at: None,
        };
        for cleanup in state.cleanup.values() {
            if cleanup.attempt_count >= MAX_CLEANUP_ATTEMPTS {
                result.exhausted += 1;
            }
            match cleanup.status {
                MemoryCleanupStatus::Held => result.held += 1,
                MemoryCleanupStatus::Pending => {
                    result.pending += 1;
                    if cleanup.attempt_count < MAX_CLEANUP_ATTEMPTS {
                        result.next_attempt_at = Some(
                            result
                                .next_attempt_at
                                .map_or(cleanup.next_attempt_at, |old| {
                                    old.min(cleanup.next_attempt_at)
                                }),
                        );
                    }
                }
                MemoryCleanupStatus::Revoking { .. } => result.retrying += 1,
                MemoryCleanupStatus::OperatorRequired => {
                    result.operator_recovery_required = true;
                }
            }
            result.last_failure_at = result.last_failure_at.max(cleanup.last_failure_at);
        }
        Ok(result)
    }

    async fn claim_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<RevocationFenceClaim, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        #[cfg(test)]
        if state.volatile_claim_failures_remaining > 0 {
            state.volatile_claim_failures_remaining -= 1;
            return Err(GoogleOAuthRepositoryError::Internal);
        }
        if let Some(fence) = state.revocation_fence {
            let exact_owner = fence.kind == MemoryRevocationKind::Guardian
                && fence.owner_id == owner_id
                && fence.claim_id == claim_id;
            let stale_guardian =
                fence.kind == MemoryRevocationKind::Guardian && fence.claimed_at <= stale_before;
            if !exact_owner && !stale_guardian {
                return Err(GoogleOAuthRepositoryError::RevocationInProgress);
            }
        }
        if !state
            .sessions
            .values()
            .any(|session| session.value.id == owner_id)
        {
            return Err(GoogleOAuthRepositoryError::InvalidCallbackState);
        }
        let credential_generation = state.credential_generation;
        let protected_accounts = state
            .accounts
            .values()
            .filter_map(memory_snapshot)
            .collect();
        state.revocation_fence = Some(MemoryRevocationFence {
            kind: MemoryRevocationKind::Guardian,
            owner_id,
            claim_id,
            credential_generation,
            claimed_at: now,
        });
        Ok(RevocationFenceClaim {
            owner_id,
            claim_id,
            protected_accounts,
            credential_generation,
        })
    }

    async fn complete_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        if state
            .guardian_resolutions
            .get(&(owner_id, claim_id))
            .is_some_and(|resolution| {
                resolution.credential_generation == credential_generation
                    && resolution.outcome == MemoryGuardianOutcome::Revoked
            })
        {
            return Ok(());
        }
        ensure_memory_fence(
            &state,
            MemoryRevocationKind::Guardian,
            owner_id,
            claim_id,
            credential_generation,
        )?;
        state.cleanup.remove(&owner_id);
        if let Some(session) = state
            .sessions
            .values_mut()
            .find(|session| session.value.id == owner_id)
        {
            fail_memory_session(session);
        }
        state.guardian_resolutions.insert(
            (owner_id, claim_id),
            MemoryGuardianResolution {
                credential_generation,
                outcome: MemoryGuardianOutcome::Revoked,
            },
        );
        state.revocation_fence = None;
        Ok(())
    }

    async fn release_volatile_revocation(
        &self,
        owner_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        if state
            .guardian_resolutions
            .get(&(owner_id, claim_id))
            .is_some_and(|resolution| {
                resolution.credential_generation == credential_generation
                    && resolution.outcome == MemoryGuardianOutcome::Released
            })
        {
            return Ok(());
        }
        ensure_memory_fence(
            &state,
            MemoryRevocationKind::Guardian,
            owner_id,
            claim_id,
            credential_generation,
        )?;
        state.guardian_resolutions.insert(
            (owner_id, claim_id),
            MemoryGuardianResolution {
                credential_generation,
                outcome: MemoryGuardianOutcome::Released,
            },
        );
        state.revocation_fence = None;
        Ok(())
    }

    async fn recover_startup(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(fence) = state.revocation_fence
            && fence.kind == MemoryRevocationKind::Guardian
            && fence.claimed_at <= stale_before
        {
            if state.cleanup.contains_key(&fence.owner_id) {
                if let Some(cleanup) = state.cleanup.get_mut(&fence.owner_id) {
                    cleanup.status = MemoryCleanupStatus::Pending;
                    cleanup.next_attempt_at = now;
                }
                if let Some(session) = state
                    .sessions
                    .values_mut()
                    .find(|session| session.value.id == fence.owner_id)
                {
                    fail_memory_session(session);
                }
                state.revocation_fence = None;
            } else if let Some(current) = state.revocation_fence.as_mut() {
                current.kind = MemoryRevocationKind::Recovery;
                current.claimed_at = now;
            }
        }
        if state.revocation_fence.is_none() {
            let stale_session_id = state
                .sessions
                .values()
                .find(|session| {
                    session.status == MemorySessionStatus::Exchanging
                        && session
                            .exchange_started_at
                            .is_some_and(|started| started <= stale_before)
                })
                .map(|session| session.value.id);
            if let Some(session_id) = stale_session_id {
                if state.cleanup.contains_key(&session_id) {
                    if let Some(session) = state
                        .sessions
                        .values_mut()
                        .find(|session| session.value.id == session_id)
                    {
                        fail_memory_session(session);
                    }
                    promote_memory_cleanup(&mut state, session_id, now);
                } else {
                    state.revocation_fence = Some(MemoryRevocationFence {
                        kind: MemoryRevocationKind::Recovery,
                        owner_id: session_id,
                        claim_id: Uuid::new_v4(),
                        credential_generation: state.credential_generation,
                        claimed_at: now,
                    });
                }
            }
        }
        Ok(())
    }

    async fn acknowledge_operator_recovery(
        &self,
        now: DateTime<Utc>,
    ) -> Result<OperatorRecoveryResult, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        let Some(fence) = state
            .revocation_fence
            .filter(|fence| fence.kind == MemoryRevocationKind::Recovery)
        else {
            return Err(GoogleOAuthRepositoryError::OperatorRecoveryNotRequired);
        };
        let mut affected = 0_u64;
        for account in state.accounts.values_mut() {
            if account.value.status != GoogleAccountStatus::Revoked {
                account.value.status = GoogleAccountStatus::ReauthorizationRequired;
                account.value.sync_enabled = false;
                bump(&mut account.value, now)?;
                affected = affected
                    .checked_add(1)
                    .ok_or(GoogleOAuthRepositoryError::Internal)?;
            }
        }
        state.cleanup.remove(&fence.owner_id);
        if let Some(session) = state
            .sessions
            .values_mut()
            .find(|session| session.value.id == fence.owner_id)
        {
            fail_memory_session(session);
        }
        state.credential_generation = state
            .credential_generation
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        state.revocation_fence = None;
        Ok(OperatorRecoveryResult {
            accounts_marked_reauthorization_required: affected,
            legacy_accounts_finalized: 0,
        })
    }

    async fn fail_authorization(
        &self,
        session_id: Uuid,
        _now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(session) = state
            .sessions
            .values_mut()
            .find(|session| session.value.id == session_id)
            && session.status == MemorySessionStatus::Exchanging
        {
            fail_memory_session(session);
        }
        Ok(())
    }

    async fn account(&self) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
        Ok(default_account(&*self.state.lock().await))
    }

    async fn account_by_id(
        &self,
        account_id: Uuid,
    ) -> Result<Option<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
        Ok(account_snapshot(&*self.state.lock().await, account_id))
    }

    async fn accounts(&self) -> Result<Vec<AccountSecretSnapshot>, GoogleOAuthRepositoryError> {
        let state = self.state.lock().await;
        let mut accounts = state
            .accounts
            .values()
            .filter_map(memory_snapshot)
            .collect::<Vec<_>>();
        accounts.sort_by_key(|snapshot| {
            (
                !snapshot.account.is_default,
                snapshot.account.created_at,
                snapshot.account.id,
            )
        });
        Ok(accounts)
    }

    async fn update_access_credentials(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        credentials: EncryptedCredentials,
        granted_scopes: std::collections::BTreeSet<String>,
        token_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<AccountSecretSnapshot, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_no_open_authorization(&state)?;
        ensure_no_revocation_fence(&state)?;
        let snapshot = {
            let account = account_mut(&mut state, account_id)?;
            check_revision(account, expected_revision)?;
            if account.value.status != GoogleAccountStatus::Active || !account.value.sync_enabled {
                return Err(GoogleOAuthRepositoryError::AccountStateConflict);
            }
            account.credentials = Some(credentials);
            account.value.granted_scopes = granted_scopes;
            account.value.token_expires_at = Some(token_expires_at);
            bump(&mut account.value, now)?;
            memory_snapshot(account).ok_or(GoogleOAuthRepositoryError::Internal)?
        };
        state.credential_generation = state
            .credential_generation
            .checked_add(1)
            .ok_or(GoogleOAuthRepositoryError::Internal)?;
        Ok(snapshot)
    }

    async fn set_paused(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        paused: bool,
        now: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(replay) = replay(&mut state, &idempotency, now)? {
            return account_replay(replay);
        }
        cleanup_sessions(&mut state, now, exchange_stale_before);
        ensure_no_open_authorization(&state)?;
        ensure_no_revocation_fence(&state)?;
        let account = account_mut(&mut state, account_id)?;
        check_revision(account, expected_revision)?;
        let required_status = if paused {
            GoogleAccountStatus::Active
        } else {
            GoogleAccountStatus::Paused
        };
        if account.value.status != required_status {
            return Err(GoogleOAuthRepositoryError::AccountStateConflict);
        }
        account.value.status = if paused {
            GoogleAccountStatus::Paused
        } else {
            GoogleAccountStatus::Active
        };
        account.value.sync_enabled = !paused;
        bump(&mut account.value, now)?;
        let result = account.value.clone();
        remember(
            &mut state,
            &idempotency,
            Some(MemoryReplay::Account(result.clone())),
        );
        Ok(GoogleAccountMutation {
            account: result,
            replayed: false,
        })
    }

    async fn claim_disconnect(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        claim_id: Uuid,
        now: DateTime<Utc>,
        disconnect_stale_before: DateTime<Utc>,
        exchange_stale_before: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<DisconnectMutation, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        let replayed = replay(&mut state, &idempotency, now)?;
        if let Some(MemoryReplay::Account(account)) = replayed {
            return Ok(DisconnectMutation::Replay(account));
        }
        cleanup_sessions(&mut state, now, exchange_stale_before);
        ensure_no_open_authorization(&state)?;
        let idempotency_retry = matches!(
            replayed,
            Some(MemoryReplay::DisconnectPending { account_id: replay_id }) if replay_id == account_id
        );
        if replayed.is_some() && !idempotency_retry {
            return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
        }
        let matching_disconnect_fence = state.revocation_fence.is_some_and(|fence| {
            fence.kind == MemoryRevocationKind::Disconnect && fence.owner_id == account_id
        });
        // A failed provider revocation deliberately retains its account operation
        // hash and scope fence beyond the ordinary idempotency TTL. Reconstruct
        // only that exact key/account operation; the fence still rejects every
        // other absent key.
        let recovered_expired_retry = replayed.is_none()
            && matching_disconnect_fence
            && state.accounts.get(&account_id).is_some_and(|account| {
                account.disconnect_operation_hash == Some(idempotency.key_hash)
            });
        let retry = idempotency_retry || recovered_expired_retry;
        if state.revocation_fence.is_some_and(|fence| {
            !retry || fence.kind != MemoryRevocationKind::Disconnect || fence.owner_id != account_id
        }) {
            return Err(GoogleOAuthRepositoryError::RevocationInProgress);
        }
        let (credentials, claimed_account) = {
            let account = account_mut(&mut state, account_id)?;
            if retry {
                if account.disconnect_operation_hash != Some(idempotency.key_hash) {
                    return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
                }
                if account.value.status == GoogleAccountStatus::Disconnecting
                    && account
                        .disconnect_claim
                        .is_some_and(|(_, claimed_at)| claimed_at > disconnect_stale_before)
                {
                    return Err(GoogleOAuthRepositoryError::IdempotencyInProgress);
                }
                if !matches!(
                    account.value.status,
                    GoogleAccountStatus::Disconnecting | GoogleAccountStatus::RevocationFailed
                ) {
                    return Err(GoogleOAuthRepositoryError::AccountStateConflict);
                }
            } else {
                check_revision(account, expected_revision)?;
                if account.value.status == GoogleAccountStatus::Disconnecting
                    && account
                        .disconnect_claim
                        .is_some_and(|(_, claimed_at)| claimed_at > disconnect_stale_before)
                {
                    return Err(GoogleOAuthRepositoryError::DisconnectInProgress);
                }
            }
            let credentials = account
                .credentials
                .clone()
                .ok_or(GoogleOAuthRepositoryError::AccountNotFound)?;
            account.value.status = GoogleAccountStatus::Disconnecting;
            account.value.sync_enabled = false;
            bump(&mut account.value, now)?;
            account.disconnect_claim = Some((claim_id, now));
            account.disconnect_operation_hash = Some(idempotency.key_hash);
            (credentials, account.value.clone())
        };
        if !idempotency_retry {
            remember(
                &mut state,
                &idempotency,
                Some(MemoryReplay::DisconnectPending { account_id }),
            );
        }
        let protected_accounts = state
            .accounts
            .values()
            .filter_map(memory_snapshot)
            .collect();
        let credential_generation = state.credential_generation;
        state.revocation_fence = Some(MemoryRevocationFence {
            kind: MemoryRevocationKind::Disconnect,
            owner_id: account_id,
            claim_id,
            credential_generation,
            claimed_at: now,
        });
        Ok(DisconnectMutation::Execute(DisconnectClaim {
            claim_id,
            account: claimed_account,
            credentials,
            protected_accounts,
            credential_generation,
        }))
    }

    async fn complete_disconnect(
        &self,
        account_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
        idempotency: OAuthIdempotency,
    ) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        if let Some(MemoryReplay::Account(account)) = replay(&mut state, &idempotency, now)? {
            return Ok(GoogleAccountMutation {
                account,
                replayed: true,
            });
        }
        ensure_memory_fence(
            &state,
            MemoryRevocationKind::Disconnect,
            account_id,
            claim_id,
            credential_generation,
        )?;
        let account = account_mut(&mut state, account_id)?;
        if account.disconnect_claim.map(|claim| claim.0) != Some(claim_id)
            || account.disconnect_operation_hash != Some(idempotency.key_hash)
        {
            return Err(GoogleOAuthRepositoryError::DisconnectInProgress);
        }
        let was_default = account.value.is_default;
        account.value.status = GoogleAccountStatus::Revoked;
        account.value.sync_enabled = false;
        account.value.is_default = false;
        account.value.granted_scopes.clear();
        account.value.token_expires_at = None;
        bump(&mut account.value, now)?;
        account.credentials = None;
        account.disconnect_claim = None;
        account.disconnect_operation_hash = None;
        let result = account.value.clone();
        if was_default {
            promote_memory_default(&mut state, now)?;
        }
        let entry = state
            .idempotency
            .get_mut(&(idempotency.namespace.to_owned(), idempotency.key_hash))
            .ok_or(GoogleOAuthRepositoryError::IdempotencyConflict)?;
        if entry.fingerprint != idempotency.request_fingerprint {
            return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
        }
        entry.response = Some(MemoryReplay::Account(result.clone()));
        state.revocation_fence = None;
        Ok(GoogleAccountMutation {
            account: result,
            replayed: false,
        })
    }

    async fn fail_disconnect(
        &self,
        account_id: Uuid,
        claim_id: Uuid,
        credential_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthRepositoryError> {
        let mut state = self.state.lock().await;
        ensure_memory_fence(
            &state,
            MemoryRevocationKind::Disconnect,
            account_id,
            claim_id,
            credential_generation,
        )?;
        let account = account_mut(&mut state, account_id)?;
        if account.disconnect_claim.map(|claim| claim.0) != Some(claim_id) {
            return Err(GoogleOAuthRepositoryError::CleanupClaimLost);
        }
        account.value.status = GoogleAccountStatus::RevocationFailed;
        account.value.sync_enabled = false;
        bump(&mut account.value, now)?;
        account.disconnect_claim = None;
        Ok(())
    }
}

fn memory_snapshot(account: &MemoryAccount) -> Option<AccountSecretSnapshot> {
    (account.value.status != GoogleAccountStatus::Revoked)
        .then(|| {
            account
                .credentials
                .clone()
                .map(|credentials| AccountSecretSnapshot {
                    account: account.value.clone(),
                    credentials,
                })
        })
        .flatten()
}

fn account_snapshot(state: &MemoryState, account_id: Uuid) -> Option<AccountSecretSnapshot> {
    state.accounts.get(&account_id).and_then(memory_snapshot)
}

fn default_account(state: &MemoryState) -> Option<AccountSecretSnapshot> {
    state
        .accounts
        .values()
        .find(|account| account.value.is_default)
        .and_then(memory_snapshot)
}

fn ensure_expected_account(
    state: &MemoryState,
    expected_id: Option<Uuid>,
    expected_revision: Option<u64>,
) -> Result<(), GoogleOAuthRepositoryError> {
    let matches = match (expected_id, expected_revision) {
        (None, None) => true,
        (Some(id), Some(revision)) => {
            account_snapshot(state, id).is_some_and(|current| current.account.revision == revision)
        }
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::AuthorizationConflict)
    }
}

fn ensure_no_revocation_fence(state: &MemoryState) -> Result<(), GoogleOAuthRepositoryError> {
    if state.revocation_fence.is_some() {
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    } else {
        Ok(())
    }
}

fn ensure_cleanup_hold_allowed(
    state: &MemoryState,
    session_id: Uuid,
) -> Result<(), GoogleOAuthRepositoryError> {
    match state.revocation_fence {
        None => Ok(()),
        Some(fence)
            if fence.kind == MemoryRevocationKind::Guardian && fence.owner_id == session_id =>
        {
            Ok(())
        }
        Some(_) => Err(GoogleOAuthRepositoryError::RevocationInProgress),
    }
}

fn ensure_memory_fence(
    state: &MemoryState,
    kind: MemoryRevocationKind,
    owner_id: Uuid,
    claim_id: Uuid,
    credential_generation: u64,
) -> Result<(), GoogleOAuthRepositoryError> {
    if state.revocation_fence.is_some_and(|fence| {
        fence.kind == kind
            && fence.owner_id == owner_id
            && fence.claim_id == claim_id
            && fence.credential_generation == credential_generation
            && state.credential_generation == credential_generation
    }) {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    }
}

fn cleanup_sessions(
    state: &mut MemoryState,
    now: DateTime<Utc>,
    _exchange_stale_before: DateTime<Utc>,
) {
    let mut failed_session_ids = Vec::new();
    for session in state.sessions.values_mut() {
        let expired_pending =
            session.status == MemorySessionStatus::Pending && session.value.expires_at <= now;
        if expired_pending {
            failed_session_ids.push(session.value.id);
            fail_memory_session(session);
        }
    }
    for session_id in failed_session_ids {
        if let Some(cleanup) = state.cleanup.get_mut(&session_id)
            && matches!(cleanup.status, MemoryCleanupStatus::Held)
        {
            cleanup.status = MemoryCleanupStatus::Pending;
            cleanup.next_attempt_at = now;
        }
    }
}

fn fail_memory_session(session: &mut MemorySession) {
    session.value.encrypted_verifier.ciphertext.zeroize();
    session.value.encrypted_verifier.ciphertext.clear();
    session.status = MemorySessionStatus::Failed;
    session.staged = None;
}

fn promote_memory_cleanup(state: &mut MemoryState, session_id: Uuid, now: DateTime<Utc>) {
    if let Some(cleanup) = state.cleanup.get_mut(&session_id)
        && matches!(cleanup.status, MemoryCleanupStatus::Held)
    {
        cleanup.status = MemoryCleanupStatus::Pending;
        cleanup.next_attempt_at = now;
    }
}

fn ensure_no_open_authorization(state: &MemoryState) -> Result<(), GoogleOAuthRepositoryError> {
    if state.sessions.values().any(|session| {
        matches!(
            session.status,
            MemorySessionStatus::Pending
                | MemorySessionStatus::Exchanging
                | MemorySessionStatus::Staged
        )
    }) {
        Err(GoogleOAuthRepositoryError::AuthorizationInProgress)
    } else {
        Ok(())
    }
}

fn complete_memory_staged(
    state: &mut MemoryState,
    session_id: Uuid,
) -> Result<GoogleAccount, GoogleOAuthRepositoryError> {
    let session_key = *state
        .sessions
        .iter()
        .find(|(_, session)| session.value.id == session_id)
        .map(|(key, _)| key)
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
    let session = state
        .sessions
        .get(&session_key)
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
    if session.status == MemorySessionStatus::Consumed {
        return session
            .consumed_account_id
            .and_then(|account_id| state.accounts.get(&account_id))
            .map(|account| account.value.clone())
            .ok_or(GoogleOAuthRepositoryError::Internal);
    }
    let completion = session
        .staged
        .clone()
        .filter(|_| session.status == MemorySessionStatus::Staged)
        .ok_or(GoogleOAuthRepositoryError::InvalidCallbackState)?;
    if state.accounts.values().any(|existing| {
        existing.value.id != completion.account_id
            && existing.value.status != GoogleAccountStatus::Revoked
            && existing.value.external_account_id == completion.external_account_id
    }) {
        return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
    }
    let next = if let Some(expected) = completion.expected_account_revision {
        let existing = state
            .accounts
            .get(&completion.account_id)
            .filter(|existing| existing.value.status != GoogleAccountStatus::Revoked)
            .ok_or(GoogleOAuthRepositoryError::AuthorizationConflict)?;
        if existing.value.revision != expected {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        let mut account = existing.value.clone();
        account.external_account_id = completion.external_account_id;
        account.display_label = completion.display_label;
        account.status = GoogleAccountStatus::Active;
        account.sync_enabled = true;
        account.is_default |= completion.make_default;
        account.granted_scopes = completion.granted_scopes;
        account.token_expires_at = Some(completion.token_expires_at);
        bump(&mut account, completion.now)?;
        account
    } else {
        if state.accounts.contains_key(&completion.account_id) {
            return Err(GoogleOAuthRepositoryError::AuthorizationConflict);
        }
        GoogleAccount {
            id: completion.account_id,
            external_account_id: completion.external_account_id,
            display_label: completion.display_label,
            status: GoogleAccountStatus::Active,
            sync_enabled: true,
            is_default: completion.make_default || default_account(state).is_none(),
            granted_scopes: completion.granted_scopes,
            token_expires_at: Some(completion.token_expires_at),
            revision: 1,
            created_at: completion.now,
            updated_at: completion.now,
        }
    };
    if next.is_default {
        for account in state.accounts.values_mut() {
            if account.value.id != next.id && account.value.is_default {
                account.value.is_default = false;
                bump(&mut account.value, completion.now)?;
            }
        }
    }
    state.accounts.insert(
        next.id,
        MemoryAccount {
            value: next.clone(),
            credentials: Some(completion.credentials),
            disconnect_claim: None,
            disconnect_operation_hash: None,
        },
    );
    state.credential_generation = state
        .credential_generation
        .checked_add(1)
        .ok_or(GoogleOAuthRepositoryError::Internal)?;
    state.cleanup.remove(&session_id);
    let session = state
        .sessions
        .get_mut(&session_key)
        .ok_or(GoogleOAuthRepositoryError::Internal)?;
    session.staged = None;
    session.consumed_account_id = Some(next.id);
    session.status = MemorySessionStatus::Consumed;
    Ok(next)
}

fn promote_memory_default(
    state: &mut MemoryState,
    now: DateTime<Utc>,
) -> Result<(), GoogleOAuthRepositoryError> {
    let next = state
        .accounts
        .values()
        .filter(|account| account.value.status != GoogleAccountStatus::Revoked)
        .min_by_key(|account| (account.value.created_at, account.value.id))
        .map(|account| account.value.id);
    if let Some(next) = next
        && let Some(account) = state.accounts.get_mut(&next)
    {
        account.value.is_default = true;
        bump(&mut account.value, now)?;
    }
    Ok(())
}

fn replay(
    state: &mut MemoryState,
    idempotency: &OAuthIdempotency,
    now: DateTime<Utc>,
) -> Result<Option<MemoryReplay>, GoogleOAuthRepositoryError> {
    let key = (idempotency.namespace.to_owned(), idempotency.key_hash);
    if state
        .idempotency
        .get(&key)
        .is_some_and(|entry| entry.expires_at <= now)
    {
        state.idempotency.remove(&key);
    }
    let Some(entry) = state.idempotency.get(&key) else {
        return Ok(None);
    };
    if entry.fingerprint != idempotency.request_fingerprint {
        return Err(GoogleOAuthRepositoryError::IdempotencyConflict);
    }
    Ok(entry.response.clone())
}

fn remember(
    state: &mut MemoryState,
    idempotency: &OAuthIdempotency,
    response: Option<MemoryReplay>,
) {
    state.idempotency.insert(
        (idempotency.namespace.to_owned(), idempotency.key_hash),
        MemoryIdempotency {
            fingerprint: idempotency.request_fingerprint,
            expires_at: idempotency.expires_at,
            response,
        },
    );
}

fn account_replay(
    replay: MemoryReplay,
) -> Result<GoogleAccountMutation, GoogleOAuthRepositoryError> {
    match replay {
        MemoryReplay::Account(account) => Ok(GoogleAccountMutation {
            account,
            replayed: true,
        }),
        _ => Err(GoogleOAuthRepositoryError::IdempotencyConflict),
    }
}

fn account_mut(
    state: &mut MemoryState,
    account_id: Uuid,
) -> Result<&mut MemoryAccount, GoogleOAuthRepositoryError> {
    state
        .accounts
        .get_mut(&account_id)
        .filter(|account| account.value.status != GoogleAccountStatus::Revoked)
        .ok_or(GoogleOAuthRepositoryError::AccountNotFound)
}

fn check_revision(
    account: &MemoryAccount,
    expected: u64,
) -> Result<(), GoogleOAuthRepositoryError> {
    if account.value.revision == expected {
        Ok(())
    } else {
        Err(GoogleOAuthRepositoryError::RevisionConflict {
            expected,
            actual: account.value.revision,
        })
    }
}

fn bump(account: &mut GoogleAccount, now: DateTime<Utc>) -> Result<(), GoogleOAuthRepositoryError> {
    account.revision = account
        .revision
        .checked_add(1)
        .ok_or(GoogleOAuthRepositoryError::Internal)?;
    account.updated_at = now;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google_oauth::SealedSecret;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_788_739_200, 0).expect("synthetic test time")
    }

    fn session() -> NewOAuthSession {
        NewOAuthSession {
            id: Uuid::new_v4(),
            owner_subject_hash: [81; 32],
            state_hash: [82; 32],
            encrypted_verifier: SealedSecret {
                key_version: 9,
                ciphertext: vec![83; 48],
            },
            encrypted_authorization_url: SealedSecret {
                key_version: 10,
                ciphertext: vec![84; 48],
            },
            requested_scopes: ["openid".to_owned()].into_iter().collect(),
            expected_account_id: None,
            expected_account_revision: None,
            make_default: false,
            created_at: now(),
            expires_at: now() + Duration::minutes(15),
        }
    }

    fn idempotency() -> OAuthIdempotency {
        OAuthIdempotency {
            namespace: "deletion_pending_cancellation_test",
            key_hash: [85; 32],
            request_fingerprint: [86; 32],
            expires_at: now() + Duration::hours(1),
        }
    }

    fn cleanup_credential() -> SealedSecret {
        SealedSecret {
            key_version: 11,
            ciphertext: vec![87; 48],
        }
    }

    async fn start(repository: &InMemoryGoogleOAuthRepository, session: &NewOAuthSession) {
        repository
            .create_session(session.clone(), idempotency(), now() - Duration::minutes(5))
            .await
            .expect("pending authorization fixture");
    }

    #[tokio::test]
    async fn deletion_cancellation_scrubs_pending_and_cached_url_without_discarding_cleanup() {
        let repository = InMemoryGoogleOAuthRepository::default();
        let session = session();
        start(&repository, &session).await;
        // A retained legacy/defensive custody row must survive even if its
        // associated session is still pending rather than exchanging.
        repository.state.lock().await.cleanup.insert(
            session.id,
            MemoryCleanup {
                encrypted_refresh_token: cleanup_credential(),
                external_account_id: Some("synthetic-google-subject".to_owned()),
                status: MemoryCleanupStatus::Held,
                attempt_count: 3,
                created_at: now(),
                last_failure_at: Some(now()),
                next_attempt_at: now() + Duration::hours(1),
            },
        );
        let cancelled_at = now() + Duration::seconds(10);
        assert_eq!(
            repository
                .cancel_pending_authorizations_for_deletion(cancelled_at)
                .await
                .expect("cancel pending authorization"),
            1
        );
        assert_eq!(
            repository
                .clone()
                .cancel_pending_authorizations_for_deletion(cancelled_at + Duration::seconds(10))
                .await
                .expect("repeat cancellation"),
            0
        );
        {
            let state = repository.state.lock().await;
            let retained = &state.sessions[&session.state_hash];
            assert_eq!(retained.status, MemorySessionStatus::Failed);
            assert!(retained.value.encrypted_verifier.ciphertext.is_empty());
            assert_eq!(retained.value.encrypted_verifier.key_version, 0);
            assert!(
                retained
                    .value
                    .encrypted_authorization_url
                    .ciphertext
                    .is_empty()
            );
            assert_eq!(retained.value.encrypted_authorization_url.key_version, 0);
            assert!(retained.exchange_started_at.is_none());
            assert!(retained.staged.is_none());
            assert!(matches!(
                state
                    .idempotency
                    .values()
                    .next()
                    .expect("retained request identity")
                    .response,
                Some(MemoryReplay::CancelledSession)
            ));
            let cleanup = &state.cleanup[&session.id];
            assert_eq!(cleanup.encrypted_refresh_token, cleanup_credential());
            assert_eq!(
                cleanup.external_account_id.as_deref(),
                Some("synthetic-google-subject")
            );
            assert!(matches!(cleanup.status, MemoryCleanupStatus::Pending));
            assert_eq!(cleanup.next_attempt_at, cancelled_at);
            assert_eq!(cleanup.attempt_count, 3);
            assert_eq!(cleanup.created_at, now());
            assert_eq!(cleanup.last_failure_at, Some(now()));
        }
        assert!(matches!(
            repository
                .claim_callback(session.state_hash, cancelled_at, now())
                .await,
            Err(GoogleOAuthRepositoryError::InvalidCallbackState)
        ));
        assert!(matches!(
            repository
                .create_session(session.clone(), idempotency(), now())
                .await,
            Err(GoogleOAuthRepositoryError::InvalidCallbackState)
        ));
        let mut conflicting = idempotency();
        conflicting.request_fingerprint = [88; 32];
        assert!(matches!(
            repository.create_session(session, conflicting, now()).await,
            Err(GoogleOAuthRepositoryError::IdempotencyConflict)
        ));
    }

    #[tokio::test]
    async fn deletion_cancellation_is_isolated_to_its_repository_scope() {
        let selected = InMemoryGoogleOAuthRepository::default();
        let other = InMemoryGoogleOAuthRepository::default();
        let session = session();
        start(&selected, &session).await;
        start(&other, &session).await;
        assert_eq!(
            selected
                .cancel_pending_authorizations_for_deletion(now())
                .await
                .unwrap(),
            1
        );
        {
            let state = other.state.lock().await;
            let retained = &state.sessions[&session.state_hash];
            assert_eq!(retained.status, MemorySessionStatus::Pending);
            assert_eq!(
                retained.value.encrypted_verifier,
                session.encrypted_verifier
            );
            assert_eq!(
                retained.value.encrypted_authorization_url,
                session.encrypted_authorization_url
            );
        }
        assert!(matches!(
            other
                .claim_callback(session.state_hash, now(), now())
                .await
                .unwrap(),
            CallbackClaim::Exchange(_)
        ));
    }

    #[tokio::test]
    async fn deletion_cancellation_preserves_exchanging_and_staged_credentials() {
        for staged in [false, true] {
            let repository = InMemoryGoogleOAuthRepository::default();
            let session = session();
            start(&repository, &session).await;
            repository
                .claim_callback(session.state_hash, now(), now())
                .await
                .unwrap();
            repository
                .hold_cleanup_token(session.id, cleanup_credential(), now())
                .await
                .unwrap();
            let staged_credentials = EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 12,
                    ciphertext: vec![89; 48],
                },
            };
            if staged {
                repository
                    .stage_authorization(AuthorizationCompletion {
                        session_id: session.id,
                        owner_subject_hash: session.owner_subject_hash,
                        expected_account_revision: None,
                        account_id: Uuid::new_v4(),
                        make_default: session.make_default,
                        external_account_id: "synthetic-google-subject".to_owned(),
                        display_label: "Synthetic account".to_owned(),
                        credentials: staged_credentials.clone(),
                        granted_scopes: session.requested_scopes.clone(),
                        token_expires_at: now() + Duration::hours(1),
                        now: now(),
                    })
                    .await
                    .unwrap();
            }
            assert_eq!(
                repository
                    .cancel_pending_authorizations_for_deletion(now() + Duration::days(1))
                    .await
                    .unwrap(),
                0
            );
            let state = repository.state.lock().await;
            let retained = &state.sessions[&session.state_hash];
            assert_eq!(
                retained.value.encrypted_authorization_url,
                session.encrypted_authorization_url
            );
            assert_eq!(retained.exchange_started_at, Some(now()));
            if staged {
                assert_eq!(retained.status, MemorySessionStatus::Staged);
                assert_eq!(
                    retained.staged.as_ref().unwrap().credentials.sealed,
                    staged_credentials.sealed
                );
            } else {
                assert_eq!(retained.status, MemorySessionStatus::Exchanging);
                assert_eq!(
                    retained.value.encrypted_verifier,
                    session.encrypted_verifier
                );
                assert!(retained.staged.is_none());
            }
            let cleanup = &state.cleanup[&session.id];
            assert!(matches!(cleanup.status, MemoryCleanupStatus::Held));
            assert_eq!(cleanup.encrypted_refresh_token, cleanup_credential());
            assert_eq!(cleanup.next_attempt_at, now());
            assert_eq!(state.credential_generation, 0);
            assert!(state.revocation_fence.is_none());
        }
    }
}
