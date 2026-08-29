use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, Weak},
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use dayweave_google::GoogleError;
use dayweave_google::oauth::{
    AuthorizationOptions, AuthorizationSession, OAuthClient, OAuthTokenSet,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    config::{
        GOOGLE_CALENDAR_READONLY_SCOPE, GOOGLE_CALENDAR_SCOPE, GOOGLE_EMAIL_SCOPE,
        GOOGLE_OPENID_SCOPE, GOOGLE_TASKS_READONLY_SCOPE, GOOGLE_TASKS_SCOPE,
    },
    proposals::Clock,
    readiness::Readiness,
};

use super::{
    crypto::{CryptoError, SecretCipher, erase},
    domain::{
        AuthorizationCompletion, AuthorizationResolution, CallbackClaim, DisconnectMutation,
        GoogleAccount, GoogleAccountMutation, GoogleAccountStatus, GoogleOAuthCleanupStatus,
        OAuthIdempotency, OperatorRecoveryResult, RevocationFenceClaim, StoredCredentials,
        account_credentials_aad, hash_secret, oauth_authorization_url_aad, oauth_cleanup_token_aad,
        oauth_session_aad,
    },
    repository::{GoogleOAuthRepository, GoogleOAuthRepositoryError, OAuthScope},
};

const GOOGLE_EXCHANGE_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const GOOGLE_IDENTITY_TIMEOUT: StdDuration = StdDuration::from_secs(20);
const GOOGLE_REVOCATION_TIMEOUT: StdDuration = StdDuration::from_secs(20);
const EXCHANGE_LEASE: TimeDelta = TimeDelta::minutes(2);
const DISCONNECT_LEASE: TimeDelta = TimeDelta::minutes(2);
// Longer than the bounded provider revocation request, so stealing a crashed
// guardian cannot overlap a still-live outbound revoke from its predecessor.
const GUARDIAN_LEASE: TimeDelta = TimeDelta::minutes(2);
const MUTATION_IDEMPOTENCY_TTL: TimeDelta = TimeDelta::hours(24);
const MAX_RETIRED_REFRESH_TOKENS: usize = 32;
const CLEANUP_BACKOFF_CAP_SECONDS: i64 = 3_600;
const HOLD_RETRY_DELAYS: [StdDuration; 4] = [
    StdDuration::from_millis(50),
    StdDuration::from_millis(100),
    StdDuration::from_millis(200),
    StdDuration::from_millis(400),
];
const MAX_VOLATILE_GUARDIANS: usize = 8;
const STARTUP_RECOVERY_INTERVAL: StdDuration = StdDuration::from_secs(15);

#[derive(Default)]
struct GuardianRegistry {
    sessions: Mutex<HashMap<Uuid, [u8; 32]>>,
    readiness: Mutex<Option<Readiness>>,
}

impl GuardianRegistry {
    fn set_readiness(&self, readiness: Readiness) {
        *self.readiness.lock().expect("guardian readiness lock") = Some(readiness);
    }

    fn register(&self, session_id: Uuid, token_hash: [u8; 32]) -> GuardianRegistration {
        let mut sessions = self.sessions.lock().expect("guardian registry lock");
        if let Some(existing) = sessions.get(&session_id) {
            return if existing == &token_hash {
                GuardianRegistration::AlreadyOwned
            } else {
                GuardianRegistration::Rejected
            };
        }
        if sessions.len() >= MAX_VOLATILE_GUARDIANS {
            return GuardianRegistration::Rejected;
        }
        sessions.insert(session_id, token_hash);
        if let Some(readiness) = self
            .readiness
            .lock()
            .expect("guardian readiness lock")
            .as_ref()
        {
            readiness.add_durability_blocker();
        }
        GuardianRegistration::Registered
    }

    fn finish(&self, session_id: Uuid) {
        if self
            .sessions
            .lock()
            .expect("guardian registry lock")
            .remove(&session_id)
            .is_some()
            && let Some(readiness) = self
                .readiness
                .lock()
                .expect("guardian readiness lock")
                .as_ref()
        {
            readiness.remove_durability_blocker();
        }
    }

    fn count(&self) -> u64 {
        u64::try_from(self.sessions.lock().expect("guardian registry lock").len())
            .unwrap_or(u64::MAX)
    }
}

impl Drop for GuardianRegistry {
    fn drop(&mut self) {
        let count = self
            .sessions
            .get_mut()
            .expect("guardian registry lock")
            .len();
        if let Some(readiness) = self
            .readiness
            .get_mut()
            .expect("guardian readiness lock")
            .as_ref()
        {
            for _ in 0..count {
                readiness.remove_durability_blocker();
            }
        }
    }
}

enum GuardianRegistration {
    Registered,
    AlreadyOwned,
    Rejected,
}

enum GuardianPayload {
    Plain(SecretString),
    Sealed(super::crypto::SealedSecret),
}

#[derive(Clone)]
pub(crate) struct AuthorizationMaterial {
    pub authorization_url: Url,
    pub state: SecretString,
    pub verifier: SecretString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoogleIdentity {
    pub subject: String,
    pub verified_email: Option<String>,
}

impl std::fmt::Debug for AuthorizationMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut endpoint = self.authorization_url.clone();
        endpoint.set_query(None);
        formatter
            .debug_struct("AuthorizationMaterial")
            .field("authorization_endpoint", &endpoint)
            .field("state", &"[REDACTED]")
            .field("verifier", &"[REDACTED]")
            .finish()
    }
}

#[async_trait]
pub(crate) trait GoogleOAuthTransport: Send + Sync {
    fn begin(&self, options: &AuthorizationOptions) -> Result<AuthorizationMaterial, GoogleError>;

    async fn exchange(
        &self,
        state: &SecretString,
        verifier: &SecretString,
        code: &SecretString,
    ) -> Result<OAuthTokenSet, GoogleError>;

    async fn refresh(&self, refresh_token: &SecretString) -> Result<OAuthTokenSet, GoogleError>;

    async fn identity(&self, access_token: &SecretString) -> Result<GoogleIdentity, GoogleError>;

    async fn revoke(&self, token: &SecretString) -> Result<(), GoogleError>;
}

#[derive(Clone)]
pub(crate) struct ProductionGoogleOAuthTransport {
    client: OAuthClient,
    http: reqwest::Client,
}

impl ProductionGoogleOAuthTransport {
    pub(crate) fn new(client: OAuthClient) -> Result<Self, GoogleError> {
        Ok(Self {
            client,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("DayWeave/0.1")
                .connect_timeout(StdDuration::from_secs(5))
                .timeout(StdDuration::from_secs(15))
                .build()
                .map_err(GoogleError::Transport)?,
        })
    }
}

#[async_trait]
impl GoogleOAuthTransport for ProductionGoogleOAuthTransport {
    fn begin(&self, options: &AuthorizationOptions) -> Result<AuthorizationMaterial, GoogleError> {
        let session = self.client.begin_authorization(options)?;
        Ok(AuthorizationMaterial {
            authorization_url: session.authorization_url.clone(),
            state: session.state().clone(),
            verifier: session.code_verifier().clone(),
        })
    }

    async fn exchange(
        &self,
        state: &SecretString,
        verifier: &SecretString,
        code: &SecretString,
    ) -> Result<OAuthTokenSet, GoogleError> {
        let session = AuthorizationSession::from_stored(
            Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
                .expect("constant Google authorization URL is valid"),
            state.clone(),
            verifier.clone(),
        );
        self.client
            .exchange_code(&session, state.expose_secret(), code)
            .await
    }

    async fn refresh(&self, refresh_token: &SecretString) -> Result<OAuthTokenSet, GoogleError> {
        self.client.refresh(refresh_token).await
    }

    async fn identity(&self, access_token: &SecretString) -> Result<GoogleIdentity, GoogleError> {
        let response = self
            .http
            .get("https://openidconnect.googleapis.com/v1/userinfo")
            .bearer_auth(access_token.expose_secret())
            .send()
            .await
            .map_err(GoogleError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    GoogleError::Unauthorized
                }
                reqwest::StatusCode::TOO_MANY_REQUESTS => GoogleError::RateLimited {
                    retry_after_seconds: None,
                },
                value if value.is_server_error() => GoogleError::Temporary {
                    status: value.as_u16(),
                },
                value => GoogleError::Api {
                    status: value.as_u16(),
                },
            });
        }
        let identity: UserInfoResponse = response.json().await.map_err(GoogleError::Transport)?;
        if identity.sub.is_empty()
            || identity.sub.len() > 500
            || identity.sub.chars().any(char::is_control)
        {
            return Err(GoogleError::InvalidOAuthRequest(
                "userinfo response omitted a valid subject",
            ));
        }
        let verified_email = identity.email.filter(|email| {
            identity.email_verified
                && !email.is_empty()
                && email.len() <= 200
                && !email.chars().any(char::is_control)
        });
        Ok(GoogleIdentity {
            subject: identity.sub,
            verified_email,
        })
    }

    async fn revoke(&self, token: &SecretString) -> Result<(), GoogleError> {
        self.client.revoke(token).await
    }
}

#[derive(Deserialize)]
struct UserInfoResponse {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct BeginAuthorization {
    pub owner_subject: String,
    pub services: BTreeSet<GoogleService>,
    pub force_consent: bool,
    pub login_hint: Option<String>,
    pub account_id: Option<Uuid>,
    pub connect_new: bool,
    pub make_default: bool,
}

#[derive(Clone)]
pub(crate) struct OAuthIdempotencyKey {
    pub key: String,
    pub fingerprint: [u8; 32],
}

impl std::fmt::Debug for OAuthIdempotencyKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthIdempotencyKey")
            .field("key", &"[REDACTED]")
            .field("fingerprint", &"[SHA-256]")
            .finish()
    }
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GoogleService {
    CalendarReadOnly,
    Calendar,
    TasksReadOnly,
    Tasks,
}

impl GoogleService {
    const fn scope(self) -> &'static str {
        match self {
            Self::CalendarReadOnly => GOOGLE_CALENDAR_READONLY_SCOPE,
            Self::Calendar => GOOGLE_CALENDAR_SCOPE,
            Self::TasksReadOnly => GOOGLE_TASKS_READONLY_SCOPE,
            Self::Tasks => GOOGLE_TASKS_SCOPE,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AuthorizationStarted {
    pub authorization_url: Url,
    pub expires_at: DateTime<Utc>,
    pub replayed: bool,
}

impl std::fmt::Debug for AuthorizationStarted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut endpoint = self.authorization_url.clone();
        endpoint.set_query(None);
        formatter
            .debug_struct("AuthorizationStarted")
            .field("authorization_endpoint", &endpoint)
            .field("expires_at", &self.expires_at)
            .field("replayed", &self.replayed)
            .finish()
    }
}

pub struct GoogleOAuthService {
    repository: Arc<dyn GoogleOAuthRepository>,
    transport: Arc<dyn GoogleOAuthTransport>,
    cipher: SecretCipher,
    scope: OAuthScope,
    clock: Arc<dyn Clock>,
    session_ttl: TimeDelta,
    guardians: Arc<GuardianRegistry>,
}

impl std::fmt::Debug for GoogleOAuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleOAuthService")
            .field("cipher", &self.cipher)
            .field("scope", &self.scope)
            .field("session_ttl", &self.session_ttl)
            .field("volatile_guardians", &self.guardians.count())
            .finish_non_exhaustive()
    }
}

impl GoogleOAuthService {
    #[must_use]
    /// # Panics
    ///
    /// Panics only if the already validated session TTL cannot fit in a
    /// `chrono::TimeDelta`.
    pub(crate) fn new(
        repository: Arc<dyn GoogleOAuthRepository>,
        transport: Arc<dyn GoogleOAuthTransport>,
        cipher: SecretCipher,
        scope: OAuthScope,
        clock: Arc<dyn Clock>,
        session_ttl: StdDuration,
    ) -> Self {
        Self {
            repository,
            transport,
            cipher,
            scope,
            clock,
            session_ttl: TimeDelta::from_std(session_ttl)
                .expect("configured OAuth session TTL fits chrono"),
            guardians: Arc::new(GuardianRegistry::default()),
        }
    }

    #[must_use]
    pub(crate) fn with_readiness(self, readiness: Readiness) -> Self {
        self.guardians.set_readiness(readiness);
        self
    }

    pub(crate) async fn recover_startup(&self) -> Result<(), GoogleOAuthServiceError> {
        let now = self.clock.now();
        self.repository
            .recover_startup(now, now - GUARDIAN_LEASE)
            .await?;
        if let Err(error) = self.reconcile_cleanup(None).await
            && !matches!(
                error,
                GoogleOAuthServiceError::Repository(
                    GoogleOAuthRepositoryError::RevocationInProgress
                )
            )
        {
            return Err(error);
        }
        if let Err(error) = self.repository.reconcile_staged().await
            && error != GoogleOAuthRepositoryError::RevocationInProgress
        {
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) fn spawn_recovery_worker(self: &Arc<Self>) {
        let service = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(STARTUP_RECOVERY_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let Some(service) = service.upgrade() else {
                    return;
                };
                let _ = service.recover_startup().await;
            }
        });
    }

    pub(crate) async fn acknowledge_operator_recovery(
        &self,
        project_grants_revoked: bool,
    ) -> Result<OperatorRecoveryResult, GoogleOAuthServiceError> {
        if !project_grants_revoked {
            return Err(GoogleOAuthServiceError::OperatorConfirmationRequired);
        }
        let now = self.clock.now();
        self.repository
            .recover_startup(now, now - GUARDIAN_LEASE)
            .await?;
        Ok(self.repository.acknowledge_operator_recovery(now).await?)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn begin(
        &self,
        input: BeginAuthorization,
        idempotency_key: OAuthIdempotencyKey,
    ) -> Result<AuthorizationStarted, GoogleOAuthServiceError> {
        if input.owner_subject.is_empty() || input.owner_subject.len() > 500 {
            return Err(GoogleOAuthServiceError::InvalidRequest);
        }
        if input.connect_new && input.account_id.is_some() {
            return Err(GoogleOAuthServiceError::InvalidRequest);
        }
        self.reconcile_cleanup(None).await?;
        self.repository.reconcile_staged().await?;
        let requested_services = if input.services.is_empty() {
            BTreeSet::from([
                GoogleService::CalendarReadOnly,
                GoogleService::TasksReadOnly,
            ])
        } else {
            input.services
        };
        let existing = if input.connect_new {
            None
        } else if let Some(account_id) = input.account_id {
            Some(
                self.repository
                    .account_by_id(account_id)
                    .await?
                    .ok_or(GoogleOAuthRepositoryError::AccountNotFound)?,
            )
        } else {
            let default = self.repository.account().await?;
            if default.is_none() && !self.repository.accounts().await?.is_empty() {
                return Err(GoogleOAuthRepositoryError::Internal.into());
            }
            default
        };
        // Opening the selected envelope is an authorization precondition. An
        // unavailable historical key must never be treated as permission to
        // overwrite the only revocable copy of that credential.
        let opened_existing = existing
            .as_ref()
            .map(|snapshot| self.open_credentials(snapshot))
            .transpose()?;
        let has_usable_existing_refresh = opened_existing.is_some()
            && existing
                .as_ref()
                .is_some_and(|snapshot| status_allows_refresh_reuse(snapshot.account.status));
        let mut scopes = existing.as_ref().map_or_else(BTreeSet::new, |snapshot| {
            snapshot.account.granted_scopes.clone()
        });
        scopes.extend(
            requested_services
                .iter()
                .copied()
                .map(GoogleService::scope)
                .map(str::to_owned),
        );
        if scopes.contains(GOOGLE_CALENDAR_SCOPE) {
            scopes.remove(GOOGLE_CALENDAR_READONLY_SCOPE);
        }
        if scopes.contains(GOOGLE_TASKS_SCOPE) {
            scopes.remove(GOOGLE_TASKS_READONLY_SCOPE);
        }
        scopes.insert(GOOGLE_OPENID_SCOPE.to_owned());
        scopes.insert(GOOGLE_EMAIL_SCOPE.to_owned());
        let force_consent = input.force_consent || !has_usable_existing_refresh;
        let material = self.transport.begin(&AuthorizationOptions {
            scopes: scopes.clone(),
            force_consent,
            login_hint: input.login_hint,
        })?;
        let now = self.clock.now();
        let expires_at = now + self.session_ttl;
        let session_id = Uuid::new_v4();
        let verifier = self.cipher.seal(
            material.verifier.expose_secret().as_bytes(),
            &oauth_session_aad(self.scope.workspace_id, self.scope.user_id, session_id),
        )?;
        let authorization_url = self.cipher.seal(
            material.authorization_url.as_str().as_bytes(),
            &oauth_authorization_url_aad(self.scope.workspace_id, self.scope.user_id, session_id),
        )?;
        let idempotency = Self::idempotency(
            "google.oauth.start",
            &idempotency_key,
            now,
            self.session_ttl,
        )?;
        let started = self
            .repository
            .create_session(
                super::domain::NewOAuthSession {
                    id: session_id,
                    owner_subject_hash: hash_secret(&input.owner_subject),
                    state_hash: hash_secret(material.state.expose_secret()),
                    encrypted_verifier: verifier,
                    encrypted_authorization_url: authorization_url,
                    requested_scopes: scopes,
                    expected_account_id: existing.as_ref().map(|snapshot| snapshot.account.id),
                    expected_account_revision: existing
                        .as_ref()
                        .map(|snapshot| snapshot.account.revision),
                    make_default: input.make_default,
                    created_at: now,
                    expires_at,
                },
                idempotency,
                now - EXCHANGE_LEASE,
            )
            .await?;
        let mut encoded_url = self.cipher.open(
            started.encrypted_authorization_url.key_version,
            &started.encrypted_authorization_url.ciphertext,
            &oauth_authorization_url_aad(self.scope.workspace_id, self.scope.user_id, started.id),
        )?;
        let parsed_url = std::str::from_utf8(&encoded_url)
            .ok()
            .and_then(|value| Url::parse(value).ok())
            .filter(|url| {
                url.scheme() == "https"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
            })
            .ok_or(GoogleOAuthServiceError::CredentialCorrupt);
        encoded_url.zeroize();
        Ok(AuthorizationStarted {
            authorization_url: parsed_url?,
            expires_at: started.expires_at,
            replayed: started.replayed,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn callback(
        &self,
        returned_state: &str,
        code: &str,
    ) -> Result<GoogleAccount, GoogleOAuthServiceError> {
        if !(20..=512).contains(&returned_state.len()) || code.is_empty() || code.len() > 4096 {
            return Err(GoogleOAuthServiceError::InvalidCallback);
        }
        let now = self.clock.now();
        let claim = self
            .repository
            .claim_callback(hash_secret(returned_state), now, now - EXCHANGE_LEASE)
            .await?;
        let claimed = match claim {
            CallbackClaim::Exchange(claimed) => claimed,
            CallbackClaim::Staged { session_id } => {
                return Ok(self
                    .repository
                    .complete_staged_authorization(session_id)
                    .await?);
            }
        };
        let aad = oauth_session_aad(self.scope.workspace_id, self.scope.user_id, claimed.id);
        let mut verifier_bytes = match self.cipher.open(
            claimed.encrypted_verifier.key_version,
            &claimed.encrypted_verifier.ciphertext,
            &aad,
        ) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.repository.fail_authorization(claimed.id, now).await;
                return Err(error.into());
            }
        };
        let verifier = if let Ok(value) = std::str::from_utf8(&verifier_bytes) {
            SecretString::from(value.to_owned())
        } else {
            erase(&mut verifier_bytes);
            let _ = self.repository.fail_authorization(claimed.id, now).await;
            return Err(GoogleOAuthServiceError::CredentialCorrupt);
        };
        erase(&mut verifier_bytes);
        let state = SecretString::from(returned_state.to_owned());
        let code = SecretString::from(code.to_owned());
        // Exercise both the active encryption key and the durable scoped
        // store before asking Google to mint a refresh credential. Failures
        // after exchange are still handled by the durable hold/guardian path.
        if let Err(error) = self.cipher.seal(
            b"google-oauth-cleanup-preflight",
            &oauth_cleanup_token_aad(self.scope.workspace_id, self.scope.user_id, claimed.id),
        ) {
            let _ = self.repository.fail_authorization(claimed.id, now).await;
            return Err(error.into());
        }
        if let Err(error) = self
            .repository
            .preflight_cleanup_storage(claimed.id, now)
            .await
        {
            let _ = self.repository.fail_authorization(claimed.id, now).await;
            return Err(error.into());
        }
        let tokens = match tokio::time::timeout(
            GOOGLE_EXCHANGE_TIMEOUT,
            self.transport.exchange(&state, &verifier, &code),
        )
        .await
        {
            Ok(Ok(tokens)) => tokens,
            Ok(Err(error)) => {
                if exchange_failure_is_definitive(&error) {
                    let _ = self.repository.fail_authorization(claimed.id, now).await;
                }
                return Err(GoogleOAuthServiceError::Google(error));
            }
            Err(_) => {
                // The provider may have committed the code exchange before the
                // timeout. Preserve the durable `exchanging` marker so startup
                // recovery cannot silently authorize another grant.
                return Err(GoogleOAuthServiceError::IntegrationTimeout);
            }
        };
        // A newly issued refresh token is made crash-safe before any identity
        // request, validation, or staging can fail. The repository removes
        // this held copy in the same transaction that installs the account.
        let has_new_refresh = tokens
            .refresh_token
            .as_ref()
            .is_some_and(|token| !token.expose_secret().is_empty());
        if let Some(refresh_token) = tokens
            .refresh_token
            .as_ref()
            .filter(|token| !token.expose_secret().is_empty())
        {
            self.hold_new_refresh_token(claimed.id, refresh_token, now)
                .await?;
        }
        let identity = match tokio::time::timeout(
            GOOGLE_IDENTITY_TIMEOUT,
            self.transport.identity(&tokens.access_token),
        )
        .await
        {
            Ok(Ok(identity)) => identity,
            Ok(Err(error)) => {
                self.abandon_and_reconcile(claimed.id, now).await;
                return Err(GoogleOAuthServiceError::Google(error));
            }
            Err(_) => {
                self.abandon_and_reconcile(claimed.id, now).await;
                return Err(GoogleOAuthServiceError::IntegrationTimeout);
            }
        };
        if has_new_refresh
            && valid_google_subject(&identity.subject)
            && let Err(error) = self
                .repository
                .identify_cleanup_token(claimed.id, &identity.subject, self.clock.now())
                .await
        {
            self.abandon_and_reconcile(claimed.id, now).await;
            return Err(error.into());
        }
        let completion = match self.completion_from_tokens(&claimed, tokens, identity, now) {
            Ok(completion) => completion,
            Err(error) => {
                self.abandon_and_reconcile(claimed.id, now).await;
                return Err(error);
            }
        };
        let stage_error = self.repository.stage_authorization(completion).await.err();
        self.resolve_and_complete(claimed.id, stage_error, now)
            .await
    }

    pub(crate) async fn callback_denied(
        &self,
        returned_state: &str,
    ) -> Result<(), GoogleOAuthServiceError> {
        if !(20..=512).contains(&returned_state.len()) {
            return Err(GoogleOAuthServiceError::InvalidCallback);
        }
        let now = self.clock.now();
        let claimed = self
            .repository
            .claim_callback(hash_secret(returned_state), now, now - EXCHANGE_LEASE)
            .await?;
        match claimed {
            CallbackClaim::Exchange(claimed) => {
                self.repository.fail_authorization(claimed.id, now).await?;
                Ok(())
            }
            CallbackClaim::Staged { .. } => Err(GoogleOAuthServiceError::InvalidCallback),
        }
    }

    pub(crate) async fn accounts_with_cleanup(
        &self,
    ) -> Result<(Vec<GoogleAccount>, GoogleOAuthCleanupStatus), GoogleOAuthServiceError> {
        self.reconcile_cleanup(None).await?;
        if let Err(error) = self.repository.reconcile_staged().await
            && error != GoogleOAuthRepositoryError::RevocationInProgress
        {
            return Err(error.into());
        }
        let accounts = self
            .repository
            .accounts()
            .await?
            .into_iter()
            .map(|snapshot| snapshot.account)
            .collect();
        let mut cleanup = self.repository.cleanup_status().await?;
        cleanup.volatile_guardians = self.guardians.count();
        cleanup.durability_degraded = cleanup.volatile_guardians > 0
            || cleanup.operator_recovery_required
            || cleanup.uncertain_authorizations > 0
            || cleanup.legacy_recovery_required > 0
            || cleanup.exhausted > 0;
        Ok((accounts, cleanup))
    }

    pub(crate) async fn access_token_for_sync(
        &self,
        account_id: Uuid,
    ) -> Result<SecretString, GoogleOAuthServiceError> {
        for _ in 0..2 {
            let snapshot = self
                .repository
                .account_by_id(account_id)
                .await?
                .ok_or(GoogleOAuthRepositoryError::AccountNotFound)?;
            if snapshot.account.status != GoogleAccountStatus::Active
                || !snapshot.account.sync_enabled
            {
                return Err(GoogleOAuthRepositoryError::AccountStateConflict.into());
            }
            let mut credentials = self.open_credentials(&snapshot)?;
            let now = self.clock.now();
            if credentials.access_expires_at > now + TimeDelta::seconds(90) {
                return Ok(credentials.access_token);
            }
            let tokens = tokio::time::timeout(
                GOOGLE_EXCHANGE_TIMEOUT,
                self.transport.refresh(&credentials.refresh_token),
            )
            .await
            .map_err(|_| GoogleOAuthServiceError::IntegrationTimeout)??;
            if tokens.refresh_token.is_some()
                || tokens.access_token.expose_secret().is_empty()
                || tokens.access_token.expose_secret().len() > 16_384
                || !tokens.token_type.eq_ignore_ascii_case("bearer")
                || tokens.expires_in_seconds == 0
                || tokens.expires_in_seconds > 86_400
            {
                return Err(GoogleOAuthServiceError::InvalidTokenResponse);
            }
            let expires_at = now
                + TimeDelta::try_seconds(
                    i64::try_from(tokens.expires_in_seconds)
                        .map_err(|_| GoogleOAuthServiceError::InvalidTokenResponse)?,
                )
                .ok_or(GoogleOAuthServiceError::InvalidTokenResponse)?;
            let access_token = tokens.access_token.clone();
            credentials.access_token = tokens.access_token;
            credentials.token_type = tokens.token_type;
            credentials.access_expires_at = expires_at;
            let encrypted = super::domain::EncryptedCredentials {
                sealed: self.seal_credentials(account_id, &credentials)?,
            };
            let granted_scopes = if tokens.granted_scopes.is_empty() {
                snapshot.account.granted_scopes.clone()
            } else {
                tokens.granted_scopes
            };
            match self
                .repository
                .update_access_credentials(
                    account_id,
                    snapshot.account.revision,
                    encrypted,
                    granted_scopes,
                    expires_at,
                    now,
                )
                .await
            {
                Ok(_) => return Ok(access_token),
                Err(GoogleOAuthRepositoryError::RevisionConflict { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(GoogleOAuthRepositoryError::AuthorizationConflict.into())
    }

    pub(crate) async fn account_for_sync(
        &self,
        account_id: Uuid,
    ) -> Result<GoogleAccount, GoogleOAuthServiceError> {
        let snapshot = self
            .repository
            .account_by_id(account_id)
            .await?
            .ok_or(GoogleOAuthRepositoryError::AccountNotFound)?;
        if snapshot.account.status != GoogleAccountStatus::Active || !snapshot.account.sync_enabled
        {
            return Err(GoogleOAuthRepositoryError::AccountStateConflict.into());
        }
        Ok(snapshot.account)
    }

    pub(crate) async fn set_paused(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        paused: bool,
        idempotency_key: OAuthIdempotencyKey,
    ) -> Result<GoogleAccountMutation, GoogleOAuthServiceError> {
        self.reconcile_cleanup(None).await?;
        self.repository.reconcile_staged().await?;
        let now = self.clock.now();
        let namespace = if paused {
            "google.account.pause"
        } else {
            "google.account.resume"
        };
        let idempotency =
            Self::idempotency(namespace, &idempotency_key, now, MUTATION_IDEMPOTENCY_TTL)?;
        Ok(self
            .repository
            .set_paused(
                account_id,
                expected_revision,
                paused,
                now,
                now - EXCHANGE_LEASE,
                idempotency,
            )
            .await?)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn disconnect(
        &self,
        account_id: Uuid,
        expected_revision: u64,
        idempotency_key: OAuthIdempotencyKey,
    ) -> Result<GoogleAccountMutation, GoogleOAuthServiceError> {
        self.reconcile_cleanup(None).await?;
        if let Err(error) = self.repository.reconcile_staged().await
            && error != GoogleOAuthRepositoryError::RevocationInProgress
        {
            return Err(error.into());
        }
        let now = self.clock.now();
        let idempotency = Self::idempotency(
            "google.account.disconnect",
            &idempotency_key,
            now,
            MUTATION_IDEMPOTENCY_TTL,
        )?;
        let claim_id = Uuid::new_v4();
        let claim = match self
            .repository
            .claim_disconnect(
                account_id,
                expected_revision,
                claim_id,
                now,
                now - DISCONNECT_LEASE,
                now - EXCHANGE_LEASE,
                idempotency.clone(),
            )
            .await?
        {
            DisconnectMutation::Replay(account) => {
                return Ok(GoogleAccountMutation {
                    account,
                    replayed: true,
                });
            }
            DisconnectMutation::Execute(claim) => claim,
        };
        let credentials = match self.open_claim_credentials(&claim) {
            Ok(credentials) => credentials,
            Err(error) => {
                self.repository
                    .fail_disconnect(account_id, claim_id, claim.credential_generation, now)
                    .await?;
                return Err(error);
            }
        };
        let retained_same_grant = claim.protected_accounts.iter().any(|snapshot| {
            snapshot.account.id != account_id
                && snapshot.account.external_account_id == claim.account.external_account_id
        });
        let mut tokens = Vec::with_capacity(1 + credentials.retired_refresh_tokens.len());
        if !retained_same_grant {
            for token in
                std::iter::once(credentials.refresh_token).chain(credentials.retired_refresh_tokens)
            {
                if tokens.iter().any(|candidate: &SecretString| {
                    candidate.expose_secret() == token.expose_secret()
                }) {
                    continue;
                }
                tokens.push(token);
            }
        }
        for token in tokens {
            match tokio::time::timeout(GOOGLE_REVOCATION_TIMEOUT, self.transport.revoke(&token))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(GoogleError::OAuthRejected { code })) if code == "invalid_token" => {}
                Ok(Err(error)) => {
                    self.repository
                        .fail_disconnect(
                            account_id,
                            claim_id,
                            claim.credential_generation,
                            self.clock.now(),
                        )
                        .await?;
                    return Err(GoogleOAuthServiceError::Google(error));
                }
                Err(_) => {
                    self.repository
                        .fail_disconnect(
                            account_id,
                            claim_id,
                            claim.credential_generation,
                            self.clock.now(),
                        )
                        .await?;
                    return Err(GoogleOAuthServiceError::IntegrationTimeout);
                }
            }
        }
        Ok(self
            .repository
            .complete_disconnect(
                account_id,
                claim_id,
                claim.credential_generation,
                self.clock.now(),
                idempotency,
            )
            .await?)
    }

    async fn hold_new_refresh_token(
        &self,
        session_id: Uuid,
        refresh_token: &SecretString,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthServiceError> {
        let token_hash = hash_secret(refresh_token.expose_secret());
        let owns_registration = match self.guardians.register(session_id, token_hash) {
            GuardianRegistration::Registered => true,
            GuardianRegistration::AlreadyOwned => false,
            GuardianRegistration::Rejected => {
                // Scoped repositories allow only one exchange, so capacity
                // rejection is defensive. Keep ownership in this request and
                // do not return while the sole credential is volatile.
                let payload = self.guardian_payload(session_id, refresh_token);
                guardian_loop(
                    Arc::downgrade(&self.guardians),
                    GuardianContext {
                        repository: self.repository.clone(),
                        transport: self.transport.clone(),
                        cipher: self.cipher.clone(),
                        scope: self.scope,
                        clock: self.clock.clone(),
                    },
                    session_id,
                    payload,
                    false,
                )
                .await;
                return Err(GoogleOAuthServiceError::CredentialDurabilityPending);
            }
        };
        let payload = self.guardian_payload(session_id, refresh_token);
        if let GuardianPayload::Sealed(sealed) = &payload {
            for delay in HOLD_RETRY_DELAYS {
                match self
                    .repository
                    .hold_cleanup_token(session_id, sealed.clone(), now)
                    .await
                {
                    Ok(()) => {
                        if owns_registration {
                            self.guardians.finish(session_id);
                        }
                        return Ok(());
                    }
                    Err(_) => tokio::time::sleep(delay).await,
                }
            }
        }
        if owns_registration {
            let context = GuardianContext {
                repository: self.repository.clone(),
                transport: self.transport.clone(),
                cipher: self.cipher.clone(),
                scope: self.scope,
                clock: self.clock.clone(),
            };
            let registry = Arc::downgrade(&self.guardians);
            tokio::spawn(async move {
                guardian_loop(registry, context, session_id, payload, true).await;
            });
        }
        Err(GoogleOAuthServiceError::CredentialDurabilityPending)
    }

    fn guardian_payload(&self, session_id: Uuid, refresh_token: &SecretString) -> GuardianPayload {
        match self.cipher.seal(
            refresh_token.expose_secret().as_bytes(),
            &oauth_cleanup_token_aad(self.scope.workspace_id, self.scope.user_id, session_id),
        ) {
            Ok(sealed) => GuardianPayload::Sealed(sealed),
            Err(_) => {
                GuardianPayload::Plain(SecretString::from(refresh_token.expose_secret().to_owned()))
            }
        }
    }

    async fn abandon_and_reconcile(&self, session_id: Uuid, now: DateTime<Utc>) {
        let _ = self.repository.abandon_authorization(session_id, now).await;
        let _ = self.reconcile_cleanup(Some(session_id)).await;
    }

    async fn resolve_and_complete(
        &self,
        session_id: Uuid,
        original_error: Option<GoogleOAuthRepositoryError>,
        now: DateTime<Utc>,
    ) -> Result<GoogleAccount, GoogleOAuthServiceError> {
        let resolution = match self.repository.resolve_authorization(session_id).await {
            Ok(resolution) => resolution,
            Err(error) => return Err(original_error.unwrap_or(error).into()),
        };
        match resolution {
            AuthorizationResolution::Consumed(account) => Ok(account),
            AuthorizationResolution::NeverStaged => {
                self.abandon_and_reconcile(session_id, now).await;
                Err(original_error
                    .unwrap_or(GoogleOAuthRepositoryError::InvalidCallbackState)
                    .into())
            }
            AuthorizationResolution::Staged => {
                match self
                    .repository
                    .complete_staged_authorization(session_id)
                    .await
                {
                    Ok(account) => Ok(account),
                    Err(completion_error) => {
                        match self.repository.resolve_authorization(session_id).await {
                            Ok(AuthorizationResolution::Consumed(account)) => Ok(account),
                            Ok(AuthorizationResolution::NeverStaged) => {
                                self.abandon_and_reconcile(session_id, now).await;
                                Err(original_error.unwrap_or(completion_error).into())
                            }
                            Ok(AuthorizationResolution::Staged) | Err(_) => {
                                // Still staged is durable and reconcile_staged
                                // can safely finish it on the next entry point.
                                Err(original_error.unwrap_or(completion_error).into())
                            }
                        }
                    }
                }
            }
        }
    }

    async fn reconcile_cleanup(
        &self,
        only_session_id: Option<Uuid>,
    ) -> Result<(), GoogleOAuthServiceError> {
        let now = self.clock.now();
        let Some(claim) = self
            .repository
            .claim_cleanup(
                Uuid::new_v4(),
                now,
                now - DISCONNECT_LEASE,
                now - EXCHANGE_LEASE,
                only_session_id,
            )
            .await?
        else {
            return Ok(());
        };
        let Ok(mut plaintext) = self.cipher.open(
            claim.encrypted_refresh_token.key_version,
            &claim.encrypted_refresh_token.ciphertext,
            &oauth_cleanup_token_aad(
                self.scope.workspace_id,
                self.scope.user_id,
                claim.session_id,
            ),
        ) else {
            self.fail_cleanup_claim(&claim, now).await?;
            return Ok(());
        };
        let token = std::str::from_utf8(&plaintext)
            .ok()
            .filter(|value| !value.is_empty() && value.len() <= 16_384)
            .map(|value| SecretString::from(value.to_owned()));
        plaintext.zeroize();
        let Some(token) = token else {
            self.fail_cleanup_claim(&claim, now).await?;
            return Ok(());
        };
        let retained_same_grant = claim.external_account_id.as_ref().is_some_and(|subject| {
            claim
                .protected_accounts
                .iter()
                .any(|snapshot| snapshot.account.external_account_id == *subject)
        });
        if retained_same_grant {
            // Google revocation is project/user-grant wide. The newly issued
            // token is merely another credential for a retained grant, so
            // discard our encrypted copy without contacting Google.
            self.repository
                .complete_cleanup(
                    claim.session_id,
                    claim.claim_id,
                    claim.credential_generation,
                    self.clock.now(),
                )
                .await?;
            return Ok(());
        }
        if claim.external_account_id.is_none() && !claim.protected_accounts.is_empty() {
            // Identity is unknowable and revoking could invalidate any retained
            // account. Preserve the token and fence until an operator confirms
            // project-wide revocation outside DayWeave.
            self.repository
                .defer_cleanup_for_operator(
                    claim.session_id,
                    claim.claim_id,
                    claim.credential_generation,
                    self.clock.now(),
                )
                .await?;
            return Ok(());
        }
        let revoked =
            match tokio::time::timeout(GOOGLE_REVOCATION_TIMEOUT, self.transport.revoke(&token))
                .await
            {
                Ok(Ok(())) => true,
                Ok(Err(GoogleError::OAuthRejected { code })) if code == "invalid_token" => true,
                Ok(Err(_)) | Err(_) => false,
            };
        if revoked {
            self.repository
                .complete_cleanup(
                    claim.session_id,
                    claim.claim_id,
                    claim.credential_generation,
                    self.clock.now(),
                )
                .await?;
        } else {
            self.fail_cleanup_claim(&claim, self.clock.now()).await?;
        }
        Ok(())
    }

    async fn fail_cleanup_claim(
        &self,
        claim: &super::domain::CleanupClaim,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleOAuthServiceError> {
        let exponent = claim.attempt.saturating_sub(1).min(20);
        let seconds = 1_i64
            .checked_shl(exponent)
            .unwrap_or(CLEANUP_BACKOFF_CAP_SECONDS)
            .min(CLEANUP_BACKOFF_CAP_SECONDS);
        self.repository
            .fail_cleanup(
                claim.session_id,
                claim.claim_id,
                claim.credential_generation,
                now,
                now + TimeDelta::seconds(seconds),
            )
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn completion_from_tokens(
        &self,
        claimed: &super::domain::ClaimedOAuthSession,
        tokens: OAuthTokenSet,
        identity: GoogleIdentity,
        now: DateTime<Utc>,
    ) -> Result<AuthorizationCompletion, GoogleOAuthServiceError> {
        if tokens.access_token.expose_secret().is_empty()
            || tokens.access_token.expose_secret().len() > 16_384
            || !tokens.token_type.eq_ignore_ascii_case("bearer")
            || tokens.expires_in_seconds == 0
            || tokens.expires_in_seconds > 86_400
            || tokens.granted_scopes.len() > 64
            || tokens.granted_scopes.iter().any(|scope| {
                scope.is_empty() || scope.len() > 500 || scope.chars().any(char::is_control)
            })
        {
            return Err(GoogleOAuthServiceError::InvalidTokenResponse);
        }
        if !valid_google_subject(&identity.subject) {
            return Err(GoogleOAuthServiceError::InvalidTokenResponse);
        }
        let granted_scopes = if tokens.granted_scopes.is_empty() {
            claimed.requested_scopes.clone()
        } else {
            tokens.granted_scopes
        };
        if !claimed.requested_scopes.is_subset(&granted_scopes) {
            return Err(GoogleOAuthServiceError::MissingRequestedScopes);
        }
        let same_identity = claimed
            .existing_account
            .as_ref()
            .is_some_and(|snapshot| snapshot.account.external_account_id == identity.subject);
        if claimed.existing_account.is_some() && !same_identity {
            return Err(GoogleOAuthServiceError::IdentityMismatch);
        }
        let existing_credentials = claimed
            .existing_account
            .as_ref()
            .map(|snapshot| self.open_credentials(snapshot))
            .transpose()?;
        let may_reuse_existing = same_identity
            && claimed
                .existing_account
                .as_ref()
                .is_some_and(|snapshot| status_allows_refresh_reuse(snapshot.account.status));
        let (refresh_token, retired_refresh_tokens) = match tokens.refresh_token {
            Some(refresh_token) => {
                let retired = existing_credentials
                    .map(|existing| retire_credentials(&refresh_token, existing))
                    .transpose()?
                    .unwrap_or_default();
                (refresh_token, retired)
            }
            None if may_reuse_existing => {
                let existing =
                    existing_credentials.ok_or(GoogleOAuthServiceError::MissingRefreshToken)?;
                (existing.refresh_token, existing.retired_refresh_tokens)
            }
            None => return Err(GoogleOAuthServiceError::MissingRefreshToken),
        };
        if refresh_token.expose_secret().is_empty() || refresh_token.expose_secret().len() > 16_384
        {
            return Err(GoogleOAuthServiceError::InvalidTokenResponse);
        }
        let expires_delta = TimeDelta::try_seconds(
            i64::try_from(tokens.expires_in_seconds)
                .map_err(|_| GoogleOAuthServiceError::InvalidTokenResponse)?,
        )
        .ok_or(GoogleOAuthServiceError::InvalidTokenResponse)?;
        let expires_at = now + expires_delta;
        let account_id = claimed
            .existing_account
            .as_ref()
            .map_or_else(Uuid::new_v4, |snapshot| snapshot.account.id);
        let credentials = StoredCredentials {
            access_token: tokens.access_token,
            refresh_token,
            retired_refresh_tokens,
            token_type: tokens.token_type,
            access_expires_at: expires_at,
        };
        let encrypted = self.seal_credentials(account_id, &credentials)?;
        Ok(AuthorizationCompletion {
            session_id: claimed.id,
            owner_subject_hash: claimed.owner_subject_hash,
            expected_account_revision: claimed
                .existing_account
                .as_ref()
                .map(|snapshot| snapshot.account.revision),
            account_id,
            make_default: claimed.make_default,
            external_account_id: identity.subject,
            display_label: identity
                .verified_email
                .filter(|email| {
                    !email.is_empty() && email.len() <= 200 && !email.chars().any(char::is_control)
                })
                .unwrap_or_else(|| "Google Calendar and Tasks".to_owned()),
            credentials: super::domain::EncryptedCredentials { sealed: encrypted },
            granted_scopes,
            token_expires_at: expires_at,
            now,
        })
    }

    fn seal_credentials(
        &self,
        account_id: Uuid,
        credentials: &StoredCredentials,
    ) -> Result<super::crypto::SealedSecret, GoogleOAuthServiceError> {
        let wire = CredentialWireRef {
            access_token: credentials.access_token.expose_secret(),
            refresh_token: credentials.refresh_token.expose_secret(),
            retired_refresh_tokens: credentials
                .retired_refresh_tokens
                .iter()
                .map(ExposeSecret::expose_secret)
                .collect(),
            token_type: &credentials.token_type,
            access_expires_at: credentials.access_expires_at,
        };
        let mut plaintext =
            serde_json::to_vec(&wire).map_err(|_| GoogleOAuthServiceError::CredentialCorrupt)?;
        let result = self.cipher.seal(
            &plaintext,
            &account_credentials_aad(self.scope.workspace_id, self.scope.user_id, account_id),
        );
        plaintext.zeroize();
        Ok(result?)
    }

    fn open_credentials(
        &self,
        snapshot: &super::domain::AccountSecretSnapshot,
    ) -> Result<StoredCredentials, GoogleOAuthServiceError> {
        self.open_credentials_for(snapshot.account.id, &snapshot.credentials)
    }

    fn open_claim_credentials(
        &self,
        claim: &super::domain::DisconnectClaim,
    ) -> Result<StoredCredentials, GoogleOAuthServiceError> {
        self.open_credentials_for(claim.account.id, &claim.credentials)
    }

    fn open_credentials_for(
        &self,
        account_id: Uuid,
        credentials: &super::domain::EncryptedCredentials,
    ) -> Result<StoredCredentials, GoogleOAuthServiceError> {
        open_credentials_with(&self.cipher, self.scope, account_id, credentials)
    }

    fn idempotency(
        namespace: &'static str,
        key: &OAuthIdempotencyKey,
        now: DateTime<Utc>,
        ttl: TimeDelta,
    ) -> Result<OAuthIdempotency, GoogleOAuthServiceError> {
        if key.key.len() < 8
            || key.key.len() > 128
            || !key
                .key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(GoogleOAuthServiceError::InvalidIdempotencyKey);
        }
        let expires_at = now
            .checked_add_signed(ttl)
            .ok_or(GoogleOAuthServiceError::InvalidRequest)?;
        Ok(OAuthIdempotency {
            namespace,
            key_hash: Sha256::digest(key.key.as_bytes()).into(),
            request_fingerprint: key.fingerprint,
            expires_at,
        })
    }
}

#[derive(Clone)]
struct GuardianContext {
    repository: Arc<dyn GoogleOAuthRepository>,
    transport: Arc<dyn GoogleOAuthTransport>,
    cipher: SecretCipher,
    scope: OAuthScope,
    clock: Arc<dyn Clock>,
}

#[allow(clippy::too_many_lines)]
async fn guardian_loop(
    registry: Weak<GuardianRegistry>,
    context: GuardianContext,
    session_id: Uuid,
    mut payload: GuardianPayload,
    owns_registration: bool,
) {
    let mut delay = StdDuration::from_secs(1);
    let mut active_claim: Option<RevocationFenceClaim> = None;
    let mut provider_revocation_definitive = false;
    loop {
        let Some(supervisor) = registry.upgrade() else {
            // Service shutdown cancels the guardian. If the process is lost
            // while storage, its keystore, and Google are all unavailable,
            // no in-process design can preserve the sole plaintext token.
            // Readiness/status expose this window while the service is alive.
            return;
        };
        if let GuardianPayload::Plain(token) = &payload
            && let Ok(sealed) = context.cipher.seal(
                token.expose_secret().as_bytes(),
                &oauth_cleanup_token_aad(
                    context.scope.workspace_id,
                    context.scope.user_id,
                    session_id,
                ),
            )
        {
            payload = GuardianPayload::Sealed(sealed);
        }

        if provider_revocation_definitive {
            let Some(claim) = active_claim.as_ref() else {
                // The flag and claim are changed together below. Retaining the
                // guardian is safer than discarding the only live credential
                // if that invariant is ever violated.
                drop(supervisor);
                tokio::time::sleep(delay).await;
                continue;
            };
            if context
                .repository
                .complete_volatile_revocation(
                    session_id,
                    claim.claim_id,
                    claim.credential_generation,
                )
                .await
                .is_ok()
            {
                let _ = context
                    .repository
                    .abandon_authorization(session_id, context.clock.now())
                    .await;
                if owns_registration {
                    supervisor.finish(session_id);
                }
                return;
            }
            drop(supervisor);
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(StdDuration::from_mins(1));
            continue;
        }

        if let GuardianPayload::Sealed(sealed) = &payload
            && context
                .repository
                .hold_cleanup_token(session_id, sealed.clone(), context.clock.now())
                .await
                .is_ok()
        {
            if let Some(claim) = active_claim.as_ref()
                && context
                    .repository
                    .release_volatile_revocation(
                        session_id,
                        claim.claim_id,
                        claim.credential_generation,
                    )
                    .await
                    .is_err()
            {
                drop(supervisor);
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(StdDuration::from_mins(1));
                continue;
            }
            let _ = context
                .repository
                .abandon_authorization(session_id, context.clock.now())
                .await;
            if owns_registration {
                supervisor.finish(session_id);
            }
            return;
        }

        let claim_id = active_claim
            .as_ref()
            .map_or_else(Uuid::new_v4, |claim| claim.claim_id);
        let Ok(claim) = context
            .repository
            .claim_volatile_revocation(
                session_id,
                claim_id,
                context.clock.now(),
                context.clock.now() - GUARDIAN_LEASE,
            )
            .await
        else {
            // Storage or another scoped revocation owns the fence. Never
            // contact Google until a durable protected-set snapshot and
            // exact generation have been acquired.
            drop(supervisor);
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(StdDuration::from_mins(1));
            continue;
        };
        active_claim = Some(claim.clone());
        let Some(token) = guardian_token(&context, session_id, &payload) else {
            drop(supervisor);
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(StdDuration::from_mins(1));
            continue;
        };
        if !claim.protected_accounts.is_empty() {
            // The guardian has no verified subject. Any retained account could
            // share this project/user grant, so provider revocation is unsafe.
            // Keep retrying durable storage under the fence; after a crash the
            // startup worker converts the stale fence to operator recovery.
            drop(supervisor);
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(StdDuration::from_mins(1));
            continue;
        }

        let definitive =
            match tokio::time::timeout(GOOGLE_REVOCATION_TIMEOUT, context.transport.revoke(&token))
                .await
            {
                Ok(Ok(())) => true,
                Ok(Err(GoogleError::OAuthRejected { code })) if code == "invalid_token" => true,
                Ok(Err(_)) | Err(_) => false,
            };
        if definitive {
            provider_revocation_definitive = true;
            if context
                .repository
                .complete_volatile_revocation(
                    session_id,
                    claim.claim_id,
                    claim.credential_generation,
                )
                .await
                .is_ok()
            {
                let _ = context
                    .repository
                    .abandon_authorization(session_id, context.clock.now())
                    .await;
                if owns_registration {
                    supervisor.finish(session_id);
                }
                return;
            }
        }
        drop(supervisor);
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(StdDuration::from_mins(1));
    }
}

fn guardian_token(
    context: &GuardianContext,
    session_id: Uuid,
    payload: &GuardianPayload,
) -> Option<SecretString> {
    match payload {
        GuardianPayload::Plain(token) => Some(SecretString::from(token.expose_secret().to_owned())),
        GuardianPayload::Sealed(sealed) => {
            let mut plaintext = context
                .cipher
                .open(
                    sealed.key_version,
                    &sealed.ciphertext,
                    &oauth_cleanup_token_aad(
                        context.scope.workspace_id,
                        context.scope.user_id,
                        session_id,
                    ),
                )
                .ok()?;
            let token = std::str::from_utf8(&plaintext)
                .ok()
                .filter(|value| !value.is_empty() && value.len() <= 16_384)
                .map(|value| SecretString::from(value.to_owned()));
            plaintext.zeroize();
            token
        }
    }
}

fn open_credentials_with(
    cipher: &SecretCipher,
    scope: OAuthScope,
    account_id: Uuid,
    credentials: &super::domain::EncryptedCredentials,
) -> Result<StoredCredentials, GoogleOAuthServiceError> {
    let mut plaintext = cipher.open(
        credentials.sealed.key_version,
        &credentials.sealed.ciphertext,
        &account_credentials_aad(scope.workspace_id, scope.user_id, account_id),
    )?;
    let parsed = serde_json::from_slice::<CredentialWire>(&plaintext)
        .map_err(|_| GoogleOAuthServiceError::CredentialCorrupt);
    plaintext.zeroize();
    let mut wire = parsed?;
    if wire.access_token.is_empty()
        || wire.access_token.len() > 16_384
        || wire.refresh_token.is_empty()
        || wire.refresh_token.len() > 16_384
        || !wire.token_type.eq_ignore_ascii_case("bearer")
        || wire.retired_refresh_tokens.len() > MAX_RETIRED_REFRESH_TOKENS
        || wire
            .retired_refresh_tokens
            .iter()
            .any(|token| token.is_empty() || token.len() > 16_384)
    {
        return Err(GoogleOAuthServiceError::CredentialCorrupt);
    }
    Ok(StoredCredentials {
        access_token: SecretString::from(std::mem::take(&mut wire.access_token)),
        refresh_token: SecretString::from(std::mem::take(&mut wire.refresh_token)),
        retired_refresh_tokens: std::mem::take(&mut wire.retired_refresh_tokens)
            .into_iter()
            .map(SecretString::from)
            .collect(),
        token_type: std::mem::take(&mut wire.token_type),
        access_expires_at: wire.access_expires_at,
    })
}

fn status_allows_refresh_reuse(status: GoogleAccountStatus) -> bool {
    matches!(
        status,
        GoogleAccountStatus::Active | GoogleAccountStatus::Paused
    )
}

fn retire_credentials(
    current: &SecretString,
    existing: StoredCredentials,
) -> Result<Vec<SecretString>, GoogleOAuthServiceError> {
    let mut retired = existing.retired_refresh_tokens;
    if existing.refresh_token.expose_secret() != current.expose_secret() {
        retired.push(existing.refresh_token);
    }
    let mut unique = Vec::with_capacity(retired.len().min(MAX_RETIRED_REFRESH_TOKENS));
    for token in retired {
        if token.expose_secret() == current.expose_secret() {
            continue;
        }
        if unique
            .iter()
            .any(|saved: &SecretString| saved.expose_secret() == token.expose_secret())
        {
            continue;
        }
        if unique.len() == MAX_RETIRED_REFRESH_TOKENS {
            return Err(GoogleOAuthServiceError::InvalidTokenResponse);
        }
        unique.push(token);
    }
    Ok(unique)
}

fn valid_google_subject(subject: &str) -> bool {
    !subject.is_empty() && subject.len() <= 500 && !subject.chars().any(char::is_control)
}

fn exchange_failure_is_definitive(error: &GoogleError) -> bool {
    matches!(
        error,
        GoogleError::OAuthStateMismatch
            | GoogleError::OAuthRejected { .. }
            | GoogleError::Unauthorized
            | GoogleError::RateLimited { .. }
            | GoogleError::Api { .. }
    )
}

#[derive(Serialize)]
struct CredentialWireRef<'a> {
    access_token: &'a str,
    refresh_token: &'a str,
    retired_refresh_tokens: Vec<&'a str>,
    token_type: &'a str,
    access_expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct CredentialWire {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    retired_refresh_tokens: Vec<String>,
    token_type: String,
    access_expires_at: DateTime<Utc>,
}

impl Drop for CredentialWire {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        self.retired_refresh_tokens.zeroize();
        self.token_type.zeroize();
    }
}

#[derive(Debug, Error)]
pub(crate) enum GoogleOAuthServiceError {
    #[error("Google OAuth request is invalid")]
    InvalidRequest,
    #[error("Idempotency-Key must be 8-128 URL-safe ASCII characters")]
    InvalidIdempotencyKey,
    #[error("Google OAuth callback is invalid, expired, or already used")]
    InvalidCallback,
    #[error("Google did not return all requested scopes")]
    MissingRequestedScopes,
    #[error("Google did not return a refresh token; authorization must be restarted with consent")]
    MissingRefreshToken,
    #[error("the verified Google identity does not match the selected account")]
    IdentityMismatch,
    #[error("Google returned an invalid token response")]
    InvalidTokenResponse,
    #[error("stored Google credentials are corrupt")]
    CredentialCorrupt,
    #[error("Google integration request timed out")]
    IntegrationTimeout,
    #[error("Google refresh credential durability is still being reconciled")]
    CredentialDurabilityPending,
    #[error("operator must confirm external revocation of all affected Google project grants")]
    OperatorConfirmationRequired,
    #[error(transparent)]
    Repository(#[from] GoogleOAuthRepositoryError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Google(#[from] GoogleError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Mutex,
    };
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        config::CredentialKey,
        google_oauth::{GoogleAccountStatus, InMemoryGoogleOAuthRepository},
    };

    struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        fn new(now: DateTime<Utc>) -> Self {
            Self(Mutex::new(now))
        }

        fn advance(&self, delta: TimeDelta) {
            let mut now = self.0.lock().expect("clock lock");
            *now += delta;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().expect("clock lock")
        }
    }

    #[derive(Default)]
    #[allow(clippy::struct_excessive_bools)]
    struct FakeTransportState {
        begins: Vec<AuthorizationOptions>,
        states: Vec<String>,
        exchanges: usize,
        refreshes: usize,
        rotate_refresh_next: bool,
        refresh_tokens: VecDeque<Option<String>>,
        fail_next_revoke: bool,
        revoke_failures_remaining: usize,
        already_revoked_next: bool,
        revoked_tokens: Vec<String>,
        identity_subject: String,
        fail_next_identity: bool,
        omit_tasks_scope_next: bool,
        temporary_exchange_next: bool,
    }

    #[derive(Default)]
    struct FakeTransport(
        Mutex<FakeTransportState>,
        Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    );

    impl FakeTransport {
        fn with_refreshes(refreshes: impl IntoIterator<Item = Option<&'static str>>) -> Self {
            Self(
                Mutex::new(FakeTransportState {
                    refresh_tokens: refreshes
                        .into_iter()
                        .map(|value| value.map(str::to_owned))
                        .collect(),
                    identity_subject: "google-user-one".to_owned(),
                    ..FakeTransportState::default()
                }),
                Mutex::default(),
            )
        }

        fn pause_next_revoke(&self) -> (Arc<Notify>, Arc<Notify>) {
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            *self.1.lock().expect("revoke barrier lock") = Some((entered.clone(), release.clone()));
            (entered, release)
        }

        fn state_from_url(url: &Url) -> String {
            url.query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned())
                .expect("state query")
        }
    }

    #[async_trait]
    impl GoogleOAuthTransport for FakeTransport {
        fn begin(
            &self,
            options: &AuthorizationOptions,
        ) -> Result<AuthorizationMaterial, GoogleError> {
            let mut state = self.0.lock().expect("transport lock");
            let opaque_state = format!("state-{:0>40}", state.begins.len() + 1);
            let authorization_url = Url::parse_with_params(
                "https://accounts.google.test/authorize",
                [("state", opaque_state.as_str())],
            )
            .expect("test URL");
            state.begins.push(options.clone());
            state.states.push(opaque_state.clone());
            Ok(AuthorizationMaterial {
                authorization_url,
                state: SecretString::from(opaque_state),
                verifier: SecretString::from(format!("verifier-{:0>64}", state.begins.len())),
            })
        }

        async fn exchange(
            &self,
            _state: &SecretString,
            _verifier: &SecretString,
            _code: &SecretString,
        ) -> Result<OAuthTokenSet, GoogleError> {
            let mut state = self.0.lock().expect("transport lock");
            state.exchanges += 1;
            if state.temporary_exchange_next {
                state.temporary_exchange_next = false;
                return Err(GoogleError::Temporary { status: 503 });
            }
            let exchange = state.exchanges;
            let refresh_token = state.refresh_tokens.pop_front().flatten();
            let mut granted_scopes = state
                .begins
                .last()
                .map(|begin| begin.scopes.clone())
                .unwrap_or_default();
            if state.omit_tasks_scope_next {
                state.omit_tasks_scope_next = false;
                granted_scopes.remove(GOOGLE_TASKS_SCOPE);
                granted_scopes.remove(GOOGLE_TASKS_READONLY_SCOPE);
            }
            Ok(OAuthTokenSet {
                access_token: SecretString::from(format!("access-{exchange}")),
                refresh_token: refresh_token.map(SecretString::from),
                expires_in_seconds: 3_600,
                token_type: "Bearer".to_owned(),
                granted_scopes,
                id_token: None,
            })
        }

        async fn refresh(
            &self,
            _refresh_token: &SecretString,
        ) -> Result<OAuthTokenSet, GoogleError> {
            let mut state = self.0.lock().expect("transport lock");
            state.refreshes += 1;
            let refresh_token = state
                .rotate_refresh_next
                .then(|| SecretString::from("unexpected-rotated-refresh"));
            state.rotate_refresh_next = false;
            Ok(OAuthTokenSet {
                access_token: SecretString::from("refreshed-access"),
                refresh_token,
                expires_in_seconds: 3_600,
                token_type: "Bearer".to_owned(),
                granted_scopes: BTreeSet::new(),
                id_token: None,
            })
        }

        async fn identity(
            &self,
            _access_token: &SecretString,
        ) -> Result<GoogleIdentity, GoogleError> {
            let mut state = self.0.lock().expect("transport lock");
            if state.fail_next_identity {
                state.fail_next_identity = false;
                return Err(GoogleError::Temporary { status: 503 });
            }
            Ok(GoogleIdentity {
                subject: state.identity_subject.clone(),
                verified_email: Some(format!("{}@example.test", state.identity_subject)),
            })
        }

        async fn revoke(&self, token: &SecretString) -> Result<(), GoogleError> {
            let result = {
                let mut state = self.0.lock().expect("transport lock");
                state.revoked_tokens.push(token.expose_secret().to_owned());
                if state.revoke_failures_remaining > 0 {
                    state.revoke_failures_remaining -= 1;
                    Err(GoogleError::Temporary { status: 503 })
                } else if state.fail_next_revoke {
                    state.fail_next_revoke = false;
                    Err(GoogleError::Temporary { status: 503 })
                } else if state.already_revoked_next {
                    state.already_revoked_next = false;
                    Err(GoogleError::OAuthRejected {
                        code: "invalid_token".to_owned(),
                    })
                } else {
                    Ok(())
                }
            };
            let barrier = self.1.lock().expect("revoke barrier lock").take();
            if let Some((entered, release)) = barrier {
                entered.notify_one();
                release.notified().await;
            }
            result
        }
    }

    fn fixture(
        refreshes: impl IntoIterator<Item = Option<&'static str>>,
    ) -> (
        Arc<GoogleOAuthService>,
        Arc<InMemoryGoogleOAuthRepository>,
        Arc<FakeTransport>,
        Arc<TestClock>,
    ) {
        let repository = Arc::new(InMemoryGoogleOAuthRepository::default());
        let transport = Arc::new(FakeTransport::with_refreshes(refreshes));
        let clock = Arc::new(TestClock::new(
            "2026-08-29T10:00:00Z".parse().expect("time"),
        ));
        let cipher = SecretCipher::new(
            Arc::new(BTreeMap::from([(
                1,
                CredentialKey::from_test_bytes([7; 32]),
            )])),
            1,
        );
        let service = Arc::new(GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            cipher,
            OAuthScope {
                workspace_id: Uuid::from_u128(1),
                user_id: Uuid::from_u128(2),
            },
            clock.clone(),
            StdDuration::from_mins(10),
        ));
        (service, repository, transport, clock)
    }

    fn begin_input() -> BeginAuthorization {
        BeginAuthorization {
            owner_subject: "token:owner".to_owned(),
            services: BTreeSet::new(),
            force_consent: false,
            login_hint: None,
            account_id: None,
            connect_new: false,
            make_default: false,
        }
    }

    fn idempotency(label: &str) -> OAuthIdempotencyKey {
        OAuthIdempotencyKey {
            key: format!("oauth-test-{label}"),
            fingerprint: Sha256::digest(label.as_bytes()).into(),
        }
    }

    async fn pending_cleanup(
        service: &GoogleOAuthService,
        repository: &InMemoryGoogleOAuthRepository,
        label: &str,
        token: &str,
    ) -> Uuid {
        let started = service
            .begin(begin_input(), idempotency(label))
            .await
            .expect("begin cleanup session");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        let CallbackClaim::Exchange(claimed) = repository
            .claim_callback(
                hash_secret(&state),
                service.clock.now(),
                service.clock.now() - EXCHANGE_LEASE,
            )
            .await
            .expect("claim cleanup session")
        else {
            panic!("new session is exchangeable");
        };
        let sealed = service
            .cipher
            .seal(
                token.as_bytes(),
                &oauth_cleanup_token_aad(
                    service.scope.workspace_id,
                    service.scope.user_id,
                    claimed.id,
                ),
            )
            .expect("seal cleanup token");
        repository
            .hold_cleanup_token(claimed.id, sealed, service.clock.now())
            .await
            .expect("hold cleanup token");
        repository
            .abandon_authorization(claimed.id, service.clock.now())
            .await
            .expect("promote cleanup token");
        claimed.id
    }

    #[tokio::test]
    async fn callback_is_one_use_and_concurrent_exchange_is_single() {
        let (service, _, transport, _) = fixture([Some("refresh-one")]);
        let started = service
            .begin(begin_input(), idempotency("concurrent-start"))
            .await
            .expect("begin");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        let (first, second) = tokio::join!(
            service.callback(&state, "code-one"),
            service.callback(&state, "code-one")
        );
        assert!(first.is_ok() ^ second.is_ok());
        let rejected = if first.is_err() { first } else { second };
        assert!(matches!(
            rejected,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::InvalidCallbackState
            ))
        ));
        assert_eq!(transport.0.lock().expect("transport lock").exchanges, 1);
        assert!(matches!(
            service.callback(&state, "code-one").await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::InvalidCallbackState
            ))
        ));
    }

    #[tokio::test]
    async fn expired_callback_is_consumed_without_network_exchange() {
        let (service, _, transport, clock) = fixture([Some("refresh-one")]);
        let started = service
            .begin(begin_input(), idempotency("expired-start"))
            .await
            .expect("begin");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        clock.advance(TimeDelta::minutes(11));
        assert!(matches!(
            service.callback(&state, "code").await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::InvalidCallbackState
            ))
        ));
        assert_eq!(transport.0.lock().expect("transport lock").exchanges, 0);
    }

    #[tokio::test]
    async fn incremental_authorization_retains_omitted_refresh_token() {
        let (service, _, transport, _) = fixture([Some("refresh-one"), None]);
        let first = service
            .begin(begin_input(), idempotency("incremental-first"))
            .await
            .expect("first begin");
        let first_state = FakeTransport::state_from_url(&first.authorization_url);
        let first_account = service
            .callback(&first_state, "code-one")
            .await
            .expect("first callback");
        assert_eq!(first_account.revision, 1);

        let mut promoted = begin_input();
        promoted.services = BTreeSet::from([GoogleService::Tasks]);
        let second = service
            .begin(promoted, idempotency("incremental-second"))
            .await
            .expect("second begin");
        let second_state = FakeTransport::state_from_url(&second.authorization_url);
        let account = service
            .callback(&second_state, "code-two")
            .await
            .expect("refresh token retained");
        assert_eq!(account.revision, 2);
        assert_eq!(account.external_account_id, "google-user-one");
        {
            let transport_state = transport.0.lock().expect("transport lock");
            assert!(transport_state.begins[0].force_consent);
            assert!(!transport_state.begins[1].force_consent);
            assert!(
                transport_state.begins[1]
                    .scopes
                    .contains(GOOGLE_CALENDAR_READONLY_SCOPE)
            );
            assert!(
                transport_state.begins[1]
                    .scopes
                    .contains(GOOGLE_TASKS_SCOPE)
            );
            assert!(
                !transport_state.begins[1]
                    .scopes
                    .contains(GOOGLE_TASKS_READONLY_SCOPE)
            );
        }

        service
            .disconnect(
                account.id,
                account.revision,
                idempotency("incremental-disconnect"),
            )
            .await
            .expect("disconnect");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["refresh-one"]
        );
    }

    #[tokio::test]
    async fn sync_access_refresh_is_encrypted_revisioned_and_rejects_unexpected_rotation() {
        let (service, repository, transport, clock) = fixture([Some("refresh-one")]);
        let started = service
            .begin(begin_input(), idempotency("sync-refresh-connect"))
            .await
            .expect("authorization start");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        let account = service
            .callback(&state, "code")
            .await
            .expect("account installed");

        let reused = service
            .access_token_for_sync(account.id)
            .await
            .expect("unexpired access reused");
        assert_eq!(reused.expose_secret(), "access-1");
        assert_eq!(transport.0.lock().expect("transport lock").refreshes, 0);

        clock.advance(TimeDelta::minutes(59));
        let refreshed = service
            .access_token_for_sync(account.id)
            .await
            .expect("expiring access refreshed");
        assert_eq!(refreshed.expose_secret(), "refreshed-access");
        assert_eq!(transport.0.lock().expect("transport lock").refreshes, 1);
        let stored = repository
            .account_by_id(account.id)
            .await
            .expect("account query")
            .expect("account");
        assert_eq!(stored.account.revision, account.revision + 1);
        let opened = service
            .open_credentials(&stored)
            .expect("encrypted credentials");
        assert_eq!(opened.access_token.expose_secret(), "refreshed-access");
        assert_eq!(opened.refresh_token.expose_secret(), "refresh-one");

        clock.advance(TimeDelta::minutes(59));
        transport
            .0
            .lock()
            .expect("transport lock")
            .rotate_refresh_next = true;
        assert!(matches!(
            service.access_token_for_sync(account.id).await,
            Err(GoogleOAuthServiceError::InvalidTokenResponse)
        ));
        let unchanged = repository
            .account_by_id(account.id)
            .await
            .expect("account query")
            .expect("account");
        assert_eq!(unchanged.account.revision, stored.account.revision);
    }

    #[tokio::test]
    async fn selected_account_rejects_a_different_verified_google_identity() {
        let (service, repository, transport, _) = fixture([Some("refresh-one"), None]);
        let first = service
            .begin(begin_input(), idempotency("identity-first"))
            .await
            .expect("first begin");
        let first_state = FakeTransport::state_from_url(&first.authorization_url);
        let original = service
            .callback(&first_state, "code-one")
            .await
            .expect("first callback");
        transport.0.lock().expect("transport lock").identity_subject = "google-user-two".to_owned();
        let second = service
            .begin(begin_input(), idempotency("identity-second"))
            .await
            .expect("second begin");
        let second_state = FakeTransport::state_from_url(&second.authorization_url);
        assert!(matches!(
            service.callback(&second_state, "code-two").await,
            Err(GoogleOAuthServiceError::IdentityMismatch)
        ));
        let retained = repository
            .account()
            .await
            .expect("account lookup")
            .expect("original credentials retained");
        assert_eq!(retained.account.id, original.id);
        assert_eq!(retained.account.external_account_id, "google-user-one");
        assert_eq!(retained.account.revision, 1);
    }

    #[tokio::test]
    async fn reauthorization_blocks_until_old_encryption_key_is_restored() {
        let (service, repository, transport, clock) =
            fixture([Some("refresh-one"), Some("refresh-two")]);
        let first = service
            .begin(begin_input(), idempotency("rotation-first"))
            .await
            .expect("first begin");
        let first_state = FakeTransport::state_from_url(&first.authorization_url);
        let original = service
            .callback(&first_state, "code-one")
            .await
            .expect("first callback");

        let missing_old_key = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            SecretCipher::new(
                Arc::new(BTreeMap::from([(
                    2,
                    CredentialKey::from_test_bytes([8; 32]),
                )])),
                2,
            ),
            OAuthScope {
                workspace_id: Uuid::from_u128(1),
                user_id: Uuid::from_u128(2),
            },
            clock.clone(),
            StdDuration::from_mins(10),
        );
        assert!(matches!(
            missing_old_key
                .begin(begin_input(), idempotency("rotation-blocked"))
                .await,
            Err(GoogleOAuthServiceError::Crypto(
                CryptoError::UnknownKeyVersion
            ))
        ));
        assert_eq!(transport.0.lock().expect("transport lock").begins.len(), 1);
        let retained = repository
            .account()
            .await
            .expect("repository")
            .expect("old encrypted credential retained");
        assert_eq!(retained.account, original);

        let restored = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            SecretCipher::new(
                Arc::new(BTreeMap::from([
                    (1, CredentialKey::from_test_bytes([7; 32])),
                    (2, CredentialKey::from_test_bytes([8; 32])),
                ])),
                2,
            ),
            OAuthScope {
                workspace_id: Uuid::from_u128(1),
                user_id: Uuid::from_u128(2),
            },
            clock,
            StdDuration::from_mins(10),
        );
        let second = restored
            .begin(begin_input(), idempotency("rotation-second"))
            .await
            .expect("begin after old key restoration");
        let second_state = FakeTransport::state_from_url(&second.authorization_url);
        let recovered = restored
            .callback(&second_state, "code-two")
            .await
            .expect("new credential installs only after old credential is readable");
        assert_eq!(recovered.id, original.id);
        assert_eq!(recovered.revision, 2);
        restored
            .disconnect(
                recovered.id,
                recovered.revision,
                idempotency("rotation-disconnect"),
            )
            .await
            .expect("both retained credentials are revocable");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["refresh-two", "refresh-one"]
        );
    }

    #[tokio::test]
    async fn held_refresh_survives_restart_and_stale_exchange_cleanup() {
        let (service, repository, transport, clock) = fixture([]);
        let started = service
            .begin(begin_input(), idempotency("crash-start"))
            .await
            .expect("begin");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        let CallbackClaim::Exchange(claimed) = repository
            .claim_callback(
                hash_secret(&state),
                clock.now(),
                clock.now() - EXCHANGE_LEASE,
            )
            .await
            .expect("claim exchange")
        else {
            panic!("new session must be exchangeable");
        };
        let sealed = service
            .cipher
            .seal(
                b"orphan-refresh",
                &oauth_cleanup_token_aad(
                    service.scope.workspace_id,
                    service.scope.user_id,
                    claimed.id,
                ),
            )
            .expect("seal cleanup credential");
        repository
            .hold_cleanup_token(claimed.id, sealed, clock.now())
            .await
            .expect("durably hold refresh token");
        assert_eq!(
            repository.cleanup_status().await.expect("cleanup status"),
            GoogleOAuthCleanupStatus {
                held: 1,
                pending: 0,
                retrying: 0,
                exhausted: 0,
                volatile_guardians: 0,
                durability_degraded: false,
                revocation_fenced: false,
                operator_recovery_required: false,
                uncertain_authorizations: 1,
                legacy_recovery_required: 0,
                next_attempt_at: None,
                last_failure_at: None,
            }
        );

        clock.advance(TimeDelta::minutes(3));
        let restarted = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            service.cipher.clone(),
            service.scope,
            clock,
            StdDuration::from_mins(10),
        );
        restarted
            .recover_startup()
            .await
            .expect("startup recovery promotes the durable orphan");
        let (accounts, cleanup) = restarted
            .accounts_with_cleanup()
            .await
            .expect("restart reconciles one stale exchange");
        assert!(accounts.is_empty());
        assert_eq!(cleanup.held + cleanup.pending + cleanup.retrying, 0);
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["orphan-refresh"]
        );
    }

    #[tokio::test]
    async fn failed_cleanup_revocation_is_observable_and_retried_after_restart() {
        let (service, repository, transport, clock) = fixture([Some("cleanup-refresh")]);
        let started = service
            .begin(begin_input(), idempotency("cleanup-retry-start"))
            .await
            .expect("begin");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        {
            let mut transport_state = transport.0.lock().expect("transport lock");
            transport_state.fail_next_identity = true;
            transport_state.fail_next_revoke = true;
        }
        assert!(matches!(
            service.callback(&state, "code").await,
            Err(GoogleOAuthServiceError::Google(GoogleError::Temporary {
                status: 503
            }))
        ));
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert_eq!(status.pending, 1);
        assert_eq!(status.held, 0);
        assert!(status.last_failure_at.is_some());

        clock.advance(TimeDelta::seconds(1));
        let restarted = GoogleOAuthService::new(
            repository,
            transport.clone(),
            service.cipher.clone(),
            service.scope,
            clock,
            StdDuration::from_mins(10),
        );
        let (_, status) = restarted
            .accounts_with_cleanup()
            .await
            .expect("next safe entry retries cleanup");
        assert_eq!(status.held + status.pending + status.retrying, 0);
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["cleanup-refresh", "cleanup-refresh"]
        );
    }

    #[tokio::test]
    async fn staged_resolution_is_exact_and_concurrent_consumers_share_installed_account() {
        let (service, repository, transport, clock) = fixture([]);
        let started = service
            .begin(begin_input(), idempotency("lost-ack-start"))
            .await
            .expect("begin");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        let CallbackClaim::Exchange(claimed) = repository
            .claim_callback(
                hash_secret(&state),
                clock.now(),
                clock.now() - EXCHANGE_LEASE,
            )
            .await
            .expect("claim exchange")
        else {
            panic!("new session must be exchangeable");
        };
        let cleanup = service
            .cipher
            .seal(
                b"installed-refresh",
                &oauth_cleanup_token_aad(
                    service.scope.workspace_id,
                    service.scope.user_id,
                    claimed.id,
                ),
            )
            .expect("seal cleanup token");
        repository
            .hold_cleanup_token(claimed.id, cleanup, clock.now())
            .await
            .expect("hold cleanup token");
        let account_id = Uuid::new_v4();
        let credentials = StoredCredentials {
            access_token: SecretString::from("installed-access"),
            refresh_token: SecretString::from("installed-refresh"),
            retired_refresh_tokens: Vec::new(),
            token_type: "Bearer".to_owned(),
            access_expires_at: clock.now() + TimeDelta::hours(1),
        };
        repository
            .stage_authorization(AuthorizationCompletion {
                session_id: claimed.id,
                owner_subject_hash: claimed.owner_subject_hash,
                expected_account_revision: None,
                account_id,
                make_default: false,
                external_account_id: "lost-ack-user".to_owned(),
                display_label: "lost-ack@example.test".to_owned(),
                credentials: super::super::domain::EncryptedCredentials {
                    sealed: service
                        .seal_credentials(account_id, &credentials)
                        .expect("seal installed credentials"),
                },
                granted_scopes: claimed.requested_scopes,
                token_expires_at: credentials.access_expires_at,
                now: clock.now(),
            })
            .await
            .expect("stage committed even though caller can lose acknowledgement");
        assert!(matches!(
            repository
                .resolve_authorization(claimed.id)
                .await
                .expect("exact resolution"),
            AuthorizationResolution::Staged
        ));

        let (first, second) = tokio::join!(
            repository.complete_staged_authorization(claimed.id),
            repository.complete_staged_authorization(claimed.id)
        );
        let first = first.expect("first consumer");
        let second = second.expect("concurrent consumer observes installed account");
        assert_eq!(first, second);
        assert_eq!(first.id, account_id);
        assert!(matches!(
            repository
                .resolve_authorization(claimed.id)
                .await
                .expect("consumed resolution"),
            AuthorizationResolution::Consumed(account) if account == first
        ));
        assert_eq!(
            repository
                .cleanup_status()
                .await
                .expect("cleanup status")
                .held,
            0
        );
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
    }

    #[tokio::test]
    async fn multiple_google_accounts_have_stable_ids_and_explicit_default_selection() {
        let (service, _, transport, _) = fixture([
            Some("refresh-one"),
            Some("refresh-two"),
            Some("refresh-two-next"),
        ]);
        let first_start = service
            .begin(begin_input(), idempotency("multi-first"))
            .await
            .expect("first begin");
        let first = service
            .callback(
                &FakeTransport::state_from_url(&first_start.authorization_url),
                "code-one",
            )
            .await
            .expect("first account");
        transport.0.lock().expect("transport lock").identity_subject = "google-user-two".to_owned();
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let second_start = service
            .begin(connect_new, idempotency("multi-second"))
            .await
            .expect("second begin");
        let second = service
            .callback(
                &FakeTransport::state_from_url(&second_start.authorization_url),
                "code-two",
            )
            .await
            .expect("second account");
        assert_ne!(first.id, second.id);
        let (accounts, _) = service
            .accounts_with_cleanup()
            .await
            .expect("list both accounts");
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts.iter().filter(|account| account.is_default).count(),
            1
        );
        assert_eq!(accounts[0].id, first.id);

        let mut select_second = begin_input();
        select_second.account_id = Some(second.id);
        select_second.make_default = true;
        let selection = service
            .begin(select_second, idempotency("multi-select-second"))
            .await
            .expect("select second account explicitly");
        let selected = service
            .callback(
                &FakeTransport::state_from_url(&selection.authorization_url),
                "code-three",
            )
            .await
            .expect("reauthorize selected account");
        assert_eq!(selected.id, second.id);
        assert!(selected.is_default);
        let (accounts, _) = service
            .accounts_with_cleanup()
            .await
            .expect("list selected default");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].id, second.id);
        assert!(accounts[0].is_default);
        assert!(!accounts[1].is_default);
    }

    #[tokio::test]
    async fn connect_new_rejects_duplicate_identity_without_wedging_or_revoking_current_token() {
        let (service, repository, transport, _) =
            fixture([Some("refresh-current"), Some("refresh-duplicate")]);
        let first_start = service
            .begin(begin_input(), idempotency("duplicate-first"))
            .await
            .expect("first begin");
        let current = service
            .callback(
                &FakeTransport::state_from_url(&first_start.authorization_url),
                "code-one",
            )
            .await
            .expect("first account");
        let mut duplicate_input = begin_input();
        duplicate_input.connect_new = true;
        let duplicate_start = service
            .begin(duplicate_input, idempotency("duplicate-new"))
            .await
            .expect("duplicate flow begins before verified identity is known");
        assert!(matches!(
            service
                .callback(
                    &FakeTransport::state_from_url(&duplicate_start.authorization_url),
                    "code-two"
                )
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::AuthorizationConflict
            ))
        ));
        let retained = repository
            .account()
            .await
            .expect("account lookup")
            .expect("current account retained");
        assert_eq!(retained.account, current);
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert_eq!(status.held + status.pending + status.retrying, 0);
        service
            .begin(begin_input(), idempotency("duplicate-recovery"))
            .await
            .expect("failed duplicate does not leave staged authorization wedged");
    }

    #[tokio::test]
    async fn failed_revocation_retains_encrypted_credentials_for_retry() {
        let (service, repository, transport, _) = fixture([Some("refresh-one")]);
        let started = service
            .begin(begin_input(), idempotency("failure-start"))
            .await
            .expect("begin");
        let state = FakeTransport::state_from_url(&started.authorization_url);
        let account = service.callback(&state, "code").await.expect("callback");
        transport.0.lock().expect("transport lock").fail_next_revoke = true;

        assert!(matches!(
            service
                .disconnect(
                    account.id,
                    account.revision,
                    idempotency("failure-disconnect"),
                )
                .await,
            Err(GoogleOAuthServiceError::Google(GoogleError::Temporary {
                status: 503
            }))
        ));
        let retained = repository
            .account()
            .await
            .expect("repository")
            .expect("credentials retained");
        assert_eq!(
            retained.account.status,
            GoogleAccountStatus::RevocationFailed
        );
        transport
            .0
            .lock()
            .expect("transport lock")
            .already_revoked_next = true;
        let disconnected = service
            .disconnect(
                retained.account.id,
                account.revision,
                idempotency("failure-disconnect"),
            )
            .await
            .expect("retry disconnect");
        assert_eq!(disconnected.account.status, GoogleAccountStatus::Revoked);
        assert!(repository.account().await.expect("repository").is_none());
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["refresh-one", "refresh-one"]
        );
    }

    #[tokio::test]
    async fn unknown_post_exchange_identity_requires_verified_operator_recovery() {
        let (service, repository, transport, _) =
            fixture([Some("refresh-one"), Some("refresh-two")]);
        let first = service
            .begin(begin_input(), idempotency("cleanup-first"))
            .await
            .expect("first begin");
        let first_state = FakeTransport::state_from_url(&first.authorization_url);
        let original = service
            .callback(&first_state, "code-one")
            .await
            .expect("first callback");

        let second = service
            .begin(begin_input(), idempotency("cleanup-second"))
            .await
            .expect("second begin");
        let second_state = FakeTransport::state_from_url(&second.authorization_url);
        transport
            .0
            .lock()
            .expect("transport lock")
            .fail_next_identity = true;
        assert!(matches!(
            service.callback(&second_state, "code-two").await,
            Err(GoogleOAuthServiceError::Google(GoogleError::Temporary {
                status: 503
            }))
        ));
        let retained = repository
            .account()
            .await
            .expect("repository")
            .expect("original account retained");
        assert_eq!(retained.account, original);
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert!(status.operator_recovery_required);
        assert_eq!(status.uncertain_authorizations, 0);
        assert!(status.revocation_fenced);
        assert!(matches!(
            service.acknowledge_operator_recovery(false).await,
            Err(GoogleOAuthServiceError::OperatorConfirmationRequired)
        ));
        let recovered = service
            .acknowledge_operator_recovery(true)
            .await
            .expect("operator confirms grant-wide provider recovery");
        assert_eq!(recovered.accounts_marked_reauthorization_required, 1);
        assert_eq!(recovered.legacy_accounts_finalized, 0);
        let retained = repository
            .account()
            .await
            .expect("repository")
            .expect("account retained for reauthorization");
        assert_eq!(
            retained.account.status,
            GoogleAccountStatus::ReauthorizationRequired
        );
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert!(!status.operator_recovery_required);
        assert!(!status.revocation_fenced);
        assert_eq!(status.uncertain_authorizations, 0);
    }

    #[tokio::test]
    async fn cleanup_never_revokes_a_refresh_credential_already_retained_by_account() {
        let (service, repository, transport, _) =
            fixture([Some("refresh-same"), Some("refresh-same")]);
        let first = service
            .begin(begin_input(), idempotency("same-refresh-first"))
            .await
            .expect("first begin");
        let current = service
            .callback(
                &FakeTransport::state_from_url(&first.authorization_url),
                "code-one",
            )
            .await
            .expect("first callback");
        let second = service
            .begin(begin_input(), idempotency("same-refresh-second"))
            .await
            .expect("second begin");
        transport
            .0
            .lock()
            .expect("transport lock")
            .omit_tasks_scope_next = true;
        assert!(matches!(
            service
                .callback(
                    &FakeTransport::state_from_url(&second.authorization_url),
                    "code-two"
                )
                .await,
            Err(GoogleOAuthServiceError::MissingRequestedScopes)
        ));
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let retained = repository
            .account()
            .await
            .expect("repository")
            .expect("current credential remains");
        assert_eq!(retained.account, current);
        assert_eq!(
            repository
                .cleanup_status()
                .await
                .expect("cleanup status")
                .pending,
            0
        );
        service
            .disconnect(
                current.id,
                current.revision,
                idempotency("same-refresh-disconnect"),
            )
            .await
            .expect("retained current credential remains revocable");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["refresh-same"]
        );
    }

    #[tokio::test]
    async fn failed_disconnect_fence_blocks_reauthorization_until_exact_retry_finishes() {
        let (service, repository, transport, _) = fixture([Some("refresh-one")]);
        let first = service
            .begin(begin_input(), idempotency("state-first"))
            .await
            .expect("first begin");
        let first_state = FakeTransport::state_from_url(&first.authorization_url);
        let account = service
            .callback(&first_state, "code-one")
            .await
            .expect("first callback");
        transport.0.lock().expect("transport lock").fail_next_revoke = true;
        assert!(
            service
                .disconnect(
                    account.id,
                    account.revision,
                    idempotency("state-disconnect"),
                )
                .await
                .is_err()
        );
        let failed = repository
            .account()
            .await
            .expect("repository")
            .expect("failed account retained");
        assert_eq!(failed.account.status, GoogleAccountStatus::RevocationFailed);

        assert!(matches!(
            service
                .begin(begin_input(), idempotency("state-reauthorize"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
        let revoked = service
            .disconnect(
                account.id,
                account.revision,
                idempotency("state-disconnect"),
            )
            .await
            .expect("same operation retries behind its fence");
        assert_eq!(revoked.account.status, GoogleAccountStatus::Revoked);
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["refresh-one", "refresh-one"]
        );
    }

    #[tokio::test]
    async fn replacement_refresh_is_retained_for_eventual_full_revocation() {
        let (service, _, transport, _) = fixture([Some("refresh-one"), Some("refresh-two")]);
        let first = service
            .begin(begin_input(), idempotency("retire-first"))
            .await
            .expect("first begin");
        let first_state = FakeTransport::state_from_url(&first.authorization_url);
        service
            .callback(&first_state, "code-one")
            .await
            .expect("first callback");
        let second = service
            .begin(begin_input(), idempotency("retire-second"))
            .await
            .expect("second begin");
        let second_state = FakeTransport::state_from_url(&second.authorization_url);
        let account = service
            .callback(&second_state, "code-two")
            .await
            .expect("replacement callback");

        service
            .disconnect(
                account.id,
                account.revision,
                idempotency("retire-disconnect"),
            )
            .await
            .expect("all accepted credentials revoked");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["refresh-two", "refresh-one"]
        );
    }

    #[tokio::test]
    async fn disconnect_revokes_selected_identity_even_if_fake_tokens_share_bytes() {
        let (service, repository, transport, _) =
            fixture([Some("shared-refresh"), Some("shared-refresh")]);
        let first_start = service
            .begin(begin_input(), idempotency("shared-disconnect-first"))
            .await
            .expect("first begin");
        let first = service
            .callback(
                &FakeTransport::state_from_url(&first_start.authorization_url),
                "first-code",
            )
            .await
            .expect("first account");
        transport.0.lock().expect("transport lock").identity_subject = "google-user-two".to_owned();
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let second_start = service
            .begin(connect_new, idempotency("shared-disconnect-second"))
            .await
            .expect("second begin");
        let second = service
            .callback(
                &FakeTransport::state_from_url(&second_start.authorization_url),
                "second-code",
            )
            .await
            .expect("second account");

        service
            .disconnect(first.id, first.revision, idempotency("shared-disconnect"))
            .await
            .expect("disconnect only removes the selected account");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["shared-refresh"]
        );
        let retained = repository.accounts().await.expect("retained account");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].account.id, second.id);
    }

    #[tokio::test]
    async fn disconnect_revokes_all_selected_grant_credentials_and_preserves_other_identity() {
        let (service, repository, transport, _) = fixture([
            Some("shared-retired"),
            Some("first-current"),
            Some("second-current"),
        ]);
        let first_start = service
            .begin(begin_input(), idempotency("retired-disconnect-first"))
            .await
            .expect("first begin");
        service
            .callback(
                &FakeTransport::state_from_url(&first_start.authorization_url),
                "first-code",
            )
            .await
            .expect("first account");
        let rotate_start = service
            .begin(begin_input(), idempotency("retired-disconnect-rotate"))
            .await
            .expect("rotation begin");
        let rotated = service
            .callback(
                &FakeTransport::state_from_url(&rotate_start.authorization_url),
                "rotate-code",
            )
            .await
            .expect("rotation");
        transport.0.lock().expect("transport lock").identity_subject = "google-user-two".to_owned();
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let second_start = service
            .begin(connect_new, idempotency("retired-disconnect-second"))
            .await
            .expect("second begin");
        let second = service
            .callback(
                &FakeTransport::state_from_url(&second_start.authorization_url),
                "second-code",
            )
            .await
            .expect("second account");

        service
            .disconnect(
                rotated.id,
                rotated.revision,
                idempotency("retired-disconnect"),
            )
            .await
            .expect("disconnect");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["first-current", "shared-retired"]
        );
        let retained = repository.accounts().await.expect("retained account");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].account.id, second.id);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn disconnect_fence_and_generation_block_every_concurrent_install() {
        let (service, repository, _, clock) = fixture([Some("fenced-refresh")]);
        let started = service
            .begin(begin_input(), idempotency("disconnect-fence-start"))
            .await
            .expect("begin");
        let account = service
            .callback(
                &FakeTransport::state_from_url(&started.authorization_url),
                "code",
            )
            .await
            .expect("account");
        let operation_key = idempotency("disconnect-fence-operation");
        let operation = GoogleOAuthService::idempotency(
            "google.account.disconnect",
            &operation_key,
            clock.now(),
            MUTATION_IDEMPOTENCY_TTL,
        )
        .expect("idempotency");
        let claim_id = Uuid::new_v4();
        let DisconnectMutation::Execute(claim) = repository
            .claim_disconnect(
                account.id,
                account.revision,
                claim_id,
                clock.now(),
                clock.now() - DISCONNECT_LEASE,
                clock.now() - EXCHANGE_LEASE,
                operation.clone(),
            )
            .await
            .expect("disconnect claim")
        else {
            panic!("disconnect executes");
        };
        assert!(
            repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        assert!(matches!(
            service
                .begin(connect_new, idempotency("disconnect-fence-connect"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
        assert!(matches!(
            repository
                .claim_volatile_revocation(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    clock.now(),
                    clock.now() - GUARDIAN_LEASE,
                )
                .await,
            Err(GoogleOAuthRepositoryError::RevocationInProgress)
        ));
        assert!(matches!(
            repository
                .complete_disconnect(
                    account.id,
                    claim_id,
                    claim.credential_generation + 1,
                    clock.now(),
                    operation.clone(),
                )
                .await,
            Err(GoogleOAuthRepositoryError::CleanupClaimLost)
        ));
        assert_eq!(
            repository
                .fail_disconnect(
                    account.id,
                    claim_id,
                    claim.credential_generation + 1,
                    clock.now(),
                )
                .await,
            Err(GoogleOAuthRepositoryError::CleanupClaimLost)
        );
        repository
            .complete_disconnect(
                account.id,
                claim_id,
                claim.credential_generation,
                clock.now(),
                operation,
            )
            .await
            .expect("exact owner, claim, and generation clear the fence");
        assert!(
            !repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
    }

    #[tokio::test]
    async fn disconnect_keeps_scope_fenced_while_provider_revocation_is_in_flight() {
        let (service, repository, transport, _) = fixture([Some("barrier-refresh")]);
        let started = service
            .begin(begin_input(), idempotency("disconnect-barrier-start"))
            .await
            .expect("begin");
        let account = service
            .callback(
                &FakeTransport::state_from_url(&started.authorization_url),
                "code",
            )
            .await
            .expect("account");
        let (entered, release) = transport.pause_next_revoke();
        let disconnect_service = service.clone();
        let disconnect = tokio::spawn(async move {
            disconnect_service
                .disconnect(
                    account.id,
                    account.revision,
                    idempotency("disconnect-barrier-operation"),
                )
                .await
        });
        tokio::time::timeout(StdDuration::from_secs(2), entered.notified())
            .await
            .expect("provider revoke reached deterministic barrier");
        assert!(
            repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        assert!(matches!(
            service
                .begin(connect_new, idempotency("disconnect-barrier-connect"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
        release.notify_one();
        disconnect
            .await
            .expect("disconnect task")
            .expect("disconnect completes after provider result");
        assert!(
            !repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn disconnect_does_not_decrypt_a_different_google_identity() {
        let (service, repository, transport, clock) =
            fixture([Some("opaque-retained"), Some("selected-refresh")]);
        let first_start = service
            .begin(begin_input(), idempotency("disconnect-opaque-first"))
            .await
            .expect("first begin");
        let first = service
            .callback(
                &FakeTransport::state_from_url(&first_start.authorization_url),
                "first-code",
            )
            .await
            .expect("first account");
        let both_keys = SecretCipher::new(
            Arc::new(BTreeMap::from([
                (1, CredentialKey::from_test_bytes([7; 32])),
                (2, CredentialKey::from_test_bytes([8; 32])),
            ])),
            2,
        );
        let key_two_writer = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            both_keys.clone(),
            service.scope,
            clock.clone(),
            StdDuration::from_mins(10),
        );
        transport.0.lock().expect("transport lock").identity_subject = "google-user-two".to_owned();
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let second_start = key_two_writer
            .begin(connect_new, idempotency("disconnect-opaque-second"))
            .await
            .expect("second begin");
        let second = key_two_writer
            .callback(
                &FakeTransport::state_from_url(&second_start.authorization_url),
                "second-code",
            )
            .await
            .expect("second account");
        let key_two_only = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            SecretCipher::new(
                Arc::new(BTreeMap::from([(
                    2,
                    CredentialKey::from_test_bytes([8; 32]),
                )])),
                2,
            ),
            service.scope,
            clock.clone(),
            StdDuration::from_mins(10),
        );
        key_two_only
            .disconnect(
                second.id,
                second.revision,
                idempotency("disconnect-opaque-operation"),
            )
            .await
            .expect("a different Google identity is not part of the selected grant");
        assert_eq!(
            transport.0.lock().expect("transport lock").revoked_tokens,
            vec!["selected-refresh"]
        );
        assert!(
            !repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
        let retained = repository.accounts().await.expect("retained account");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].account.id, first.id);
    }

    #[tokio::test]
    async fn scope_fence_blocks_every_install_phase_until_claim_is_finalized() {
        let (service, repository, _, clock) = fixture([]);
        let session_id = pending_cleanup(&service, &repository, "fence-source", "orphan").await;
        let claim = repository
            .claim_cleanup(
                Uuid::new_v4(),
                clock.now(),
                clock.now() - GUARDIAN_LEASE,
                clock.now() - EXCHANGE_LEASE,
                Some(session_id),
            )
            .await
            .expect("claim cleanup")
            .expect("pending cleanup exists");
        assert!(matches!(
            service
                .begin(begin_input(), idempotency("fence-begin"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
        let dummy = AuthorizationCompletion {
            session_id,
            owner_subject_hash: hash_secret("owner"),
            expected_account_revision: None,
            account_id: Uuid::new_v4(),
            make_default: false,
            external_account_id: "unused".to_owned(),
            display_label: "unused".to_owned(),
            credentials: super::super::domain::EncryptedCredentials {
                sealed: claim.encrypted_refresh_token.clone(),
            },
            granted_scopes: BTreeSet::new(),
            token_expires_at: clock.now(),
            now: clock.now(),
        };
        assert_eq!(
            repository.stage_authorization(dummy).await,
            Err(GoogleOAuthRepositoryError::RevocationInProgress)
        );
        assert_eq!(
            repository.complete_staged_authorization(session_id).await,
            Err(GoogleOAuthRepositoryError::RevocationInProgress)
        );
        repository
            .complete_cleanup(
                claim.session_id,
                claim.claim_id,
                claim.credential_generation,
                clock.now(),
            )
            .await
            .expect("definitive revocation clears fence");
        service
            .begin(begin_input(), idempotency("fence-after"))
            .await
            .expect("authorization resumes only after fence clears");
    }

    #[tokio::test]
    async fn invalid_public_callbacks_never_amplify_pending_cleanup() {
        let (service, repository, transport, _) = fixture([]);
        pending_cleanup(&service, &repository, "bogus-source", "orphan").await;
        for suffix in 0..8 {
            let state = format!("bogus-callback-{suffix:0>32}");
            assert!(service.callback(&state, "code").await.is_err());
            assert!(service.callback_denied(&state).await.is_err());
        }
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        assert_eq!(
            repository
                .cleanup_status()
                .await
                .expect("cleanup retained")
                .pending,
            1
        );
    }

    #[tokio::test]
    async fn exhausted_cleanup_is_visible_retained_and_remains_fail_closed() {
        let (service, repository, _, clock) = fixture([]);
        let session_id = pending_cleanup(&service, &repository, "exhaust-source", "orphan").await;
        for attempt in 1..=super::super::MAX_CLEANUP_ATTEMPTS {
            let now = clock.now();
            let claim = repository
                .claim_cleanup(
                    Uuid::new_v4(),
                    now,
                    now - DISCONNECT_LEASE,
                    now - EXCHANGE_LEASE,
                    Some(session_id),
                )
                .await
                .expect("claim cleanup")
                .expect("attempt below cap is claimable");
            assert_eq!(claim.attempt, attempt);
            repository
                .fail_cleanup(
                    claim.session_id,
                    claim.claim_id,
                    claim.credential_generation,
                    now,
                    now + TimeDelta::seconds(1),
                )
                .await
                .expect("retain failed cleanup");
            clock.advance(TimeDelta::seconds(1));
        }
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert_eq!(status.pending, 1);
        assert_eq!(status.exhausted, 1);
        assert!(
            repository
                .claim_cleanup(
                    Uuid::new_v4(),
                    clock.now(),
                    clock.now() - GUARDIAN_LEASE,
                    clock.now() - EXCHANGE_LEASE,
                    None,
                )
                .await
                .expect("cap query")
                .is_none()
        );
        assert!(matches!(
            service
                .begin(begin_input(), idempotency("exhaust-blocked"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
    }

    #[tokio::test]
    async fn duplicate_connect_returning_installed_token_never_revokes_it() {
        let (service, repository, transport, _) = fixture([Some("same-token"), Some("same-token")]);
        let first = service
            .begin(begin_input(), idempotency("same-connect-first"))
            .await
            .expect("first begin");
        service
            .callback(
                &FakeTransport::state_from_url(&first.authorization_url),
                "first-code",
            )
            .await
            .expect("first install");
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let duplicate = service
            .begin(connect_new, idempotency("same-connect-duplicate"))
            .await
            .expect("duplicate begin");
        assert!(
            service
                .callback(
                    &FakeTransport::state_from_url(&duplicate.authorization_url),
                    "duplicate-code",
                )
                .await
                .is_err()
        );
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert_eq!(status.held + status.pending + status.retrying, 0);
    }

    #[tokio::test]
    async fn retired_token_is_also_protected_across_connect_new_cleanup() {
        let (service, repository, transport, _) = fixture([
            Some("retired-token"),
            Some("current-token"),
            Some("retired-token"),
        ]);
        let first = service
            .begin(begin_input(), idempotency("retired-protect-first"))
            .await
            .expect("first begin");
        service
            .callback(
                &FakeTransport::state_from_url(&first.authorization_url),
                "first-code",
            )
            .await
            .expect("first install");
        let rotate = service
            .begin(begin_input(), idempotency("retired-protect-rotate"))
            .await
            .expect("rotate begin");
        service
            .callback(
                &FakeTransport::state_from_url(&rotate.authorization_url),
                "rotate-code",
            )
            .await
            .expect("rotate token");
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let duplicate = service
            .begin(connect_new, idempotency("retired-protect-duplicate"))
            .await
            .expect("duplicate begin");
        assert!(
            service
                .callback(
                    &FakeTransport::state_from_url(&duplicate.authorization_url),
                    "duplicate-code",
                )
                .await
                .is_err()
        );
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        assert_eq!(
            repository
                .cleanup_status()
                .await
                .expect("cleanup status")
                .pending,
            0
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn unknown_identity_cleanup_requires_explicit_operator_recovery() {
        let (service, repository, transport, clock) =
            fixture([Some("account-token"), Some("orphan-token")]);
        let first = service
            .begin(begin_input(), idempotency("opaque-first"))
            .await
            .expect("first begin");
        service
            .callback(
                &FakeTransport::state_from_url(&first.authorization_url),
                "first-code",
            )
            .await
            .expect("first install");
        let key_two_only = SecretCipher::new(
            Arc::new(BTreeMap::from([(
                2,
                CredentialKey::from_test_bytes([8; 32]),
            )])),
            2,
        );
        let key_two_service = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            key_two_only,
            service.scope,
            clock.clone(),
            StdDuration::from_mins(10),
        );
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        let second = key_two_service
            .begin(connect_new, idempotency("opaque-second"))
            .await
            .expect("connect new does not overwrite opaque account");
        transport
            .0
            .lock()
            .expect("transport lock")
            .fail_next_identity = true;
        assert!(
            key_two_service
                .callback(
                    &FakeTransport::state_from_url(&second.authorization_url),
                    "second-code",
                )
                .await
                .is_err()
        );
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let cleanup = repository.cleanup_status().await.expect("cleanup retained");
        assert_eq!(cleanup.pending, 0);
        assert_eq!(cleanup.uncertain_authorizations, 0);
        assert!(cleanup.operator_recovery_required);
        assert!(cleanup.revocation_fenced);
        clock.advance(TimeDelta::seconds(1));
        let restored = GoogleOAuthService::new(
            repository.clone(),
            transport.clone(),
            SecretCipher::new(
                Arc::new(BTreeMap::from([
                    (1, CredentialKey::from_test_bytes([7; 32])),
                    (2, CredentialKey::from_test_bytes([8; 32])),
                ])),
                2,
            ),
            service.scope,
            clock,
            StdDuration::from_mins(10),
        );
        restored
            .accounts_with_cleanup()
            .await
            .expect("restoring the key does not guess an unknown Google grant");
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let recovery = restored
            .acknowledge_operator_recovery(true)
            .await
            .expect("operator confirms affected project grants were revoked externally");
        assert_eq!(recovery.accounts_marked_reauthorization_required, 1);
        assert_eq!(recovery.legacy_accounts_finalized, 0);
        let retained = repository
            .account()
            .await
            .expect("repository")
            .expect("account retained for reauthorization");
        assert_eq!(
            retained.account.status,
            GoogleAccountStatus::ReauthorizationRequired
        );
        let cleanup = repository.cleanup_status().await.expect("cleanup cleared");
        assert!(!cleanup.operator_recovery_required);
        assert_eq!(cleanup.uncertain_authorizations, 0);
    }

    #[tokio::test]
    async fn preflight_storage_failure_prevents_exchange() {
        let (service, repository, transport, _) = fixture([Some("never-issued")]);
        let started = service
            .begin(begin_input(), idempotency("preflight-failure"))
            .await
            .expect("begin");
        repository.fail_next_preflights(1).await;
        assert!(
            service
                .callback(
                    &FakeTransport::state_from_url(&started.authorization_url),
                    "code",
                )
                .await
                .is_err()
        );
        assert_eq!(transport.0.lock().expect("transport lock").exchanges, 0);
    }

    #[tokio::test]
    async fn ambiguous_token_endpoint_failure_survives_restart_as_operator_recovery() {
        let (service, repository, transport, clock) = fixture([]);
        let started = service
            .begin(begin_input(), idempotency("ambiguous-exchange"))
            .await
            .expect("begin");
        transport
            .0
            .lock()
            .expect("transport lock")
            .temporary_exchange_next = true;
        assert!(matches!(
            service
                .callback(
                    &FakeTransport::state_from_url(&started.authorization_url),
                    "code",
                )
                .await,
            Err(GoogleOAuthServiceError::Google(GoogleError::Temporary {
                status: 503
            }))
        ));
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert_eq!(status.uncertain_authorizations, 1);
        assert!(!status.operator_recovery_required);

        clock.advance(TimeDelta::minutes(3));
        service
            .recover_startup()
            .await
            .expect("restart turns the ambiguous exchange into a durable recovery fence");
        let status = repository.cleanup_status().await.expect("cleanup status");
        assert_eq!(status.uncertain_authorizations, 1);
        assert!(status.operator_recovery_required);
        assert!(status.revocation_fenced);
        let recovery = service
            .acknowledge_operator_recovery(true)
            .await
            .expect("operator confirms external grant revocation");
        assert_eq!(recovery.accounts_marked_reauthorization_required, 0);
        assert!(
            !repository
                .cleanup_status()
                .await
                .expect("cleanup status")
                .revocation_fenced
        );
    }

    #[tokio::test]
    async fn guardian_keeps_readiness_degraded_until_durable_hold_recovers() {
        let (service, repository, transport, clock) = fixture([Some("guarded-token")]);
        let readiness = Readiness::default();
        readiness.set_ready(true);
        let service = Arc::new(
            GoogleOAuthService::new(
                repository.clone(),
                transport.clone(),
                service.cipher.clone(),
                service.scope,
                clock,
                StdDuration::from_mins(10),
            )
            .with_readiness(readiness.clone()),
        );
        let started = service
            .begin(begin_input(), idempotency("guardian-start"))
            .await
            .expect("begin");
        repository.fail_next_holds(100).await;
        repository.fail_next_volatile_claims(100).await;
        transport
            .0
            .lock()
            .expect("transport lock")
            .revoke_failures_remaining = 100;
        assert!(matches!(
            service
                .callback(
                    &FakeTransport::state_from_url(&started.authorization_url),
                    "code",
                )
                .await,
            Err(GoogleOAuthServiceError::CredentialDurabilityPending)
        ));
        assert!(!readiness.is_ready());
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        let (_, status) = service
            .accounts_with_cleanup()
            .await
            .expect("guardian status remains observable");
        assert_eq!(status.volatile_guardians, 1);
        assert!(status.durability_degraded);
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty(),
            "guardian must not revoke without a durable fence snapshot"
        );

        repository.fail_next_holds(0).await;
        repository.fail_next_volatile_claims(0).await;
        transport
            .0
            .lock()
            .expect("transport lock")
            .revoke_failures_remaining = 0;
        tokio::time::sleep(StdDuration::from_millis(1_200)).await;
        assert!(readiness.is_ready());
        let status = repository
            .cleanup_status()
            .await
            .expect("durable hold status");
        assert_eq!(status.pending, 1);
        service
            .accounts_with_cleanup()
            .await
            .expect("durable cleanup reconciles");
        assert_eq!(
            repository
                .cleanup_status()
                .await
                .expect("cleanup complete")
                .pending,
            0
        );
    }

    #[tokio::test]
    async fn guardian_keeps_scope_fenced_while_provider_revocation_is_in_flight() {
        let (service, repository, transport, _) = fixture([Some("guardian-barrier-token")]);
        let started = service
            .begin(begin_input(), idempotency("guardian-barrier-start"))
            .await
            .expect("begin");
        repository.fail_next_holds(100).await;
        let (entered, release) = transport.pause_next_revoke();
        assert!(matches!(
            service
                .callback(
                    &FakeTransport::state_from_url(&started.authorization_url),
                    "code",
                )
                .await,
            Err(GoogleOAuthServiceError::CredentialDurabilityPending)
        ));
        tokio::time::timeout(StdDuration::from_secs(2), entered.notified())
            .await
            .expect("guardian revoke reached deterministic barrier");
        assert!(
            repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
        let mut connect_new = begin_input();
        connect_new.connect_new = true;
        assert!(matches!(
            service
                .begin(connect_new, idempotency("guardian-barrier-connect"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
        release.notify_one();
        tokio::time::timeout(StdDuration::from_secs(2), async {
            while repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("guardian clears fence after definitive provider result");
        assert_eq!(service.guardians.count(), 0);
    }

    #[tokio::test]
    async fn stale_guardian_requires_operator_recovery_when_token_identity_is_unknown() {
        let (service, repository, transport, clock) = fixture([Some("shared-after-stale")]);
        let stale_start = service
            .begin(begin_input(), idempotency("stale-guardian-source"))
            .await
            .expect("stale begin");
        let stale_state = FakeTransport::state_from_url(&stale_start.authorization_url);
        let CallbackClaim::Exchange(stale) = repository
            .claim_callback(
                hash_secret(&stale_state),
                clock.now(),
                clock.now() - EXCHANGE_LEASE,
            )
            .await
            .expect("claim stale session")
        else {
            panic!("stale session exchanges");
        };
        repository
            .fail_authorization(stale.id, clock.now())
            .await
            .expect("stale session fails");

        let current_start = service
            .begin(begin_input(), idempotency("stale-guardian-current"))
            .await
            .expect("new begin");
        service
            .callback(
                &FakeTransport::state_from_url(&current_start.authorization_url),
                "current-code",
            )
            .await
            .expect("new token installs");

        let claim = repository
            .claim_volatile_revocation(
                stale.id,
                Uuid::new_v4(),
                clock.now(),
                clock.now() - GUARDIAN_LEASE,
            )
            .await
            .expect("guardian takes a fresh scope fence");
        assert_eq!(claim.protected_accounts.len(), 1);
        assert!(matches!(
            service
                .begin(begin_input(), idempotency("stale-guardian-blocked"))
                .await,
            Err(GoogleOAuthServiceError::Repository(
                GoogleOAuthRepositoryError::RevocationInProgress
            ))
        ));
        clock.advance(TimeDelta::minutes(3));
        service
            .recover_startup()
            .await
            .expect("startup recovery converts ambiguous custody to an operator fence");
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .is_empty()
        );
        let status = repository.cleanup_status().await.expect("status");
        assert!(status.operator_recovery_required);
        assert!(status.revocation_fenced);
        let recovery = service
            .acknowledge_operator_recovery(true)
            .await
            .expect("operator confirms the project grant was externally revoked");
        assert_eq!(recovery.accounts_marked_reauthorization_required, 1);
        let account = repository
            .account()
            .await
            .expect("repository")
            .expect("account remains for reauthorization");
        assert_eq!(
            account.account.status,
            GoogleAccountStatus::ReauthorizationRequired
        );
        assert!(
            !repository
                .cleanup_status()
                .await
                .expect("status")
                .revocation_fenced
        );
    }

    #[tokio::test]
    async fn seal_failure_keeps_plaintext_guarded_until_definitive_revocation() {
        let (service, repository, transport, clock) = fixture([]);
        let started = service
            .begin(begin_input(), idempotency("seal-guardian-source"))
            .await
            .expect("begin");
        let returned_state = FakeTransport::state_from_url(&started.authorization_url);
        let CallbackClaim::Exchange(claimed) = repository
            .claim_callback(
                hash_secret(&returned_state),
                clock.now(),
                clock.now() - EXCHANGE_LEASE,
            )
            .await
            .expect("claim callback")
        else {
            panic!("new session exchanges");
        };
        let readiness = Readiness::default();
        readiness.set_ready(true);
        let broken = GoogleOAuthService::new(
            repository,
            transport.clone(),
            SecretCipher::new(Arc::new(BTreeMap::new()), 1),
            service.scope,
            clock.clone(),
            StdDuration::from_mins(10),
        )
        .with_readiness(readiness.clone());
        transport
            .0
            .lock()
            .expect("transport lock")
            .revoke_failures_remaining = 100;
        assert!(matches!(
            broken
                .hold_new_refresh_token(
                    claimed.id,
                    &SecretString::from("plain-guarded-token".to_owned()),
                    clock.now(),
                )
                .await,
            Err(GoogleOAuthServiceError::CredentialDurabilityPending)
        ));
        assert!(!readiness.is_ready());
        assert_eq!(broken.guardians.count(), 1);
        transport
            .0
            .lock()
            .expect("transport lock")
            .revoke_failures_remaining = 0;
        tokio::time::sleep(StdDuration::from_millis(1_200)).await;
        assert!(readiness.is_ready());
        assert_eq!(broken.guardians.count(), 0);
        assert!(
            transport
                .0
                .lock()
                .expect("transport lock")
                .revoked_tokens
                .iter()
                .all(|token| token == "plain-guarded-token")
        );
    }
}
