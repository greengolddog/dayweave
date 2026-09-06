use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use dayweave_google::{
    GoogleError,
    oauth::{AuthorizationOptions, OAuthTokenSet},
};
use secrecy::SecretString;
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

use super::{
    AccountDeletionProviderPreparationError, AccountDeletionProviderPreparationResult,
    AccountDeletionProviderPreparationService,
};
use crate::{
    account_deletion::{
        AccountDeletionFenceConfirmation, AccountDeletionFenceSafetyEvidence,
        AccountDeletionPreparationSafetyEvidence, AccountDeletionPrincipalKey,
        AccountDeletionPrincipalPseudonym, AccountDeletionRepository,
        AccountDeletionRepositoryError, AccountDeletionSafetyGate, AccountDeletionSafetyGateError,
        AccountDeletionStatus, AccountDeletionTransition, account_deletion_approval_digest,
    },
    config::CredentialKey,
    credential_auth::{
        AccountRecoveryCodeSpec, CredentialKind, CredentialRepository,
        DEVICE_CLIENT_CONTRACT_VERSION, DeviceClientKind, DeviceEnrollmentSpec, DeviceSession,
        OpaqueCredential, full_owner_device_scopes,
    },
    google_oauth::{
        AuthorizationMaterial, GoogleIdentity, GoogleOAuthRepository, GoogleOAuthService,
        GoogleOAuthTransport, NewOAuthSession, OAuthIdempotency, OAuthScope, SealedSecret,
        SecretCipher,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresAccountDeletionRepository, PostgresCredentialRepository,
        PostgresGoogleOAuthRepository,
    },
    proposals::SystemClock,
    provider_admission::{ProviderAdmission, ProviderAdmissionError},
};

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // Real adapters and the exact shared gate complete one pre-fence workflow.
async fn postgres_preparation_cancels_pending_authorization_then_drains_and_fences() {
    let database = TestDatabase::create().await;
    let pool = &database.pool;
    let (scope, confirmation, principal) = seed_confirmed_preparation(pool).await;
    let oauth_scope = OAuthScope {
        workspace_id: scope.workspace_id,
        user_id: scope.user_id,
    };
    let admission = ProviderAdmission::postgres(pool.clone(), oauth_scope);
    let repository = Arc::new(
        PostgresAccountDeletionRepository::new(pool.clone(), scope)
            .with_safety_gate(Arc::new(VerifiedSafetyGate), principal)
            .with_provider_admission(admission.clone()),
    );
    let oauth_repository = Arc::new(PostgresGoogleOAuthRepository::new(pool.clone(), scope));
    let transport = Arc::new(NoProviderCalls::default());
    let mismatched_oauth = build_oauth(
        oauth_repository.clone(),
        transport.clone(),
        oauth_scope,
        ProviderAdmission::postgres(pool.clone(), oauth_scope),
    );
    assert!(
        matches!(
            AccountDeletionProviderPreparationService::new(repository.clone(), mismatched_oauth),
            Err(AccountDeletionProviderPreparationError::Repository(
                AccountDeletionRepositoryError::Disabled
            ))
        ),
        "an independently constructed matching scope is not the configured OAuth runtime"
    );
    let oauth = build_oauth(
        oauth_repository.clone(),
        transport.clone(),
        oauth_scope,
        admission.clone(),
    );
    let service =
        AccountDeletionProviderPreparationService::new(repository.clone(), oauth).unwrap();
    let session_id = seed_pending_authorization(&oauth_repository).await;
    let mut unauthorized = confirmation.clone();
    unauthorized.confirming_session_id = Uuid::new_v4();
    assert!(matches!(
        service.prepare(&unauthorized).await,
        Err(AccountDeletionProviderPreparationError::Repository(
            AccountDeletionRepositoryError::InvalidAuthority
        ))
    ));
    assert!(
        matches!(
            admission.run(service.prepare(&confirmation)).await,
            Ok(Err(
                AccountDeletionProviderPreparationError::AdmissionUnavailable
            ))
        ),
        "reentrant preparation cannot mutate pending work or wait for itself to drain"
    );
    let untouched: bool = sqlx::query_scalar(
        "SELECT status = 'pending' AND encrypted_pkce_verifier IS NOT NULL \
        AND encrypted_authorization_url IS NOT NULL FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        untouched,
        "authorization is validated before pending cancellation or recovery"
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(admission.run(async {}).await.is_ok());
    let mut cancelled = 0;
    let proof = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match service
                .prepare(&confirmation)
                .await
                .expect("bounded authorized preparation")
            {
                AccountDeletionProviderPreparationResult::Drained {
                    proof,
                    cancelled_authorizations,
                } => {
                    cancelled += cancelled_authorizations;
                    break proof;
                }
                AccountDeletionProviderPreparationResult::Waiting {
                    readiness,
                    cancelled_authorizations,
                } => {
                    cancelled += cancelled_authorizations;
                    assert!(!readiness.admission_closed);
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .expect("explicit recovery-operation settlement permits the next bounded attempt");
    assert_eq!(cancelled, 1);
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        0,
        "pending cancellation needs no Google calls"
    );
    let scrubbed: bool = sqlx::query_scalar(
        "SELECT status = 'failed' AND encrypted_pkce_verifier IS NULL \
        AND verifier_key_version IS NULL AND encrypted_authorization_url IS NULL \
        AND authorization_url_key_version IS NULL FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(scrubbed);
    assert_eq!(
        admission.run(async {}).await,
        Err(ProviderAdmissionError::Closed)
    );
    assert!(matches!(
        service.prepare(&confirmation).await.unwrap(),
        AccountDeletionProviderPreparationResult::Drained {
            cancelled_authorizations: 0,
            ..
        }
    ));
    let fenced = repository
        .begin_fence(confirmation, &proof)
        .await
        .expect("prepared service proof authorizes existing hard fence");
    assert_eq!(fenced.status, AccountDeletionStatus::FenceCommitting);
    assert_eq!(fenced.revision, 2);
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn postgres_preparation_reports_already_closed_pending_work_without_reopening() {
    let database = TestDatabase::create().await;
    let pool = &database.pool;
    let (scope, confirmation, principal) = seed_confirmed_preparation(pool).await;
    let oauth_scope = OAuthScope {
        workspace_id: scope.workspace_id,
        user_id: scope.user_id,
    };
    let admission = ProviderAdmission::postgres(pool.clone(), oauth_scope);
    let repository = Arc::new(
        PostgresAccountDeletionRepository::new(pool.clone(), scope)
            .with_safety_gate(Arc::new(VerifiedSafetyGate), principal)
            .with_provider_admission(admission.clone()),
    );
    let oauth_repository = Arc::new(PostgresGoogleOAuthRepository::new(pool.clone(), scope));
    let session_id = seed_pending_authorization(&oauth_repository).await;
    let transport = Arc::new(NoProviderCalls::default());
    let oauth = build_oauth(
        oauth_repository,
        transport.clone(),
        oauth_scope,
        admission.clone(),
    );
    let service = AccountDeletionProviderPreparationService::new(repository, oauth).unwrap();
    let _premature_proof = admission
        .close_and_drain(confirmation.transition.deletion_id)
        .await
        .unwrap();
    let result = service.prepare(&confirmation).await.unwrap();
    let AccountDeletionProviderPreparationResult::Waiting {
        readiness,
        cancelled_authorizations,
    } = result
    else {
        panic!("closed admission with unfinished authorization is not ready for fencing");
    };
    assert!(readiness.admission_closed);
    assert!(!readiness.is_ready());
    assert_eq!(readiness.pending_authorizations, 1);
    assert_eq!(cancelled_authorizations, 0);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        admission.run(async {}).await,
        Err(ProviderAdmissionError::Closed)
    );
    let retained: bool = sqlx::query_scalar(
        "SELECT status = 'pending' AND encrypted_pkce_verifier IS NOT NULL \
        AND encrypted_authorization_url IS NOT NULL FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(
        retained,
        "the closed legacy/interrupted state is reported without silently cancelling custody"
    );
    database.destroy().await;
}

async fn seed_pending_authorization(repository: &PostgresGoogleOAuthRepository) -> Uuid {
    let now = Utc::now();
    let session_id = Uuid::new_v4();
    let pending = NewOAuthSession {
        id: session_id,
        owner_subject_hash: [0x51; 32],
        state_hash: [0x52; 32],
        encrypted_verifier: SealedSecret {
            key_version: 1,
            ciphertext: vec![0x53; 64],
        },
        encrypted_authorization_url: SealedSecret {
            key_version: 1,
            ciphertext: vec![0x54; 96],
        },
        requested_scopes: ["openid".to_owned()].into_iter().collect(),
        expected_account_id: None,
        expected_account_revision: None,
        make_default: false,
        created_at: now,
        expires_at: now + TimeDelta::minutes(10),
    };
    let idempotency = OAuthIdempotency {
        namespace: "google_oauth_start",
        key_hash: [0x55; 32],
        request_fingerprint: [0x56; 32],
        expires_at: now + TimeDelta::days(1),
    };
    repository
        .create_session(pending, idempotency, now - TimeDelta::minutes(2))
        .await
        .unwrap();
    session_id
}

fn build_oauth(
    repository: Arc<PostgresGoogleOAuthRepository>,
    transport: Arc<NoProviderCalls>,
    scope: OAuthScope,
    admission: ProviderAdmission,
) -> Arc<GoogleOAuthService> {
    let cipher = SecretCipher::new(
        Arc::new(BTreeMap::from([(
            1,
            CredentialKey::from_test_bytes([7; 32]),
        )])),
        1,
    );
    Arc::new(
        GoogleOAuthService::new_with_admission(
            repository,
            transport,
            cipher,
            scope,
            Arc::new(SystemClock),
            Duration::from_mins(10),
            admission,
        )
        .unwrap(),
    )
}

#[derive(Default)]
struct NoProviderCalls {
    calls: AtomicUsize,
}

#[async_trait]
impl GoogleOAuthTransport for NoProviderCalls {
    fn begin(&self, _options: &AuthorizationOptions) -> Result<AuthorizationMaterial, GoogleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GoogleError::Unauthorized)
    }
    async fn exchange(
        &self,
        _state: &SecretString,
        _verifier: &SecretString,
        _code: &SecretString,
    ) -> Result<OAuthTokenSet, GoogleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GoogleError::Unauthorized)
    }
    async fn refresh(&self, _refresh_token: &SecretString) -> Result<OAuthTokenSet, GoogleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GoogleError::Unauthorized)
    }
    async fn identity(&self, _access_token: &SecretString) -> Result<GoogleIdentity, GoogleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GoogleError::Unauthorized)
    }
    async fn revoke(&self, _token: &SecretString) -> Result<(), GoogleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GoogleError::Unauthorized)
    }
}

struct VerifiedSafetyGate;

#[async_trait]
impl AccountDeletionSafetyGate for VerifiedSafetyGate {
    async fn authorize_preparation(
        &self,
        _principal: AccountDeletionPrincipalPseudonym,
        _deletion_id: Uuid,
    ) -> Result<AccountDeletionPreparationSafetyEvidence, AccountDeletionSafetyGateError> {
        Ok(AccountDeletionPreparationSafetyEvidence {
            principal_rate_limit_hash: [0x41; 32],
        })
    }
    async fn commit_tombstone(
        &self,
        _principal: AccountDeletionPrincipalPseudonym,
        _deletion_id: Uuid,
    ) -> Result<AccountDeletionFenceSafetyEvidence, AccountDeletionSafetyGateError> {
        Ok(AccountDeletionFenceSafetyEvidence {
            external_tombstone_hash: [0x42; 32],
        })
    }
}

async fn seed_confirmed_preparation(
    pool: &PgPool,
) -> (
    DatabaseScope,
    AccountDeletionFenceConfirmation,
    crate::account_deletion::AccountDeletionPrincipalBinding,
) {
    let scope = DatabaseScope {
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
    };
    let subject = "provider-preparation-owner";
    sqlx::query("INSERT INTO users (id, auth_subject, display_name, timezone_name) VALUES ($1, $2, 'Fixture owner', 'UTC')")
        .bind(scope.user_id).bind(subject).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) VALUES ($1, $2, 'preparation', 'Fixture', 'UTC')")
        .bind(scope.workspace_id).bind(scope.user_id).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
    let credentials = PostgresCredentialRepository::new(pool.clone(), scope);
    let now = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let old_issued_at = now - TimeDelta::hours(50);
    let recovery_issuer = issue_session(&credentials, old_issued_at, 1).await;
    let recovery_raw = token(CredentialKind::AccountRecovery, 9);
    let recovery = OpaqueCredential::parse(CredentialKind::AccountRecovery, &recovery_raw).unwrap();
    let recovery_at = old_issued_at + TimeDelta::seconds(1);
    let recovery = credentials
        .create_or_rotate_account_recovery_code(
            AccountRecoveryCodeSpec {
                id: Uuid::new_v4(),
                replaces_recovery_code_id: None,
                replaces_recovery_code_revision: None,
                created_at: recovery_at,
            },
            &recovery,
            recovery_issuer.id,
        )
        .await
        .unwrap();
    let prepared_at = now - TimeDelta::hours(25);
    let authorizing_issued_at = prepared_at - TimeDelta::minutes(1);
    let authorizing = issue_session(&credentials, authorizing_issued_at, 7).await;
    let confirming = issue_session(&credentials, now, 4).await;
    let principal = AccountDeletionPrincipalKey::new(1, [0x43; 32])
        .unwrap()
        .bind(subject)
        .unwrap();
    let deletion_id = Uuid::new_v4();
    let approval = account_deletion_approval_digest(deletion_id, scope.workspace_id, scope.user_id);
    sqlx::query("INSERT INTO account_deletion_lifecycles (id, workspace_id, user_id, owner_subject_hash, prepare_request_hash, \
        explicit_approval_digest, principal_rate_limit_evidence_hash, external_principal_key_version, external_principal_pseudonym, \
        authorizing_session_id, authorizing_session_revision, authorizing_credential_issued_at, authorizing_recovery_code_id, \
        authorizing_recovery_code_revision, authorizing_recovery_code_created_at, prepared_at, created_at, updated_at) \
        VALUES ($1, $2, $3, sha256(convert_to($4, 'UTF8')), $5, $6, $7, 1, $8, $9, $10, $11, $12, $13, $14, $15, $15, $15)")
        .bind(deletion_id).bind(scope.workspace_id).bind(scope.user_id).bind(subject).bind([0x44_u8; 32].as_slice())
        .bind(approval.as_slice()).bind([0x41_u8; 32].as_slice()).bind(principal.pseudonym().digest().as_slice())
        .bind(authorizing.id).bind(i64::try_from(authorizing.revision).unwrap()).bind(authorizing_issued_at).bind(recovery.id)
        .bind(i64::try_from(recovery.revision).unwrap()).bind(recovery_at).bind(prepared_at).execute(pool).await.unwrap();
    let confirmation = AccountDeletionFenceConfirmation {
        transition: AccountDeletionTransition {
            deletion_id,
            expected_revision: 1,
            request_hash: [0x45; 32],
            from: AccountDeletionStatus::Prepared,
            to: AccountDeletionStatus::FenceCommitting,
            failure_code: None,
        },
        confirming_session_id: confirming.id,
        confirming_session_revision: confirming.revision,
        explicit_approval_digest: approval,
    };
    (scope, confirmation, principal)
}

async fn issue_session(
    repository: &PostgresCredentialRepository,
    now: DateTime<Utc>,
    marker: u8,
) -> DeviceSession {
    let enrollment_raw = token(CredentialKind::Enrollment, marker);
    let access_raw = token(CredentialKind::DeviceAccess, marker + 1);
    let refresh_raw = token(CredentialKind::DeviceRefresh, marker + 2);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Preparation fixture".to_owned(),
                scopes: full_owner_device_scopes(),
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "fixture".to_owned(),
                client_capabilities: vec!["explicit-account-deletion".to_owned()],
                created_at: now,
            },
            &enrollment,
        )
        .await
        .unwrap();
    repository
        .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
        .await
        .unwrap()
        .value
}

fn token(kind: CredentialKind, marker: u8) -> String {
    format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode([marker; 32]))
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn create() -> Self {
        let url = std::env::var("DAYWEAVE_TEST_DATABASE_URL").expect("test database URL");
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .unwrap();
        let schema = format!("dayweave_preparation_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .unwrap();
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .after_connect(move |connection, _| {
                let sql = format!("SET search_path TO {connection_schema}");
                Box::pin(async move {
                    connection.execute(AssertSqlSafe(sql)).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }
    async fn destroy(self) {
        self.pool.close().await;
        self.admin
            .execute(AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .await
            .unwrap();
        self.admin.close().await;
    }
}
