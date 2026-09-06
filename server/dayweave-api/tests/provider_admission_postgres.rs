use std::{
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{Duration as ChronoDuration, Utc};
use dayweave_api::{
    google_oauth::OAuthScope,
    persistence::MIGRATOR,
    provider_admission::{ProviderAdmission, ProviderAdmissionError},
};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::sync::oneshot;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn independent_runtimes_share_closure_and_wait_for_explicit_settlement() {
    let database = TestDatabase::create().await;
    let (scope, deletion_id) = seed_deletion(&database.pool_a).await;
    let runtime_a = ProviderAdmission::postgres(database.pool_a.clone(), scope);
    let runtime_b = ProviderAdmission::postgres(database.pool_b.clone(), scope);
    let (entered, running) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let active = runtime_a.clone();
    let work = tokio::spawn(async move {
        active
            .run(async move {
                entered.send(()).expect("provider body entered");
                released.await.expect("provider response released");
            })
            .await
    });
    running
        .await
        .expect("registration commits before body begins");
    assert_eq!(operation_count(&database.pool_b, scope).await, 1);
    let closing = runtime_b.clone();
    let mut drain = tokio::spawn(async move { closing.close_and_drain(deletion_id).await });
    wait_for_durable_closure(&database.pool_a, scope, deletion_id).await;
    assert_eq!(
        runtime_a.run(async {}).await,
        Err(ProviderAdmissionError::Closed),
        "another runtime's local-open controller still observes the durable closure"
    );
    assert_eq!(
        runtime_b.run(async {}).await,
        Err(ProviderAdmissionError::Closed)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut drain)
            .await
            .is_err()
    );
    release.send(()).expect("release admitted body");
    work.await
        .expect("provider task")
        .expect("normal admitted completion");
    let _drained = tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("durable settlement permits drain")
        .expect("drain task")
        .expect("drained registry");
    assert_eq!(operation_count(&database.pool_a, scope).await, 0);
    let restarted = ProviderAdmission::postgres(database.pool_a.clone(), scope);
    assert_eq!(
        restarted.run(async {}).await,
        Err(ProviderAdmissionError::Closed),
        "a new runtime cannot reopen persisted admission"
    );
    restarted
        .close_and_drain(deletion_id)
        .await
        .expect("same deletion closure replays");
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn closure_waits_for_operations_sharing_either_workspace_or_user() {
    let database = TestDatabase::create().await;
    for retain_workspace in [true, false] {
        let (old_scope, old_deletion) = seed_deletion(&database.pool_a).await;
        let old_runtime = ProviderAdmission::postgres(database.pool_a.clone(), old_scope);
        let active = old_runtime.clone();
        let (entered, running) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let work = tokio::spawn(async move {
            active
                .run(async move {
                    entered.send(()).expect("old scope operation registered");
                    released.await.expect("release old scope provider work");
                })
                .await
        });
        running.await.expect("old scope provider body entered");
        let scope = replace_deletion_scope_fixture(
            &database.pool_a,
            old_scope,
            old_deletion,
            retain_workspace,
        )
        .await;
        let deletion_id = seed_prepared_deletion(&database.pool_a, scope).await;
        let fenced_at = if retain_workspace {
            Some(advance_to_fence_fixture(&database.pool_a, deletion_id).await)
        } else {
            None
        };
        let runtime = ProviderAdmission::postgres(database.pool_b.clone(), scope);
        let close = runtime.clone();
        let mut drain = tokio::spawn(async move { close.close_and_drain(deletion_id).await });
        wait_for_durable_closure(&database.pool_b, scope, deletion_id).await;
        assert_eq!(operation_count(&database.pool_b, scope).await, 0);
        assert_eq!(operation_count(&database.pool_b, old_scope).await, 1);
        if let Some(fenced_at) = fenced_at {
            let error = insert_fence(&database.pool_a, scope, deletion_id, fenced_at)
                .await
                .expect_err("direct SQL cannot fence over a previous owner's unfinished operation");
            assert_eq!(database_code(&error).as_deref(), Some("DWCON"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut drain)
                .await
                .is_err(),
            "an exact-scope empty registry must not hide workspace/user-overlapping work"
        );
        let polled = AtomicBool::new(false);
        assert_eq!(
            old_runtime
                .run(async {
                    polled.store(true, Ordering::SeqCst);
                })
                .await,
            Err(ProviderAdmissionError::Closed),
            "closure rejects overlapping registration even on an older runtime"
        );
        assert!(!polled.load(Ordering::SeqCst));
        assert_eq!(
            runtime.run(async {}).await,
            Err(ProviderAdmissionError::Closed)
        );
        release
            .send(())
            .expect("release operation from previous scope");
        work.await
            .expect("old scope provider task")
            .expect("old scope explicitly settles");
        let _drained = tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .expect("overlapping operation settlement permits drain")
            .expect("drain task")
            .expect("account-wide drained proof");
        assert_eq!(operation_count(&database.pool_b, old_scope).await, 0);
        if let Some(fenced_at) = fenced_at {
            insert_fence(&database.pool_a, scope, deletion_id, fenced_at)
                .await
                .expect("old-owner settlement makes the same direct fence valid");
        }
    }
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn lifecycle_nonkey_writer_does_not_deadlock_closure_foreign_key() {
    let database = TestDatabase::create().await;
    let (scope, deletion_id) = seed_deletion(&database.pool_a).await;
    let mut lifecycle_writer = database.pool_a.begin().await.expect("lifecycle writer");
    sqlx::query("SELECT id FROM account_deletion_lifecycles WHERE id = $1 FOR NO KEY UPDATE")
        .bind(deletion_id)
        .execute(&mut *lifecycle_writer)
        .await
        .expect("lock immutable lifecycle keys compatibly");
    let runtime = ProviderAdmission::postgres(database.pool_b.clone(), scope);
    // The closure UPDATE takes the global shared barrier before its lifecycle
    // foreign key takes KEY SHARE. That implicit lock must remain compatible
    // with the fence repository's earlier NO KEY UPDATE lock.
    let _drained =
        tokio::time::timeout(Duration::from_secs(2), runtime.close_and_drain(deletion_id))
            .await
            .expect("closure commits while lifecycle writer is live")
            .expect("empty registry drains");
    tokio::time::timeout(Duration::from_secs(2), sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1', 0))",
    ).execute(&mut *lifecycle_writer)).await.expect("exclusive fence barrier is not trapped behind closure")
        .expect("lifecycle writer acquires exclusive barrier");
    lifecycle_writer
        .commit()
        .await
        .expect("finish lifecycle writer");
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // Both database lock queue orders establish the race contract.
async fn registration_and_closure_serialize_in_both_lock_orders() {
    let database = TestDatabase::create().await;
    for registration_first in [true, false] {
        let (scope, deletion_id) = seed_deletion(&database.pool_a).await;
        seed_open_admission_scope(&database.pool_a, scope).await;
        let runtime_a = ProviderAdmission::postgres(database.pool_a.clone(), scope);
        let runtime_b = ProviderAdmission::postgres(database.pool_b.clone(), scope);
        let mut blocker = database
            .pool_b
            .begin()
            .await
            .expect("scope admission blocker");
        sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1', 0))")
            .execute(&mut *blocker).await.expect("global-before-scope ordering");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(\
            'dayweave.provider-admission.scope.v1:' || $1::uuid::text || ':' || $2::uuid::text, 0))")
            .bind(scope.workspace_id).bind(scope.user_id).execute(&mut *blocker).await
            .expect("hold the production admission serialization lock");
        let polled = Arc::new(AtomicBool::new(false));
        let body_polled = Arc::clone(&polled);
        let (entered, running) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let registering = runtime_a.clone();
        let register = async move {
            registering
                .run(async move {
                    body_polled.store(true, Ordering::SeqCst);
                    let _ = entered.send(());
                    let _ = released.await;
                })
                .await
        };
        let close = async move { runtime_b.close_and_drain(deletion_id).await };
        let (work, mut drain) = if registration_first {
            let work = tokio::spawn(register);
            wait_for_runtime_lock(&database.admin, &database.runtime_a_name).await;
            let drain = tokio::spawn(close);
            wait_for_runtime_lock(&database.admin, &database.runtime_b_name).await;
            (work, drain)
        } else {
            let drain = tokio::spawn(close);
            wait_for_runtime_lock(&database.admin, &database.runtime_b_name).await;
            let work = tokio::spawn(register);
            wait_for_runtime_lock(&database.admin, &database.runtime_a_name).await;
            (work, drain)
        };
        assert!(!polled.load(Ordering::SeqCst));
        blocker
            .commit()
            .await
            .expect("release admission lock queue");
        if registration_first {
            running.await.expect("winning registration polls its body");
            wait_for_durable_closure(&database.pool_a, scope, deletion_id).await;
            assert_eq!(operation_count(&database.pool_a, scope).await, 1);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut drain)
                    .await
                    .is_err()
            );
            release.send(()).expect("release winning provider work");
            work.await
                .expect("registered task")
                .expect("admitted work completes");
        } else {
            assert_eq!(
                work.await.expect("losing registration task"),
                Err(ProviderAdmissionError::Closed)
            );
            assert!(
                !polled.load(Ordering::SeqCst),
                "closure wins before any provider effect"
            );
            drop(release);
        }
        let _drained = tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .expect("serialized closure drains")
            .expect("closure task")
            .expect("drained registry");
        assert_eq!(operation_count(&database.pool_a, scope).await, 0);
    }
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn cancelled_operation_survives_age_and_runtime_restart_without_reaping() {
    let database = TestDatabase::create().await;
    let (scope, deletion_id) = seed_deletion(&database.pool_a).await;
    let runtime_a = ProviderAdmission::postgres(database.pool_a.clone(), scope);
    let (entered, running) = oneshot::channel();
    let work = tokio::spawn(async move {
        runtime_a
            .run(async move {
                entered.send(()).expect("operation body entered");
                std::future::pending::<()>().await;
            })
            .await
    });
    running.await.expect("durable operation registered");
    work.abort();
    assert!(work.await.expect_err("operation cancelled").is_cancelled());
    assert_eq!(operation_count(&database.pool_b, scope).await, 1);
    age_operation_fixture(&database.pool_b, scope).await;
    let restarted = ProviderAdmission::postgres(database.pool_b.clone(), scope);
    let closing = restarted.clone();
    let mut drain = tokio::spawn(async move { closing.close_and_drain(deletion_id).await });
    wait_for_durable_closure(&database.pool_b, scope, deletion_id).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut drain)
            .await
            .is_err(),
        "an old crashed operation remains unresolved after restart"
    );
    assert_eq!(operation_count(&database.pool_b, scope).await, 1);
    assert_eq!(
        restarted.run(async {}).await,
        Err(ProviderAdmissionError::Closed)
    );
    drain.abort();
    assert!(drain.await.expect_err("stop test waiter").is_cancelled());
    assert_eq!(
        operation_count(&database.pool_b, scope).await,
        1,
        "cancelling the drain waiter does not erase durable ownership"
    );
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn database_pool_loss_cannot_turn_live_or_unsettled_work_into_drained_proof() {
    let database = TestDatabase::create().await;
    let (scope, deletion_id) = seed_deletion(&database.pool_a).await;
    let runtime_a = ProviderAdmission::postgres(database.pool_a.clone(), scope);
    let (entered, running) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let work = tokio::spawn(async move {
        runtime_a
            .run(async move {
                entered.send(()).expect("provider body entered");
                released.await.expect("release provider effect");
            })
            .await
    });
    running
        .await
        .expect("registration committed before pool loss");
    tokio::time::timeout(Duration::from_secs(2), database.pool_a.close())
        .await
        .expect("provider I/O does not retain a database pool connection");
    assert_eq!(
        operation_count(&database.pool_b, scope).await,
        1,
        "connection lifetime does not own the durable operation"
    );
    let runtime_b = ProviderAdmission::postgres(database.pool_b.clone(), scope);
    let closing = runtime_b.clone();
    let mut drain = tokio::spawn(async move { closing.close_and_drain(deletion_id).await });
    wait_for_durable_closure(&database.pool_b, scope, deletion_id).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut drain)
            .await
            .is_err()
    );
    release
        .send(())
        .expect("provider response after database loss");
    let _completion = tokio::time::timeout(Duration::from_secs(2), work)
        .await
        .expect("completed body cannot hang on its unavailable pool")
        .expect("provider task");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut drain)
            .await
            .is_err(),
        "local completion without durable settlement cannot authorize another runtime's drain"
    );
    assert_eq!(operation_count(&database.pool_b, scope).await, 1);
    drain.abort();
    assert!(drain.await.expect_err("stop test waiter").is_cancelled());
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
async fn registration_database_failure_never_polls_provider_body() {
    let database = TestDatabase::create().await;
    let (scope, _) = seed_deletion(&database.pool_a).await;
    let runtime = ProviderAdmission::postgres(database.pool_a.clone(), scope);
    database.pool_a.close().await;
    let polled = AtomicBool::new(false);
    assert!(
        runtime
            .run(async {
                polled.store(true, Ordering::SeqCst);
            })
            .await
            .is_err()
    );
    assert!(!polled.load(Ordering::SeqCst));
    assert_eq!(operation_count(&database.pool_b, scope).await, 0);
    database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL"]
#[allow(clippy::too_many_lines)] // One direct writer exercises the full durable fence boundary.
async fn sql_fence_requires_exact_closed_scope_and_no_unsettled_operations() {
    let database = TestDatabase::create().await;
    let (scope, deletion_id) = seed_deletion(&database.pool_a).await;
    let fenced_at = advance_to_fence_fixture(&database.pool_a, deletion_id).await;
    let missing = insert_fence(&database.pool_a, scope, deletion_id, fenced_at)
        .await
        .expect_err("a missing admission scope cannot prove drain");
    assert_eq!(database_code(&missing).as_deref(), Some("DWCON"));
    seed_open_admission_scope(&database.pool_a, scope).await;
    let open = insert_fence(&database.pool_a, scope, deletion_id, fenced_at)
        .await
        .expect_err("an open admission scope cannot prove drain");
    assert_eq!(database_code(&open).as_deref(), Some("DWCON"));

    let runtime_a = ProviderAdmission::postgres(database.pool_a.clone(), scope);
    let runtime_b = ProviderAdmission::postgres(database.pool_b.clone(), scope);
    let active = runtime_a.clone();
    let (entered, running) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let work = tokio::spawn(async move {
        active
            .run(async move {
                entered.send(()).expect("registered provider work");
                released.await.expect("provider response released");
            })
            .await
    });
    running.await.expect("active operation before closure");
    let truncation = sqlx::query("TRUNCATE provider_admission_operations")
        .execute(&database.pool_b)
        .await
        .expect_err("truncation cannot erase unfinished operations");
    assert_eq!(database_code(&truncation).as_deref(), Some("DWOPR"));
    assert_eq!(operation_count(&database.pool_b, scope).await, 1);
    let close = runtime_b.clone();
    let drain = tokio::spawn(async move { close.close_and_drain(deletion_id).await });
    wait_for_durable_closure(&database.pool_b, scope, deletion_id).await;
    let unsettled = insert_fence(&database.pool_a, scope, deletion_id, fenced_at)
        .await
        .expect_err("persistent closure cannot hide an unfinished provider operation");
    assert_eq!(database_code(&unsettled).as_deref(), Some("DWCON"));
    let reopening = sqlx::query(
        "UPDATE provider_admission_scopes SET closed_for_deletion_id = NULL, closed_at = NULL \
        WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&database.pool_b)
    .await
    .expect_err("closure cannot reopen");
    assert_eq!(database_code(&reopening).as_deref(), Some("DWCON"));
    release.send(()).expect("settle provider work");
    work.await
        .expect("provider task")
        .expect("provider operation finishes");
    let _drained = tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("settlement completes distributed drain")
        .expect("drain task")
        .expect("drained proof");
    insert_fence(&database.pool_a, scope, deletion_id, fenced_at)
        .await
        .expect("exact persisted closure and zero operations permit the hard fence");
    let new_runtime = ProviderAdmission::postgres(database.pool_b.clone(), scope);
    assert_eq!(
        new_runtime.run(async {}).await,
        Err(ProviderAdmissionError::Closed)
    );
    let retained = sqlx::query(
        "DELETE FROM provider_admission_scopes WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&database.pool_b)
    .await
    .expect_err("closed scope evidence is retained");
    assert_eq!(database_code(&retained).as_deref(), Some("DWCON"));
    database.destroy().await;
}

async fn advance_to_fence_fixture(pool: &PgPool, deletion_id: Uuid) -> chrono::DateTime<Utc> {
    sqlx::query_scalar(
        "WITH operation AS (SELECT clock_timestamp() AS at) \
         UPDATE account_deletion_lifecycles SET status = 'fence_committing', revision = 2, \
         confirming_session_id = $2, confirming_session_revision = 1, \
         confirming_credential_issued_at = operation.at, confirming_approval_digest = $3, \
         confirmed_at = operation.at, fence_committing_at = operation.at, updated_at = operation.at \
         FROM operation WHERE id = $1 RETURNING fence_committing_at",
    ).bind(deletion_id).bind(Uuid::new_v4()).bind([0xc1_u8; 32].as_slice())
        .fetch_one(pool).await.expect("valid fence-committing lifecycle fixture")
}

async fn insert_fence(
    pool: &PgPool,
    scope: OAuthScope,
    deletion_id: Uuid,
    fenced_at: chrono::DateTime<Utc>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query("INSERT INTO account_deletion_fences (deletion_id, workspace_id, user_id, \
        owner_subject_hash, lifecycle_revision, fenced_at) \
        SELECT id, workspace_id, user_id, owner_subject_hash, 2, $4 FROM account_deletion_lifecycles \
        WHERE id = $1 AND workspace_id = $2 AND user_id = $3")
        .bind(deletion_id).bind(scope.workspace_id).bind(scope.user_id).bind(fenced_at).execute(pool).await
}

fn database_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
}

async fn operation_count(pool: &PgPool, scope: OAuthScope) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM provider_admission_operations WHERE workspace_id = $1 AND user_id = $2")
        .bind(scope.workspace_id).bind(scope.user_id).fetch_one(pool).await.expect("durable operation count")
}

async fn wait_for_durable_closure(pool: &PgPool, scope: OAuthScope, deletion_id: Uuid) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let closed: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM provider_admission_scopes \
                WHERE workspace_id = $1 AND user_id = $2 AND closed_for_deletion_id = $3)",
            )
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .bind(deletion_id)
            .fetch_one(pool)
            .await
            .expect("durable closure state");
            if closed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scope closure commits independently of outstanding operations");
}

async fn wait_for_runtime_lock(admin: &PgPool, runtime_name: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity \
                WHERE application_name = $1 AND wait_event_type = 'Lock')",
            )
            .bind(runtime_name)
            .fetch_one(admin)
            .await
            .expect("runtime database wait graph");
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runtime reaches the held admission scope lock");
}

async fn age_operation_fixture(pool: &PgPool, scope: OAuthScope) {
    // Only the isolated fixture ages immutable metadata; production has no
    // timeout/lease expiry that can erase a crashed operation.
    let mut fixture = pool.begin().await.expect("age operation fixture");
    sqlx::query("ALTER TABLE provider_admission_operations DISABLE TRIGGER provider_admission_operations_guard")
        .execute(&mut *fixture).await.expect("disable isolated operation timestamp guard");
    sqlx::query("UPDATE provider_admission_operations SET registered_at = clock_timestamp() - interval '365 days' \
        WHERE workspace_id = $1 AND user_id = $2")
        .bind(scope.workspace_id).bind(scope.user_id).execute(&mut *fixture).await.expect("old crashed operation fixture");
    sqlx::query("ALTER TABLE provider_admission_operations ENABLE TRIGGER provider_admission_operations_guard")
        .execute(&mut *fixture).await.expect("restore operation guard");
    fixture.commit().await.expect("aged fixture commits");
}

async fn seed_open_admission_scope(pool: &PgPool, scope: OAuthScope) {
    sqlx::query("INSERT INTO provider_admission_scopes (workspace_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(scope.workspace_id).bind(scope.user_id).execute(pool).await.expect("open admission scope fixture");
}

async fn seed_deletion(pool: &PgPool) -> (OAuthScope, Uuid) {
    let scope = OAuthScope {
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
    };
    let subject = format!("distributed-admission-owner-{}", scope.user_id.simple());
    sqlx::query("INSERT INTO users (id, auth_subject, display_name, timezone_name) VALUES ($1, $2, 'Admission fixture', 'UTC')")
        .bind(scope.user_id).bind(&subject).execute(pool).await.expect("fixture owner");
    sqlx::query("INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) VALUES ($1, $2, $3, 'Admission fixture', 'UTC')")
        .bind(scope.workspace_id).bind(scope.user_id).bind(format!("admission-{}", scope.workspace_id.simple()))
        .execute(pool).await.expect("fixture workspace");
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("fixture membership");
    (scope, seed_prepared_deletion(pool, scope).await)
}

async fn seed_prepared_deletion(pool: &PgPool, scope: OAuthScope) -> Uuid {
    let subject: String = sqlx::query_scalar("SELECT auth_subject FROM users WHERE id = $1")
        .bind(scope.user_id)
        .fetch_one(pool)
        .await
        .expect("current fixture owner subject");
    let deletion_id = Uuid::new_v4();
    let mut request_hash = [0xb1_u8; 32];
    request_hash[..16].copy_from_slice(deletion_id.as_bytes());
    let prepared_at = Utc::now() - ChronoDuration::hours(26);
    sqlx::query("INSERT INTO account_deletion_lifecycles (id, workspace_id, user_id, \
        owner_subject_hash, prepare_request_hash, explicit_approval_digest, principal_rate_limit_evidence_hash, \
        external_principal_key_version, external_principal_pseudonym, authorizing_session_id, \
        authorizing_session_revision, authorizing_credential_issued_at, authorizing_recovery_code_id, \
        authorizing_recovery_code_revision, authorizing_recovery_code_created_at, prepared_at, created_at, updated_at) \
        VALUES ($1, $2, $3, sha256(convert_to($4, 'UTF8')), $5, $6, $7, 1, $8, $9, 1, $10, $11, 1, $12, $13, $13, $13)")
        .bind(deletion_id).bind(scope.workspace_id).bind(scope.user_id).bind(&subject)
        .bind(request_hash.as_slice()).bind([0xb2_u8; 32].as_slice()).bind([0xb3_u8; 32].as_slice())
        .bind([0xb4_u8; 32].as_slice()).bind(Uuid::new_v4()).bind(prepared_at - ChronoDuration::hours(1))
        .bind(Uuid::new_v4()).bind(prepared_at - ChronoDuration::hours(25)).bind(prepared_at)
        .execute(pool).await.expect("prepared deletion scope fixture");
    deletion_id
}

async fn replace_deletion_scope_fixture(
    pool: &PgPool,
    old_scope: OAuthScope,
    old_deletion: Uuid,
    retain_workspace: bool,
) -> OAuthScope {
    sqlx::query(
        "WITH operation AS (SELECT clock_timestamp() AS at) \
        UPDATE account_deletion_lifecycles SET status = 'cancelled', revision = 2, \
        cancelled_at = operation.at, updated_at = operation.at FROM operation WHERE id = $1",
    )
    .bind(old_deletion)
    .execute(pool)
    .await
    .expect("cancel prior prepared deletion normally");
    let scope = if retain_workspace {
        let scope = OAuthScope {
            workspace_id: old_scope.workspace_id,
            user_id: Uuid::new_v4(),
        };
        sqlx::query(
            "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
            VALUES ($1, $2, 'Replacement admission owner', 'UTC')",
        )
        .bind(scope.user_id)
        .bind(format!(
            "replacement-admission-owner-{}",
            scope.user_id.simple()
        ))
        .execute(pool)
        .await
        .expect("new workspace owner");
        sqlx::query("UPDATE workspaces SET owner_user_id = $2 WHERE id = $1")
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .execute(pool)
            .await
            .expect("transfer fixture ownership");
        sqlx::query("DELETE FROM workspace_members WHERE workspace_id = $1 AND user_id = $2")
            .bind(old_scope.workspace_id)
            .bind(old_scope.user_id)
            .execute(pool)
            .await
            .expect("remove former owner membership");
        scope
    } else {
        let scope = OAuthScope {
            workspace_id: Uuid::new_v4(),
            user_id: old_scope.user_id,
        };
        sqlx::query(
            "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
            VALUES ($1, $2, $3, 'Second admission workspace', 'UTC')",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(format!("admission-{}", scope.workspace_id.simple()))
        .execute(pool)
        .await
        .expect("same user second workspace");
        scope
    };
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("new scope owner membership");
    scope
}

struct TestDatabase {
    admin: PgPool,
    pool_a: PgPool,
    pool_b: PgPool,
    schema: String,
    runtime_a_name: String,
    runtime_b_name: String,
}

impl TestDatabase {
    async fn create() -> Self {
        let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL").expect("test database URL");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("test database URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("admin pool");
        let test_id = Uuid::new_v4().simple().to_string();
        let schema = format!("dayweave_provider_admission_{test_id}");
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("isolated test schema");
        let runtime_names = (
            format!("admission-a-{test_id}"),
            format!("admission-b-{test_id}"),
        );
        let pool_a = scoped_pool(options.clone().application_name(&runtime_names.0), &schema).await;
        let pool_b = scoped_pool(options.application_name(&runtime_names.1), &schema).await;
        MIGRATOR
            .run(&pool_a)
            .await
            .expect("provider admission migrations");
        Self {
            admin,
            pool_a,
            pool_b,
            schema,
            runtime_a_name: runtime_names.0,
            runtime_b_name: runtime_names.1,
        }
    }

    async fn destroy(self) {
        self.pool_a.close().await;
        self.pool_b.close().await;
        self.admin
            .execute(AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .await
            .expect("remove isolated fixture");
        self.admin.close().await;
    }
}

async fn scoped_pool(options: PgConnectOptions, schema: &str) -> PgPool {
    let schema = schema.to_owned();
    PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(2))
        .after_connect(move |connection, _| {
            let statement = format!("SET search_path TO {schema}");
            Box::pin(async move {
                connection.execute(AssertSqlSafe(statement)).await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .expect("independent runtime pool")
}
