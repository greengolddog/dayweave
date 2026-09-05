use std::{
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    account_deletion::{
        AccountDeletionFenceConfirmation, AccountDeletionFenceSafetyEvidence,
        AccountDeletionPreparation, AccountDeletionPreparationSafetyEvidence,
        AccountDeletionPrincipalKey, AccountDeletionPrincipalPseudonym, AccountDeletionRepository,
        AccountDeletionRepositoryError, AccountDeletionSafetyGate, AccountDeletionSafetyGateError,
        AccountDeletionStatus, AccountDeletionTransition, account_deletion_approval_digest,
    },
    credential_auth::{
        AccountRecoveryCodeSpec, CredentialKind, CredentialRepository,
        DEVICE_CLIENT_CONTRACT_VERSION, DeviceClientKind, DeviceEnrollmentSpec, OpaqueCredential,
        full_owner_device_scopes,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresAccountDeletionRepository, PostgresCredentialRepository,
    },
    readiness::Readiness,
};
use sqlx::{
    Acquire, AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[derive(Debug, Default)]
struct VerifiedTestSafetyGate {
    reservations: AtomicUsize,
    promotions: AtomicUsize,
}

#[async_trait]
impl AccountDeletionSafetyGate for VerifiedTestSafetyGate {
    async fn authorize_preparation(
        &self,
        _principal: AccountDeletionPrincipalPseudonym,
        _deletion_id: Uuid,
    ) -> Result<AccountDeletionPreparationSafetyEvidence, AccountDeletionSafetyGateError> {
        self.reservations.fetch_add(1, Ordering::SeqCst);
        Ok(AccountDeletionPreparationSafetyEvidence {
            principal_rate_limit_hash: [0x52; 32],
        })
    }

    async fn commit_tombstone(
        &self,
        _principal: AccountDeletionPrincipalPseudonym,
        _deletion_id: Uuid,
    ) -> Result<AccountDeletionFenceSafetyEvidence, AccountDeletionSafetyGateError> {
        self.promotions.fetch_add(1, Ordering::SeqCst);
        Ok(AccountDeletionFenceSafetyEvidence {
            external_tombstone_hash: [0x53; 32],
        })
    }
}

#[tokio::test]
async fn catalog_inventory_covers_every_tenant_table_and_user_reference() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");

    assert_account_deletion_catalog_coverage(&test_database.pool).await;

    test_database.destroy().await;
}

#[tokio::test]
async fn fence_guards_identity_edges_and_google_outbound_workspace_links() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let fenced_scope = seed_scope(pool, "identity-fenced").await;
    let unrelated_scope = seed_scope(pool, "identity-unrelated").await;
    let fenced_item = seed_item(pool, fenced_scope, "Fenced item").await;
    let unrelated_item = seed_item(pool, unrelated_scope, "Unrelated item").await;
    let fenced_google = seed_google_outbound_pair(pool, fenced_scope, fenced_item, 0x81).await;
    let unrelated_google =
        seed_google_outbound_pair(pool, unrelated_scope, unrelated_item, 0x91).await;

    let mut cross_outbox = pool.begin().await.expect("cross-outbox transaction");
    sqlx::query(
        "UPDATE google_sync_outbox SET approval_id = $2, intent_hash = $3, \
         collection_revision = 1, target_remote_collection_id = 'cross-workspace', \
         required_scope = 'calendar.write', updated_at = clock_timestamp() \
         WHERE workspace_id = $1 AND id = $4",
    )
    .bind(unrelated_scope.workspace_id)
    .bind(fenced_google.preview_id)
    .bind([0xa1_u8; 32].as_slice())
    .bind(unrelated_google.outbox_id)
    .execute(&mut *cross_outbox)
    .await
    .expect("deferred cross-workspace outbox update");
    let cross_outbox_error = cross_outbox
        .commit()
        .await
        .expect_err("outbox approval cannot cross a workspace boundary");
    assert_eq!(postgres_code(&cross_outbox_error).as_deref(), Some("23503"));

    let mut cross_preview = pool.begin().await.expect("cross-preview transaction");
    sqlx::query(
        "UPDATE google_outbound_previews SET consumed_at = clock_timestamp(), outbox_id = $2, \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND id = $3",
    )
    .bind(fenced_scope.workspace_id)
    .bind(unrelated_google.outbox_id)
    .bind(fenced_google.preview_id)
    .execute(&mut *cross_preview)
    .await
    .expect("deferred cross-workspace preview update");
    let cross_preview_error = cross_preview
        .commit()
        .await
        .expect_err("preview outbox cannot cross a workspace boundary");
    assert_eq!(
        postgres_code(&cross_preview_error).as_deref(),
        Some("23503")
    );

    install_test_fence(pool, fenced_scope).await;

    let membership_error = sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'member')",
    )
    .bind(unrelated_scope.workspace_id)
    .bind(fenced_scope.user_id)
    .execute(pool)
    .await
    .expect_err("a fenced user cannot join an unrelated workspace");
    assert_eq!(postgres_code(&membership_error).as_deref(), Some("DWDEL"));

    let owner_error = sqlx::query("UPDATE workspaces SET owner_user_id = $2 WHERE id = $1")
        .bind(unrelated_scope.workspace_id)
        .bind(fenced_scope.user_id)
        .execute(pool)
        .await
        .expect_err("a fenced user cannot become an unrelated workspace owner");
    assert_eq!(postgres_code(&owner_error).as_deref(), Some("DWDEL"));

    let old_identity_error =
        sqlx::query("UPDATE items SET workspace_id = $2, created_by_user_id = $3 WHERE id = $1")
            .bind(fenced_item)
            .bind(unrelated_scope.workspace_id)
            .bind(unrelated_scope.user_id)
            .execute(pool)
            .await
            .expect_err("the OLD fenced workspace and user remain guarded during reassignment");
    assert_eq!(postgres_code(&old_identity_error).as_deref(), Some("DWDEL"));

    let new_identity_error =
        sqlx::query("UPDATE items SET workspace_id = $2, created_by_user_id = $3 WHERE id = $1")
            .bind(unrelated_item)
            .bind(fenced_scope.workspace_id)
            .bind(fenced_scope.user_id)
            .execute(pool)
            .await
            .expect_err("the NEW fenced workspace and user are guarded during reassignment");
    assert_eq!(postgres_code(&new_identity_error).as_deref(), Some("DWDEL"));

    test_database.destroy().await;
}

#[tokio::test]
async fn fence_insert_serializes_after_an_earlier_guarded_mutation() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "ordering").await;
    let item_id = seed_item(pool, scope, "Before fence").await;
    let fence = prepare_test_fence(pool, scope).await;

    let mut mutation_connection = pool.acquire().await.expect("mutation connection");
    let mut mutation = mutation_connection
        .begin()
        .await
        .expect("guarded mutation transaction");
    sqlx::query(
        "UPDATE items SET title = 'Committed before fence', updated_at = clock_timestamp() \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .execute(&mut *mutation)
    .await
    .expect("pre-fence mutation acquires the shared advisory guard");

    let mut fence_connection = pool.acquire().await.expect("fence connection");
    let fence_backend_pid = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(&mut *fence_connection)
        .await
        .expect("fence backend pid");
    let fence_insert = tokio::spawn(async move {
        sqlx::query(
            "INSERT INTO account_deletion_fences (deletion_id, workspace_id, user_id, \
             owner_subject_hash, lifecycle_revision, fenced_at) VALUES ($1, $2, $3, $4, 2, $5)",
        )
        .bind(fence.deletion_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(fence.owner_subject_hash)
        .bind(fence.fenced_at)
        .execute(&mut *fence_connection)
        .await
    });

    wait_until_backend_waits_on_advisory_lock(pool, fence_backend_pid).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_deletion_fences WHERE deletion_id = $1",
        )
        .bind(fence.deletion_id)
        .fetch_one(pool)
        .await
        .expect("fence visibility while blocked"),
        0
    );

    mutation
        .commit()
        .await
        .expect("earlier guarded mutation commits first");
    drop(mutation_connection);
    tokio::time::timeout(StdDuration::from_secs(3), fence_insert)
        .await
        .expect("fence insert resumes after the mutation")
        .expect("fence task remains alive")
        .expect("fence insert commits");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM items WHERE id = $1")
            .bind(item_id)
            .fetch_one(pool)
            .await
            .expect("committed pre-fence mutation"),
        "Committed before fence"
    );

    let later_mutation_error =
        sqlx::query("UPDATE items SET title = 'Too late' WHERE workspace_id = $1 AND id = $2")
            .bind(scope.workspace_id)
            .bind(item_id)
            .execute(pool)
            .await
            .expect_err("a mutation starting after the fence must be rejected");
    assert_eq!(
        postgres_code(&later_mutation_error).as_deref(),
        Some("DWDEL")
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One destructive test owns and verifies the complete fenced scope.
async fn lifecycle_is_exact_fenced_and_purges_every_current_tenant_table_atomically() {
    let Some(test_database) = TestDatabase::from_environment().await else {
        return;
    };
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool, "owner").await;
    let unrelated_scope = seed_scope(pool, "unrelated").await;
    let now = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("current timestamp");
    let credential_repository = PostgresCredentialRepository::new(pool.clone(), scope);
    let authority = issue_deletion_authority(&credential_repository, now).await;

    sqlx::query(
        "INSERT INTO habit_operation_receipts (workspace_id, namespace, key_hash, \
         operation_id, request_fingerprint, response_json, completed_at) \
         VALUES ($1, 'account-deletion-fixture', $2, $3, $4, $5, $6)",
    )
    .bind(scope.workspace_id)
    .bind([0x31_u8; 32].as_slice())
    .bind(Uuid::new_v4())
    .bind([0x32_u8; 32].as_slice())
    .bind(serde_json::json!({"private_fixture": "must be purged"}))
    .bind(now)
    .execute(pool)
    .await
    .expect("immutable content fixture");
    let immutable_error =
        sqlx::query("DELETE FROM habit_operation_receipts WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .execute(pool)
            .await
            .expect_err("ordinary deletion cannot bypass immutable evidence");
    assert_ne!(postgres_code(&immutable_error).as_deref(), Some("DWDEL"));

    let deletion_id = Uuid::new_v4();
    let preparation = AccountDeletionPreparation {
        id: deletion_id,
        request_hash: [0x01; 32],
        explicit_approval_digest: account_deletion_approval_digest(
            deletion_id,
            scope.workspace_id,
            scope.user_id,
        ),
        authorizing_session_id: authority.session_id,
        authorizing_session_revision: authority.session_revision,
        authorizing_recovery_code_id: authority.recovery_code_id,
        authorizing_recovery_code_revision: authority.recovery_code_revision,
    };
    let recovery_code = OpaqueCredential::parse(
        CredentialKind::AccountRecovery,
        &authority.recovery_code_raw,
    )
    .unwrap();
    let disabled = PostgresAccountDeletionRepository::new(pool.clone(), scope);
    assert_eq!(
        disabled.prepare(preparation.clone(), &recovery_code).await,
        Err(AccountDeletionRepositoryError::Disabled),
        "the production constructor must remain disabled without both external gates"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM account_deletion_lifecycles")
            .fetch_one(pool)
            .await
            .unwrap(),
        0
    );

    let mismatched_gate = Arc::new(VerifiedTestSafetyGate::default());
    let mismatched_repository = PostgresAccountDeletionRepository::new(pool.clone(), scope)
        .with_safety_gate(
            mismatched_gate.clone(),
            AccountDeletionPrincipalKey::new(1, [0x51; 32])
                .unwrap()
                .bind("a-different-owner")
                .unwrap(),
        );
    assert_eq!(
        mismatched_repository
            .prepare(preparation.clone(), &recovery_code)
            .await,
        Err(AccountDeletionRepositoryError::InvalidAuthority),
        "an external pseudonym cannot be paired with a different local owner"
    );
    assert_eq!(mismatched_gate.reservations.load(Ordering::SeqCst), 0);

    let safety_gate = Arc::new(VerifiedTestSafetyGate::default());
    let external_principal = AccountDeletionPrincipalKey::new(1, [0x51; 32])
        .unwrap()
        .bind("account-deletion-owner")
        .unwrap();
    let repository = PostgresAccountDeletionRepository::new(pool.clone(), scope)
        .with_safety_gate(safety_gate.clone(), external_principal);
    let mut unauthorized_preparation = preparation.clone();
    unauthorized_preparation.id = Uuid::new_v4();
    unauthorized_preparation.request_hash = [0x7f; 32];
    unauthorized_preparation.explicit_approval_digest = account_deletion_approval_digest(
        unauthorized_preparation.id,
        scope.workspace_id,
        scope.user_id,
    );
    unauthorized_preparation.authorizing_session_id = Uuid::new_v4();
    assert_eq!(
        repository
            .prepare(unauthorized_preparation, &recovery_code)
            .await,
        Err(AccountDeletionRepositoryError::InvalidAuthority)
    );
    let wrong_recovery_raw = token(CredentialKind::AccountRecovery, 0x79);
    let wrong_recovery =
        OpaqueCredential::parse(CredentialKind::AccountRecovery, &wrong_recovery_raw).unwrap();
    assert_eq!(
        repository
            .prepare(preparation.clone(), &wrong_recovery)
            .await,
        Err(AccountDeletionRepositoryError::InvalidAuthority),
        "the current recovery credential must be manually supplied"
    );
    sqlx::query(
        "UPDATE account_recovery_codes SET created_at = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(authority.recovery_code_id)
    .bind(now - Duration::hours(23))
    .execute(pool)
    .await
    .expect("make recovery proof too young");
    assert_eq!(
        repository
            .prepare(preparation.clone(), &recovery_code)
            .await,
        Err(AccountDeletionRepositoryError::InvalidAuthority),
        "a recovery credential younger than 24 hours cannot authorize deletion"
    );
    sqlx::query(
        "UPDATE account_recovery_codes SET created_at = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(authority.recovery_code_id)
    .bind(authority.recovery_code_created_at)
    .execute(pool)
    .await
    .expect("restore old recovery proof");
    assert_eq!(
        safety_gate.reservations.load(Ordering::SeqCst),
        0,
        "unauthorized input must not consume a limiter or external reservation"
    );
    let mut cancelled_preparation = preparation.clone();
    cancelled_preparation.id = Uuid::new_v4();
    cancelled_preparation.request_hash = [0x09; 32];
    cancelled_preparation.explicit_approval_digest = account_deletion_approval_digest(
        cancelled_preparation.id,
        scope.workspace_id,
        scope.user_id,
    );
    repository
        .prepare(cancelled_preparation.clone(), &recovery_code)
        .await
        .expect("cancellable external reservation");
    let cancellation = transition(
        cancelled_preparation.id,
        1,
        0x0a,
        AccountDeletionStatus::Prepared,
        AccountDeletionStatus::Cancelled,
    );
    assert!(
        !repository
            .advance(cancellation.clone())
            .await
            .unwrap()
            .replayed
    );
    assert!(repository.advance(cancellation).await.unwrap().replayed);
    let prepared = repository
        .prepare(preparation.clone(), &recovery_code)
        .await
        .expect("explicitly approved preparation");
    assert_eq!(prepared.status, AccountDeletionStatus::Prepared);
    assert_eq!(prepared.revision, 1);
    assert!(!prepared.replayed);
    let persisted_external_principal = sqlx::query_as::<_, (i32, Vec<u8>)>(
        "SELECT external_principal_key_version, external_principal_pseudonym \
         FROM account_deletion_lifecycles WHERE id = $1",
    )
    .bind(deletion_id)
    .fetch_one(pool)
    .await
    .expect("persisted external principal binding");
    assert_eq!(persisted_external_principal.0, 1);
    assert_eq!(
        persisted_external_principal.1,
        external_principal.pseudonym().digest()
    );
    let principal_mutation_error = sqlx::query(
        "UPDATE account_deletion_lifecycles SET external_principal_key_version = 2 \
         WHERE id = $1",
    )
    .bind(deletion_id)
    .execute(pool)
    .await
    .expect_err("external principal binding is immutable");
    assert_eq!(
        postgres_code(&principal_mutation_error).as_deref(),
        Some("DWCON")
    );
    assert_eq!(safety_gate.reservations.load(Ordering::SeqCst), 2);
    let replayed_preparation = repository
        .prepare(preparation.clone(), &recovery_code)
        .await
        .expect("lost preparation response replays");
    assert!(replayed_preparation.replayed);
    assert_eq!(replayed_preparation.revision, 1);
    let drift_gate = Arc::new(VerifiedTestSafetyGate::default());
    let drifted_repository = PostgresAccountDeletionRepository::new(pool.clone(), scope)
        .with_safety_gate(
            drift_gate.clone(),
            AccountDeletionPrincipalKey::new(2, [0x52; 32])
                .unwrap()
                .bind("account-deletion-owner")
                .unwrap(),
        );
    assert_eq!(
        drifted_repository
            .prepare(preparation.clone(), &recovery_code)
            .await,
        Err(AccountDeletionRepositoryError::Conflict),
        "key drift cannot redirect an existing lifecycle to a new external identity"
    );
    assert_eq!(drift_gate.reservations.load(Ordering::SeqCst), 0);
    let mut changed_preparation = preparation.clone();
    changed_preparation.request_hash = [0x03; 32];
    assert_eq!(
        repository
            .prepare(changed_preparation, &recovery_code)
            .await,
        Err(AccountDeletionRepositoryError::Conflict)
    );

    let fence = transition(
        deletion_id,
        1,
        0x11,
        AccountDeletionStatus::Prepared,
        AccountDeletionStatus::FenceCommitting,
    );
    let early_confirmation = AccountDeletionFenceConfirmation {
        transition: fence.clone(),
        confirming_session_id: authority.session_id,
        confirming_session_revision: authority.session_revision,
        explicit_approval_digest: account_deletion_approval_digest(
            deletion_id,
            scope.workspace_id,
            scope.user_id,
        ),
    };
    assert_eq!(
        repository.begin_fence(early_confirmation).await,
        Err(AccountDeletionRepositoryError::CooldownPending),
        "the server clock enforces the 24-hour prepare-to-fence cooldown"
    );
    sqlx::query("ALTER TABLE account_deletion_lifecycles DISABLE TRIGGER USER")
        .execute(pool)
        .await
        .expect("age test-only preparation");
    let aged_prepared_at = now - Duration::hours(25);
    sqlx::query(
        "UPDATE account_deletion_lifecycles SET prepared_at = $2, created_at = $2, \
         updated_at = $2, authorizing_credential_issued_at = $2 WHERE id = $1",
    )
    .bind(deletion_id)
    .bind(aged_prepared_at)
    .execute(pool)
    .await
    .expect("age prepared lifecycle fixture");
    sqlx::query("ALTER TABLE account_deletion_lifecycles ENABLE TRIGGER USER")
        .execute(pool)
        .await
        .expect("restore lifecycle guard");
    let confirming_session = issue_confirmation_session(&credential_repository, now).await;
    let confirmation = AccountDeletionFenceConfirmation {
        transition: fence.clone(),
        confirming_session_id: confirming_session.id,
        confirming_session_revision: confirming_session.revision,
        explicit_approval_digest: account_deletion_approval_digest(
            deletion_id,
            scope.workspace_id,
            scope.user_id,
        ),
    };
    assert_eq!(
        PostgresAccountDeletionRepository::new(pool.clone(), scope)
            .begin_fence(confirmation.clone())
            .await,
        Err(AccountDeletionRepositoryError::Disabled),
        "removing external configuration disables a prepared lifecycle"
    );
    let fenced = repository
        .begin_fence(confirmation.clone())
        .await
        .expect("hard fence commits atomically");
    assert_eq!(fenced.revision, 2);
    assert!(!fenced.replayed);
    assert!(
        repository
            .begin_fence(confirmation.clone())
            .await
            .expect("lost fence response replays")
            .replayed
    );
    let mut conflicting_confirmation = confirmation;
    conflicting_confirmation.confirming_session_revision += 1;
    assert_eq!(
        repository.begin_fence(conflicting_confirmation).await,
        Err(AccountDeletionRepositoryError::Conflict)
    );

    assert!(
        credential_repository
            .is_account_deletion_fenced()
            .await
            .expect("fence query")
    );
    let access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &authority.access_raw).unwrap();
    assert!(
        credential_repository
            .authenticate_device_access(&access, now + Duration::seconds(3))
            .await
            .is_err(),
        "the pre-fence access credential cannot authenticate"
    );
    let readiness = Readiness::with_database(pool.clone(), scope.workspace_id, scope.user_id);
    readiness.set_ready(true);
    assert!(!readiness.check().await, "a fenced process cannot be ready");

    let mutation_error = sqlx::query(
        "UPDATE habit_operation_receipts SET completed_at = completed_at \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .execute(pool)
    .await
    .expect_err("hard fence blocks every tenant mutation");
    assert_eq!(postgres_code(&mutation_error).as_deref(), Some("DWDEL"));
    let bootstrap_error = sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, 'account-deletion-owner', 'Resurrected', 'UTC')",
    )
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .expect_err("subject fence blocks bootstrap under a changed configured UUID");
    assert_eq!(postgres_code(&bootstrap_error).as_deref(), Some("DWDEL"));

    let fenced_transition = transition(
        deletion_id,
        2,
        0x12,
        AccountDeletionStatus::FenceCommitting,
        AccountDeletionStatus::Fenced,
    );
    assert_eq!(
        PostgresAccountDeletionRepository::new(pool.clone(), scope)
            .advance(fenced_transition.clone())
            .await,
        Err(AccountDeletionRepositoryError::Disabled),
        "removing external configuration disables tombstone promotion"
    );
    assert!(
        !repository
            .advance(fenced_transition.clone())
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(safety_gate.promotions.load(Ordering::SeqCst), 1);
    assert!(
        repository
            .advance(fenced_transition)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(
        safety_gate.promotions.load(Ordering::SeqCst),
        2,
        "an exact retry reasserts the idempotent external tombstone without database locks"
    );
    let cleanup_transition = transition(
        deletion_id,
        3,
        0x13,
        AccountDeletionStatus::Fenced,
        AccountDeletionStatus::ProviderCleanup,
    );
    repository.advance(cleanup_transition).await.unwrap();
    let purge_transition = transition(
        deletion_id,
        4,
        0x14,
        AccountDeletionStatus::ProviderCleanup,
        AccountDeletionStatus::Purge,
    );
    assert_eq!(
        repository.advance(purge_transition).await,
        Err(AccountDeletionRepositoryError::InvalidInput),
        "provider cleanup cannot reach purge until durable provider outcomes exist"
    );
    sqlx::query(
        "WITH operation AS (SELECT clock_timestamp() AS at) \
         UPDATE account_deletion_lifecycles SET status = 'purge', revision = revision + 1, \
         purge_at = operation.at, updated_at = operation.at FROM operation \
         WHERE id = $1 AND status = 'provider_cleanup' AND revision = 4",
    )
    .bind(deletion_id)
    .execute(pool)
    .await
    .expect("test-only provider evidence boundary fixture");
    let local_purge = transition(
        deletion_id,
        5,
        0x15,
        AccountDeletionStatus::Purge,
        AccountDeletionStatus::BackupWait,
    );
    let purged: (i64, bool) = sqlx::query_as(
        "SELECT result_revision, replayed FROM purge_fenced_personal_account_scope($1, $2, $3)",
    )
    .bind(local_purge.deletion_id)
    .bind(i64::try_from(local_purge.expected_revision).unwrap())
    .bind(local_purge.request_hash.as_slice())
    .fetch_one(pool)
    .await
    .expect("local purge foundation commits");
    assert_eq!(purged, (6, false));
    let replayed: (i64, bool) = sqlx::query_as(
        "SELECT result_revision, replayed FROM purge_fenced_personal_account_scope($1, $2, $3)",
    )
    .bind(local_purge.deletion_id)
    .bind(i64::try_from(local_purge.expected_revision).unwrap())
    .bind(local_purge.request_hash.as_slice())
    .fetch_one(pool)
    .await
    .expect("lost local purge response replays from detached evidence");
    assert_eq!(replayed, (6, true));

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE id = $1")
            .bind(scope.user_id)
            .fetch_one(pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces WHERE id = $1")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE id = $1")
            .bind(unrelated_scope.user_id)
            .fetch_one(pool)
            .await
            .unwrap(),
        1,
        "purging one owner cannot remove an unrelated user"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspaces WHERE id = $1")
            .bind(unrelated_scope.workspace_id)
            .fetch_one(pool)
            .await
            .unwrap(),
        1,
        "purging one owner cannot remove an unrelated workspace"
    );
    assert_all_current_tenant_tables_empty_and_guarded(pool, scope.workspace_id).await;

    let lifecycle = repository
        .lifecycle(deletion_id)
        .await
        .expect("detached lifecycle lookup")
        .expect("lifecycle survives purge");
    assert_eq!(lifecycle.status, AccountDeletionStatus::BackupWait);
    assert!(lifecycle.local_purge_completed_at.is_some());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_deletion_fences WHERE deletion_id = $1",
        )
        .bind(deletion_id)
        .fetch_one(pool)
        .await
        .unwrap(),
        1,
        "the anti-resurrection fence survives local content purge"
    );
    assert_detached_evidence_is_content_free(pool).await;

    test_database.destroy().await;
}

async fn assert_account_deletion_catalog_coverage(pool: &PgPool) {
    assert_account_deletion_barrier_modes(pool).await;

    let external_binding_guarded = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_trigger AS trigger \
         JOIN pg_class AS relation ON relation.oid = trigger.tgrelid \
         JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
         JOIN pg_proc AS function ON function.oid = trigger.tgfoid \
         WHERE namespace.nspname = current_schema() \
         AND relation.relname = 'account_deletion_lifecycles' \
         AND trigger.tgname = 'account_deletion_external_principal_binding_guard' \
         AND function.proname = 'guard_account_deletion_external_principal_binding' \
         AND trigger.tgtype = 23 AND NOT trigger.tgisinternal)",
    )
    .fetch_one(pool)
    .await
    .expect("external principal binding guard inventory");
    assert!(external_binding_guarded);
    let public_can_execute_binding_guard = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_proc AS function \
         CROSS JOIN LATERAL aclexplode(COALESCE(function.proacl, \
             acldefault('f', function.proowner))) AS acl \
         JOIN pg_namespace AS namespace ON namespace.oid = function.pronamespace \
         WHERE namespace.nspname = current_schema() \
         AND function.proname = 'guard_account_deletion_external_principal_binding' \
         AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE')",
    )
    .fetch_one(pool)
    .await
    .expect("external principal guard privilege inventory");
    assert!(!public_can_execute_binding_guard);

    let tables = sqlx::query_scalar::<_, String>(
        "SELECT table_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND column_name = 'workspace_id' \
         AND table_name NOT IN ('account_deletion_lifecycles', 'account_deletion_fences') \
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await
    .expect("tenant table inventory");
    assert_eq!(
        tables.len(),
        66,
        "migration tenant-table inventory must be consciously updated"
    );
    for table in &tables {
        let guarded = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_trigger AS trigger \
             JOIN pg_class AS relation ON relation.oid = trigger.tgrelid \
             JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_proc AS function ON function.oid = trigger.tgfoid \
             WHERE namespace.nspname = current_schema() AND relation.relname = $1 \
             AND trigger.tgname = 'account_deletion_fence_guard' \
             AND function.proname = 'reject_fenced_workspace_mutation' \
             AND trigger.tgtype = 31 AND NOT trigger.tgisinternal)",
        )
        .bind(table)
        .fetch_one(pool)
        .await
        .expect("fence trigger catalog inventory");
        assert!(guarded, "{table} lacks the complete hard deletion guard");
    }

    assert_user_reference_guards(pool).await;
}

async fn assert_account_deletion_barrier_modes(pool: &PgPool) {
    let workspace_guard_definition: String = sqlx::query_scalar(
        "SELECT pg_get_functiondef(function.oid) FROM pg_proc AS function \
         JOIN pg_namespace AS namespace ON namespace.oid = function.pronamespace \
         WHERE namespace.nspname = current_schema() \
         AND function.proname = 'reject_fenced_workspace_mutation'",
    )
    .fetch_one(pool)
    .await
    .expect("workspace fence guard definition");
    assert!(
        workspace_guard_definition.contains("dayweave.account-deletion.global-mutation-barrier.v1")
            && workspace_guard_definition.contains("pg_advisory_xact_lock_shared"),
        "ordinary mutations must enter the global deletion barrier in shared mode"
    );
    let fence_guard_definition: String = sqlx::query_scalar(
        "SELECT pg_get_functiondef(function.oid) FROM pg_proc AS function \
         JOIN pg_namespace AS namespace ON namespace.oid = function.pronamespace \
         WHERE namespace.nspname = current_schema() \
         AND function.proname = 'validate_account_deletion_fence'",
    )
    .fetch_one(pool)
    .await
    .expect("fence installation guard definition");
    assert!(
        fence_guard_definition.contains("dayweave.account-deletion.global-mutation-barrier.v1")
            && !fence_guard_definition.contains("pg_advisory_xact_lock_shared"),
        "fence installation must own the global deletion barrier exclusively"
    );
}

async fn assert_user_reference_guards(pool: &PgPool) {
    let user_references = sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT source.relname::text, source_attribute.attname::text \
         FROM pg_constraint AS foreign_key \
         JOIN pg_class AS source ON source.oid = foreign_key.conrelid \
         JOIN pg_namespace AS source_namespace ON source_namespace.oid = source.relnamespace \
         JOIN pg_class AS target ON target.oid = foreign_key.confrelid \
         JOIN pg_namespace AS target_namespace ON target_namespace.oid = target.relnamespace \
         JOIN LATERAL unnest(foreign_key.conkey) WITH ORDINALITY \
              AS source_key(attnum, ordinal) ON true \
         JOIN LATERAL unnest(foreign_key.confkey) WITH ORDINALITY \
              AS target_key(attnum, ordinal) ON target_key.ordinal = source_key.ordinal \
         JOIN pg_attribute AS source_attribute \
           ON source_attribute.attrelid = source.oid \
          AND source_attribute.attnum = source_key.attnum \
         JOIN pg_attribute AS target_attribute \
           ON target_attribute.attrelid = target.oid \
          AND target_attribute.attnum = target_key.attnum \
         WHERE foreign_key.contype = 'f' \
           AND source_namespace.nspname = current_schema() \
           AND target_namespace.nspname = current_schema() \
           AND ((target.relname = 'users' AND target_attribute.attname = 'id') \
             OR (target.relname = 'workspace_members' \
                 AND target_attribute.attname = 'user_id')) \
         ORDER BY source.relname::text, source_attribute.attname::text",
    )
    .fetch_all(pool)
    .await
    .expect("user-reference catalog inventory");
    assert!(
        !user_references.is_empty(),
        "user-reference inventory is empty"
    );
    for (table, column) in &user_references {
        assert!(
            column == "user_id" || column.ends_with("_user_id"),
            "{table}.{column} is a user reference the generic deletion guard cannot recognize"
        );
        let guarded = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_trigger AS trigger \
             JOIN pg_class AS relation ON relation.oid = trigger.tgrelid \
             JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = current_schema() AND relation.relname = $1 \
             AND trigger.tgname = 'account_deletion_fence_guard' \
             AND trigger.tgtype = 31 AND NOT trigger.tgisinternal)",
        )
        .bind(table)
        .fetch_one(pool)
        .await
        .expect("user-reference guard inventory");
        assert!(
            guarded,
            "{table}.{column} is not covered by a deletion guard"
        );
    }
}

async fn assert_all_current_tenant_tables_empty_and_guarded(pool: &PgPool, workspace_id: Uuid) {
    assert_account_deletion_catalog_coverage(pool).await;
    let cyclic_fks_are_deferred = sqlx::query_scalar::<_, bool>(
        "SELECT bool_and(condeferrable AND condeferred) FROM pg_constraint \
         WHERE connamespace = current_schema()::regnamespace \
         AND conname IN ('google_sync_outbox_approval_fk', \
             'google_outbound_previews_outbox_fk')",
    )
    .fetch_one(pool)
    .await
    .expect("cyclic Google FK policy");
    assert!(cyclic_fks_are_deferred);
    let public_can_execute_purge = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_proc AS p \
         CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) AS acl \
         JOIN pg_namespace AS n ON n.oid = p.pronamespace \
         WHERE n.nspname = current_schema() \
         AND p.proname = 'purge_fenced_personal_account_scope' \
         AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE')",
    )
    .fetch_one(pool)
    .await
    .expect("purge privilege policy");
    assert!(
        !public_can_execute_purge,
        "the trigger-bypassing purge primitive cannot be executable by PUBLIC"
    );
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT table_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND column_name = 'workspace_id' \
         AND table_name NOT IN ('account_deletion_lifecycles', 'account_deletion_fences') \
         ORDER BY table_name",
    )
    .fetch_all(pool)
    .await
    .expect("tenant table inventory");
    let purge_definition: String = sqlx::query_scalar(
        "SELECT pg_get_functiondef(p.oid) FROM pg_proc AS p \
         JOIN pg_namespace AS n ON n.oid = p.pronamespace \
         WHERE n.nspname = current_schema() \
         AND p.proname = 'purge_fenced_personal_account_scope'",
    )
    .fetch_one(pool)
    .await
    .expect("purge function definition");
    let delete_inventory = purge_definition
        .split("delete_order constant text[]")
        .nth(1)
        .and_then(|tail| tail.split("BEGIN").next())
        .expect("explicit purge order");
    for table in tables {
        let guarded = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_trigger AS trigger \
             JOIN pg_class AS relation ON relation.oid = trigger.tgrelid \
             JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = current_schema() AND relation.relname = $1 \
             AND trigger.tgname = 'account_deletion_fence_guard' \
             AND NOT trigger.tgisinternal)",
        )
        .bind(&table)
        .fetch_one(pool)
        .await
        .expect("fence trigger inventory");
        assert!(guarded, "{table} lacks the hard account-deletion fence");
        assert!(
            delete_inventory.contains(&format!("'{table}'")),
            "{table} is absent from the explicit purge order"
        );
        let count_sql = format!("SELECT count(*) FROM {table} WHERE workspace_id = $1");
        let count = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql))
            .bind(workspace_id)
            .fetch_one(pool)
            .await
            .expect("scoped post-purge count");
        assert_eq!(count, 0, "{table} retained tenant rows");
    }
}

async fn assert_detached_evidence_is_content_free(pool: &PgPool) {
    let risky_columns = sqlx::query_scalar::<_, String>(
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() \
         AND table_name IN ('account_deletion_lifecycles', \
             'account_deletion_transition_receipts', 'account_deletion_fences') \
         AND (data_type IN ('json', 'jsonb', 'text') OR column_name IN (\
             'title', 'name', 'display_name', 'auth_subject', 'payload', 'metadata', \
             'notes', 'token', 'credential')) ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .expect("detached evidence column inventory");
    assert!(
        risky_columns.is_empty(),
        "content-bearing evidence: {risky_columns:?}"
    );

    let bounded_string_columns = sqlx::query_scalar::<_, String>(
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() \
         AND table_name IN ('account_deletion_lifecycles', \
             'account_deletion_transition_receipts', 'account_deletion_fences') \
         AND data_type = 'character varying' ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .expect("bounded evidence string inventory");
    assert_eq!(
        bounded_string_columns,
        vec![
            "account_deletion_lifecycles.failure_code",
            "account_deletion_lifecycles.status",
            "account_deletion_transition_receipts.failure_code",
            "account_deletion_transition_receipts.from_status",
            "account_deletion_transition_receipts.to_status",
        ],
        "only fixed-policy lifecycle labels may use bounded strings"
    );
}

fn transition(
    deletion_id: Uuid,
    expected_revision: u64,
    marker: u8,
    from: AccountDeletionStatus,
    to: AccountDeletionStatus,
) -> AccountDeletionTransition {
    AccountDeletionTransition {
        deletion_id,
        request_hash: [marker; 32],
        expected_revision,
        from,
        to,
        failure_code: None,
    }
}

struct TestFence {
    deletion_id: Uuid,
    owner_subject_hash: Vec<u8>,
    fenced_at: DateTime<Utc>,
}

async fn prepare_test_fence(pool: &PgPool, scope: DatabaseScope) -> TestFence {
    let deletion_id = Uuid::new_v4();
    let owner_subject_hash = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT sha256(convert_to(auth_subject, 'UTF8')) FROM users WHERE id = $1",
    )
    .bind(scope.user_id)
    .fetch_one(pool)
    .await
    .expect("owner subject hash");
    let prepared_at = DateTime::<Utc>::from_timestamp_micros(
        (Utc::now() - Duration::hours(25)).timestamp_micros(),
    )
    .expect("prepared timestamp");
    sqlx::query(
        "INSERT INTO account_deletion_lifecycles (id, workspace_id, user_id, \
         owner_subject_hash, prepare_request_hash, explicit_approval_digest, \
         principal_rate_limit_evidence_hash, external_principal_key_version, \
         external_principal_pseudonym, authorizing_session_id, \
         authorizing_session_revision, authorizing_credential_issued_at, \
         authorizing_recovery_code_id, authorizing_recovery_code_revision, \
         authorizing_recovery_code_created_at, prepared_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $9, 1, $10, $11, 1, $12, \
         $13, $13, $13)",
    )
    .bind(deletion_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(&owner_subject_hash)
    .bind([0xb1_u8; 32].as_slice())
    .bind([0xb2_u8; 32].as_slice())
    .bind([0xb3_u8; 32].as_slice())
    .bind([0xb5_u8; 32].as_slice())
    .bind(Uuid::new_v4())
    .bind(prepared_at - Duration::hours(1))
    .bind(Uuid::new_v4())
    .bind(prepared_at - Duration::hours(25))
    .bind(prepared_at)
    .execute(pool)
    .await
    .expect("prepared deletion lifecycle fixture");
    let fenced_at = DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("fence timestamp");
    sqlx::query(
        "UPDATE account_deletion_lifecycles SET status = 'fence_committing', revision = 2, \
         confirming_session_id = $2, confirming_session_revision = 1, \
         confirming_credential_issued_at = $3, confirming_approval_digest = $4, \
         confirmed_at = $3, fence_committing_at = $3, updated_at = $3 WHERE id = $1",
    )
    .bind(deletion_id)
    .bind(Uuid::new_v4())
    .bind(fenced_at)
    .bind([0xb4_u8; 32].as_slice())
    .execute(pool)
    .await
    .expect("fence-committing lifecycle fixture");
    TestFence {
        deletion_id,
        owner_subject_hash,
        fenced_at,
    }
}

async fn install_test_fence(pool: &PgPool, scope: DatabaseScope) {
    let fence = prepare_test_fence(pool, scope).await;
    sqlx::query(
        "INSERT INTO account_deletion_fences (deletion_id, workspace_id, user_id, \
         owner_subject_hash, lifecycle_revision, fenced_at) VALUES ($1, $2, $3, $4, 2, $5)",
    )
    .bind(fence.deletion_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(fence.owner_subject_hash)
    .bind(fence.fenced_at)
    .execute(pool)
    .await
    .expect("account deletion fence fixture");
}

async fn wait_until_backend_waits_on_advisory_lock(pool: &PgPool, backend_pid: i32) {
    tokio::time::timeout(StdDuration::from_secs(3), async {
        loop {
            let waiting = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid = $1 \
                 AND wait_event_type = 'Lock' AND wait_event = 'advisory')",
            )
            .bind(backend_pid)
            .fetch_one(pool)
            .await
            .expect("fence wait-state inspection");
            if waiting {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("fence insert must block on the earlier mutation's advisory guard");
}

async fn seed_item(pool: &PgPool, scope: DatabaseScope, title: &str) -> Uuid {
    let item_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds, scheduling_constraints) \
         VALUES ($1, $2, $3, 'task', 'planned', $4, 'UTC', 600, '{}'::jsonb)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(title)
    .execute(pool)
    .await
    .expect("item fixture");
    item_id
}

struct GoogleOutboundPair {
    preview_id: Uuid,
    outbox_id: Uuid,
}

async fn seed_google_outbound_pair(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    marker: u8,
) -> GoogleOutboundPair {
    let provider_account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, $5, $6, 1, \
         'active', true, false)",
    )
    .bind(provider_account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("deletion-provider-{provider_account_id}"))
    .bind(format!("Deletion provider {marker}"))
    .bind(vec![marker; 64])
    .execute(pool)
    .await
    .expect("provider account fixture");
    let collection_id = Uuid::new_v4();
    let remote_collection_id = format!("deletion-calendar-{collection_id}");
    sqlx::query(
        "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
         collection_kind, remote_collection_id, display_name, provider_access_role, \
         provider_selected, selected, visible, sync_role, discovered_at, configured_at, \
         created_at, updated_at) VALUES ($1, $2, $3, $4, 'calendar', $5, $6, 'owner', \
         true, true, true, 'blocking', clock_timestamp(), clock_timestamp(), \
         clock_timestamp(), clock_timestamp())",
    )
    .bind(collection_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(&remote_collection_id)
    .bind(format!("Deletion calendar {marker}"))
    .execute(pool)
    .await
    .expect("Google collection fixture");
    let preview_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_outbound_previews (id, workspace_id, user_id, provider_account_id, \
         collection_id, collection_revision, collection_remote_id, item_id, item_revision, \
         entity_kind, operation, required_scope, intent_hash, preview_hash, payload, expires_at, \
         approved_at, capability_hash, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 1, \
         $6, $7, 1, 'calendar_event', 'upsert', 'calendar.write', $8, $9, '{}'::jsonb, \
         clock_timestamp() + interval '1 hour', clock_timestamp(), $10, clock_timestamp(), \
         clock_timestamp())",
    )
    .bind(preview_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(collection_id)
    .bind(&remote_collection_id)
    .bind(item_id)
    .bind([marker; 32].as_slice())
    .bind([marker.wrapping_add(1); 32].as_slice())
    .bind([marker.wrapping_add(2); 32].as_slice())
    .execute(pool)
    .await
    .expect("Google outbound preview fixture");
    let outbox_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_sync_outbox (id, workspace_id, user_id, provider_account_id, \
         collection_id, item_id, item_revision, entity_kind, operation, app_owned, payload, \
         state, available_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 1, \
         'calendar_event', 'upsert', true, '{}'::jsonb, 'pending', clock_timestamp(), \
         clock_timestamp(), clock_timestamp())",
    )
    .bind(outbox_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(collection_id)
    .bind(item_id)
    .execute(pool)
    .await
    .expect("Google sync outbox fixture");
    GoogleOutboundPair {
        preview_id,
        outbox_id,
    }
}

struct DeletionAuthority {
    session_id: Uuid,
    session_revision: u64,
    access_raw: String,
    recovery_code_id: Uuid,
    recovery_code_revision: u64,
    recovery_code_created_at: DateTime<Utc>,
    recovery_code_raw: String,
}

async fn issue_deletion_authority(
    repository: &PostgresCredentialRepository,
    now: DateTime<Utc>,
) -> DeletionAuthority {
    let original_issued_at = now - Duration::hours(50);
    let enrollment_raw = token(CredentialKind::Enrollment, 0x71);
    let original_access_raw = token(CredentialKind::DeviceAccess, 0x72);
    let original_refresh_raw = token(CredentialKind::DeviceRefresh, 0x73);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let original_access =
        OpaqueCredential::parse(CredentialKind::DeviceAccess, &original_access_raw).unwrap();
    let original_refresh =
        OpaqueCredential::parse(CredentialKind::DeviceRefresh, &original_refresh_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Deletion test Mac".to_owned(),
                scopes: full_owner_device_scopes(),
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "account-deletion-test-1".to_owned(),
                client_capabilities: vec!["explicit-account-deletion".to_owned()],
                created_at: original_issued_at,
            },
            &enrollment,
        )
        .await
        .expect("full-owner enrollment");
    let session = repository
        .consume_device_enrollment(
            &enrollment,
            Uuid::new_v4(),
            &original_access,
            &original_refresh,
            original_issued_at,
        )
        .await
        .expect("full-owner session")
        .value;
    let recovery_code_id = Uuid::new_v4();
    let recovery_code_created_at = original_issued_at + Duration::seconds(1);
    let recovery_code_raw = token(CredentialKind::AccountRecovery, 0x74);
    let recovery_code =
        OpaqueCredential::parse(CredentialKind::AccountRecovery, &recovery_code_raw).unwrap();
    let recovery = repository
        .create_or_rotate_account_recovery_code(
            AccountRecoveryCodeSpec {
                id: recovery_code_id,
                replaces_recovery_code_id: None,
                replaces_recovery_code_revision: None,
                created_at: recovery_code_created_at,
            },
            &recovery_code,
            session.id,
        )
        .await
        .expect("old acknowledged recovery credential");
    let access_raw = token(CredentialKind::DeviceAccess, 0x75);
    let refresh_raw = token(CredentialKind::DeviceRefresh, 0x76);
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    let refreshed = repository
        .refresh_device_session(&original_refresh, &access, &refresh, now)
        .await
        .expect("fresh deletion-authorizing credentials")
        .value;
    DeletionAuthority {
        session_id: refreshed.id,
        session_revision: refreshed.revision,
        access_raw,
        recovery_code_id: recovery.id,
        recovery_code_revision: recovery.revision,
        recovery_code_created_at,
        recovery_code_raw,
    }
}

async fn issue_confirmation_session(
    repository: &PostgresCredentialRepository,
    now: DateTime<Utc>,
) -> dayweave_api::credential_auth::DeviceSession {
    let enrollment_raw = token(CredentialKind::Enrollment, 0x77);
    let access_raw = token(CredentialKind::DeviceAccess, 0x78);
    let refresh_raw = token(CredentialKind::DeviceRefresh, 0x79);
    let enrollment = OpaqueCredential::parse(CredentialKind::Enrollment, &enrollment_raw).unwrap();
    let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
    let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Deletion confirmation Mac".to_owned(),
                scopes: full_owner_device_scopes(),
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "account-deletion-confirmation-1".to_owned(),
                client_capabilities: vec!["explicit-account-deletion".to_owned()],
                created_at: now,
            },
            &enrollment,
        )
        .await
        .expect("confirmation enrollment");
    repository
        .consume_device_enrollment(&enrollment, Uuid::new_v4(), &access, &refresh, now)
        .await
        .expect("fresh confirmation session")
        .value
}

fn token(kind: CredentialKind, marker: u8) -> String {
    format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode([marker; 32]))
}

fn postgres_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
}

async fn seed_scope(pool: &PgPool, marker: &str) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, $3, 'UTC')",
    )
    .bind(scope.user_id)
    .bind(format!("account-deletion-{marker}"))
    .bind(format!("Deletion {marker}"))
    .execute(pool)
    .await
    .expect("test user");
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, $4, 'UTC')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("account-deletion-{marker}"))
    .bind(format!("Deletion {marker}"))
    .execute(pool)
    .await
    .expect("test workspace");
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("test owner membership");
    scope
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn from_environment() -> Option<Self> {
        let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; account deletion test skipped");
            return None;
        };
        let options = PgConnectOptions::from_str(&database_url)
            .expect("valid DAYWEAVE_TEST_DATABASE_URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        let schema = format!("dayweave_account_deletion_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated account deletion schema");
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |connection, _| {
                let statement = format!("SET search_path TO {connection_schema}");
                Box::pin(async move {
                    connection.execute(AssertSqlSafe(statement)).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .expect("connect isolated account deletion pool");
        Some(Self {
            admin,
            pool,
            schema,
        })
    }

    async fn destroy(self) {
        self.pool.close().await;
        self.admin
            .execute(AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .await
            .expect("drop isolated account deletion schema");
        self.admin.close().await;
    }
}
