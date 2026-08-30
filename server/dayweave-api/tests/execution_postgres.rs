use std::{
    str::FromStr,
    sync::{Arc, RwLock},
};

use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    execution::{
        DeferExecution, ExecutionCommand, ExecutionDomainError, ExecutionIdempotencyKey,
        ExecutionRepositoryError, ExecutionService, ExecutionServiceError, ExecutionStatus,
        FinishExecution, PauseExecution, ResumeExecution, StartExecution,
    },
    items::{
        IdempotencyKey as ItemIdempotencyKey, ItemKind, ItemService, ItemStatus, NewItem,
        SplitPolicy,
    },
    persistence::{DatabaseScope, MIGRATOR, PostgresExecutionRepository, PostgresItemRepository},
    proposals::Clock,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

const CONCURRENT_KEY_A: &str = "execution-concurrent-a-001";
const CONCURRENT_KEY_B: &str = "execution-concurrent-b-001";
const ACTIVE_CONFLICT_KEY: &str = "execution-active-conflict-001";
const STALE_KEY: &str = "execution-stale-001";
const ABSOLUTE_PAUSE_KEY: &str = "execution-absolute-pause-001";

#[tokio::test]
async fn postgres_execution_is_transactional_cross_device_and_recoverable() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL execution test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let scenario = tokio::spawn(run_scenario(test_database.pool.clone())).await;

    // Keep cleanup outside the scenario task so assertion panics cannot leak its schema.
    test_database.destroy().await;
    scenario.expect("PostgreSQL execution scenario task succeeds");
}

#[tokio::test]
async fn execution_defer_upgrade_repairs_the_legacy_workspace_clock() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL execution upgrade test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let scenario = tokio::spawn(run_legacy_clock_upgrade(test_database.pool.clone())).await;

    test_database.destroy().await;
    scenario.expect("PostgreSQL execution upgrade scenario task succeeds");
}

#[allow(clippy::too_many_lines)] // Keeps the exact legacy schema, rows, migration, and first commands together.
async fn run_legacy_clock_upgrade(pool: PgPool) {
    for migration in [
        include_str!("../migrations/0001_identity_and_items.sql"),
        include_str!("../migrations/0002_schedule_sync_and_audit.sql"),
        include_str!("../migrations/0003_proposals_mcp_idempotency_outbox.sql"),
        include_str!("../migrations/0004_item_delta_sync.sql"),
        include_str!("../migrations/0005_execution_sessions.sql"),
        include_str!("../migrations/0006_google_oauth.sql"),
        include_str!("../migrations/0007_google_sync.sql"),
        include_str!("../migrations/0008_credential_auth_foundation.sql"),
        include_str!("../migrations/0009_sensitive_items.sql"),
        include_str!("../migrations/0010_auth_runtime.sql"),
        include_str!("../migrations/0011_google_outbound_safety.sql"),
        include_str!("../migrations/0012_schedule_publication.sql"),
        include_str!("../migrations/0013_schedule_seal_and_mcp_submission.sql"),
        include_str!("../migrations/0014_google_calendar_projection.sql"),
        include_str!("../migrations/0015_transactional_proposal_applications.sql"),
        include_str!("../migrations/0016_mcp_simulation_evidence.sql"),
        include_str!("../migrations/0017_google_refresh_generations.sql"),
    ] {
        pool.execute(migration)
            .await
            .expect("pre-defer migration applies");
    }

    let scope = seed_scope(
        &pool,
        "execution-upgrade-owner",
        "execution-upgrade-workspace",
    )
    .await;
    let latest = postgres_now(&pool).await;
    let clock = Arc::new(TestClock::new(latest));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Legacy clock task", 40).await;
    let older_session = Uuid::from_u128(4_000);
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, occurrence_id, \
         session_index, planned_block_id, source_device_id, state, revision, accumulated_seconds, \
         actual_seconds, started_at, running_since, paused_at, pause_until, pause_reason, ended_at, \
         created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, NULL, $4, 'completed', 2, 10, \
         10, $5, NULL, NULL, NULL, NULL, $6, $5, $6)",
    )
    .bind(older_session)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .bind(latest - Duration::seconds(10))
    .bind(latest)
    .execute(&pool)
    .await
    .expect("legacy terminal session");
    let legacy_active_session = Uuid::from_u128(3_500);
    let legacy_active_start = latest - Duration::days(1);
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, occurrence_id, \
         session_index, planned_block_id, source_device_id, state, revision, accumulated_seconds, \
         actual_seconds, started_at, running_since, paused_at, pause_until, pause_reason, ended_at, \
         created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, NULL, $4, 'active', 1, 0, \
         NULL, $5, $5, NULL, NULL, NULL, NULL, $5, $5)",
    )
    .bind(legacy_active_session)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .bind(legacy_active_start)
    .execute(&pool)
    .await
    .expect("legacy active session after clock rollback");
    sqlx::query(
        "INSERT INTO execution_state (workspace_id, revision, active_session_id, updated_at) \
         VALUES ($1, 3, $2, $3)",
    )
    .bind(scope.workspace_id)
    .bind(legacy_active_session)
    .bind(legacy_active_start)
    .execute(&pool)
    .await
    .expect("legacy lagging workspace clock");

    pool.execute(include_str!("../migrations/0018_execution_defer.sql"))
        .await
        .expect("execution defer migration applies");
    let repaired: DateTime<Utc> =
        sqlx::query_scalar("SELECT updated_at FROM execution_state WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(&pool)
            .await
            .expect("repaired workspace clock");
    assert_eq!(repaired, latest);

    clock.set(legacy_active_start + Duration::seconds(10));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items,
        clock.clone(),
    );
    let deferred = execution
        .command(
            3,
            ExecutionCommand::Defer(DeferExecution {
                session_id: legacy_active_session,
                move_start: latest + Duration::hours(1),
                move_end: latest + Duration::hours(2),
                actual_seconds: None,
            }),
            execution_idempotency("execution-upgrade-defer-001", 41),
        )
        .await
        .expect("post-upgrade legacy defer");
    assert_eq!(deferred.changed_session.accumulated_seconds, 10);
    assert_eq!(
        deferred.changed_session.updated_at,
        latest + Duration::microseconds(1)
    );

    let newer_session = Uuid::from_u128(3_000);
    let started = execution
        .command(
            4,
            start_command(newer_session, item_id, Uuid::new_v4()),
            execution_idempotency("execution-upgrade-start-001", 42),
        )
        .await
        .expect("post-upgrade start");
    assert_eq!(
        started.changed_session.updated_at,
        latest + Duration::microseconds(2)
    );
    let history = execution.history(10).await.expect("post-upgrade history");
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].id, newer_session);
    assert_eq!(history[1].id, legacy_active_session);
    assert_eq!(history[2].id, older_session);
}

#[allow(clippy::too_many_lines)]
async fn run_scenario(pool: PgPool) {
    MIGRATOR.run(&pool).await.expect("migrations apply");
    MIGRATOR
        .run(&pool)
        .await
        .expect("migrations are repeatable");

    let main_scope = seed_scope(&pool, "execution-owner-one", "execution-workspace-one").await;
    let other_scope = seed_scope(&pool, "execution-owner-two", "execution-workspace-two").await;
    let base = postgres_now(&pool).await;
    assert_fixture_idempotency_expiry_is_future(&pool, base).await;
    let clock = Arc::new(TestClock::new(base));
    let main_items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), main_scope)),
        clock.clone(),
    ));
    let main_execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), main_scope)),
        main_items.clone(),
        clock.clone(),
    );
    let other_items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), other_scope)),
        clock.clone(),
    ));
    let other_execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), other_scope)),
        other_items.clone(),
        clock.clone(),
    );

    let item_a = Uuid::new_v4();
    let item_b = Uuid::new_v4();
    let other_item = Uuid::new_v4();
    create_item(&main_items, item_a, "First device task", 1).await;
    create_item(&main_items, item_b, "Second device task", 2).await;
    create_item(&other_items, other_item, "Other workspace task", 3).await;

    let session_a = Uuid::new_v4();
    let session_b = Uuid::new_v4();
    let start_a = start_command(session_a, item_a, Uuid::new_v4());
    let start_b = start_command(session_b, item_b, Uuid::new_v4());
    let (result_a, result_b) = tokio::join!(
        main_execution.command(
            0,
            start_a.clone(),
            execution_idempotency(CONCURRENT_KEY_A, 11),
        ),
        main_execution.command(
            0,
            start_b.clone(),
            execution_idempotency(CONCURRENT_KEY_B, 12),
        )
    );
    let (active_session_id, waiting_start, concurrent_failure_key) =
        one_concurrent_start_wins(result_a, result_b, start_a, start_b);

    let snapshot = main_execution.snapshot().await.unwrap();
    assert_eq!(snapshot.revision, 1);
    assert_eq!(
        snapshot.active_session.as_ref().map(|session| session.id),
        Some(active_session_id)
    );
    assert_eq!(open_session_count(&pool, main_scope).await, 1);

    let active_conflict = main_execution
        .command(
            1,
            waiting_start.clone(),
            execution_idempotency(ACTIVE_CONFLICT_KEY, 13),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        active_conflict,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ActiveSessionConflict)
    ));

    let stale = main_execution
        .command(
            0,
            ExecutionCommand::Pause(PauseExecution {
                session_id: active_session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("stale request".to_owned()),
            }),
            execution_idempotency(STALE_KEY, 14),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    ));

    // A different workspace owns an independent lease and cannot observe this one.
    assert_eq!(other_execution.snapshot().await.unwrap().revision, 0);
    let other_session_id = Uuid::new_v4();
    other_execution
        .command(
            0,
            start_command(other_session_id, other_item, Uuid::new_v4()),
            execution_idempotency("execution-other-start-001", 15),
        )
        .await
        .unwrap();
    assert_eq!(open_session_count(&pool, other_scope).await, 1);
    clock.set(base + Duration::seconds(30));
    other_execution
        .command(
            1,
            ExecutionCommand::Skip(FinishExecution {
                session_id: other_session_id,
                actual_seconds: Some(0),
            }),
            execution_idempotency("execution-other-skip-001", 16),
        )
        .await
        .unwrap();

    let absolute_until = base + Duration::minutes(5);
    let absolute_pause = ExecutionCommand::Pause(PauseExecution {
        session_id: active_session_id,
        duration_seconds: None,
        pause_until: Some(absolute_until),
        reason: Some("Short break".to_owned()),
    });
    let paused = main_execution
        .command(
            1,
            absolute_pause.clone(),
            execution_idempotency(ABSOLUTE_PAUSE_KEY, 17),
        )
        .await
        .unwrap();
    assert_eq!(paused.revision, 2);
    assert_eq!(paused.changed_session.status, ExecutionStatus::Paused);

    clock.set(absolute_until + Duration::seconds(1));
    let replay = main_execution
        .command(
            1,
            absolute_pause.clone(),
            execution_idempotency(ABSOLUTE_PAUSE_KEY, 17),
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.revision, 2);
    assert_eq!(replay.changed_session, paused.changed_session);

    let idempotency_conflict = main_execution
        .command(
            1,
            absolute_pause,
            execution_idempotency(ABSOLUTE_PAUSE_KEY, 18),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        idempotency_conflict,
        ExecutionServiceError::Repository(ExecutionRepositoryError::IdempotencyConflict)
    ));

    let extended = main_execution
        .command(
            2,
            ExecutionCommand::Pause(PauseExecution {
                session_id: active_session_id,
                duration_seconds: Some(600),
                pause_until: None,
                reason: None,
            }),
            execution_idempotency("execution-extend-001", 19),
        )
        .await
        .unwrap();
    assert_eq!(extended.revision, 3);
    assert_eq!(
        extended.changed_session.pause_reason.as_deref(),
        Some("Short break")
    );
    assert_eq!(
        extended.changed_session.pause_until,
        Some(clock.now() + Duration::seconds(600))
    );

    let resumed_at = clock.now() + Duration::minutes(1);
    clock.set(resumed_at);
    main_execution
        .command(
            3,
            ExecutionCommand::Resume(ResumeExecution {
                session_id: active_session_id,
            }),
            execution_idempotency("execution-resume-001", 20),
        )
        .await
        .unwrap();

    // A forgotten running timer remains terminally recoverable beyond the old ceiling.
    let completed_at = resumed_at + Duration::days(400);
    clock.set(completed_at);
    let completed = main_execution
        .command(
            4,
            ExecutionCommand::Complete(FinishExecution {
                session_id: active_session_id,
                actual_seconds: None,
            }),
            execution_idempotency("execution-complete-001", 21),
        )
        .await
        .unwrap();
    assert_eq!(completed.revision, 5);
    assert!(completed.active_session.is_none());
    assert!(
        completed
            .changed_session
            .actual_seconds
            .is_some_and(|seconds| seconds >= 400 * 24 * 60 * 60)
    );
    assert_eq!(open_session_count(&pool, main_scope).await, 0);

    let waiting_session_id = waiting_start.session_id();
    clock.set(completed_at + Duration::minutes(1));
    main_execution
        .command(
            5,
            waiting_start,
            execution_idempotency("execution-second-start-001", 22),
        )
        .await
        .unwrap();
    clock.set(completed_at + Duration::minutes(2));
    main_execution
        .command(
            6,
            ExecutionCommand::Skip(FinishExecution {
                session_id: waiting_session_id,
                actual_seconds: Some(15),
            }),
            execution_idempotency("execution-second-skip-001", 23),
        )
        .await
        .unwrap();

    let final_snapshot = main_execution.snapshot().await.unwrap();
    assert_eq!(final_snapshot.revision, 7);
    assert!(final_snapshot.active_session.is_none());
    let history = main_execution.history(10).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].id, waiting_session_id);
    assert_eq!(history[1].id, active_session_id);
    let other_history = other_execution.history(10).await.unwrap();
    assert_eq!(other_history.len(), 1);
    assert_eq!(other_history[0].id, other_session_id);

    assert_execution_side_effects(&pool, main_scope, 7).await;
    for failed_key in [concurrent_failure_key, ACTIVE_CONFLICT_KEY, STALE_KEY] {
        assert_eq!(
            execution_idempotency_count(&pool, main_scope, failed_key).await,
            0,
            "failed mutation leaked idempotency reservation for {failed_key}"
        );
    }

    assert_postgres_defer_transitions(&pool).await;
}

#[allow(clippy::too_many_lines)]
async fn assert_postgres_defer_transitions(pool: &PgPool) {
    const INVALID_KEY: &str = "execution-defer-invalid-001";
    const STALE_DEFER_KEY: &str = "execution-defer-stale-001";
    const DEFER_KEY: &str = "execution-defer-active-001";

    let scope = seed_scope(pool, "execution-defer-owner", "execution-defer-workspace").await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let first_item = Uuid::new_v4();
    let second_item = Uuid::new_v4();
    create_item(&items, first_item, "Defer active task", 31).await;
    create_item(&items, second_item, "Defer paused task", 32).await;

    let first_session = Uuid::from_u128(2_000);
    execution
        .command(
            0,
            start_command(first_session, first_item, Uuid::new_v4()),
            execution_idempotency("execution-defer-first-start-001", 33),
        )
        .await
        .unwrap();
    clock.set(base + Duration::seconds(45));

    let invalid = execution
        .command(
            1,
            ExecutionCommand::Defer(DeferExecution {
                session_id: first_session,
                move_start: clock.now(),
                move_end: clock.now() + Duration::minutes(30),
                actual_seconds: None,
            }),
            execution_idempotency(INVALID_KEY, 34),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        invalid,
        ExecutionServiceError::Domain(ExecutionDomainError::InvalidDefer)
    ));

    let first_move_start = clock.now() + Duration::hours(1);
    let first_move_end = first_move_start + Duration::hours(1);
    let defer_active = ExecutionCommand::Defer(DeferExecution {
        session_id: first_session,
        move_start: first_move_start,
        move_end: first_move_end,
        actual_seconds: None,
    });
    let stale = execution
        .command(
            0,
            defer_active.clone(),
            execution_idempotency(STALE_DEFER_KEY, 35),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    ));

    let deferred = execution
        .command(
            1,
            defer_active.clone(),
            execution_idempotency(DEFER_KEY, 36),
        )
        .await
        .unwrap();
    assert_eq!(deferred.revision, 2);
    assert!(deferred.active_session.is_none());
    assert_eq!(deferred.changed_session.status, ExecutionStatus::Deferred);
    assert_eq!(deferred.changed_session.accumulated_seconds, 45);
    assert_eq!(deferred.changed_session.actual_seconds, Some(45));
    assert_eq!(deferred.changed_session.move_start, Some(first_move_start));
    assert_eq!(deferred.changed_session.move_end, Some(first_move_end));
    assert_eq!(open_session_count(pool, scope).await, 0);

    clock.set(first_move_end + Duration::seconds(1));
    let replay = execution
        .command(1, defer_active, execution_idempotency(DEFER_KEY, 36))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.revision, 2);
    assert_eq!(replay.changed_session, deferred.changed_session);

    // A later session keeps causal order even if the host clock rolls backward and its UUID sorts
    // before the older terminal row.
    let rollback_start = base - Duration::days(30);
    clock.set(rollback_start);
    let second_session = Uuid::from_u128(1_000);
    let started_second = execution
        .command(
            2,
            start_command(second_session, second_item, Uuid::new_v4()),
            execution_idempotency("execution-defer-second-start-001", 37),
        )
        .await
        .unwrap();
    assert_eq!(
        started_second.changed_session.updated_at,
        deferred.changed_session.updated_at + Duration::microseconds(1)
    );
    clock.set(rollback_start + Duration::seconds(20));
    execution
        .command(
            3,
            ExecutionCommand::Pause(PauseExecution {
                session_id: second_session,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Waiting to move".to_owned()),
            }),
            execution_idempotency("execution-defer-second-pause-001", 38),
        )
        .await
        .unwrap();
    clock.set(clock.now() + Duration::minutes(10));
    let second_move_start = clock.now() + Duration::days(30);
    let second_move_end = second_move_start + Duration::hours(24);
    let deferred_paused = execution
        .command(
            4,
            ExecutionCommand::Defer(DeferExecution {
                session_id: second_session,
                move_start: second_move_start,
                move_end: second_move_end,
                actual_seconds: Some(7),
            }),
            execution_idempotency("execution-defer-paused-001", 39),
        )
        .await
        .unwrap();
    assert_eq!(deferred_paused.revision, 5);
    assert!(deferred_paused.active_session.is_none());
    assert_eq!(
        deferred_paused.changed_session.status,
        ExecutionStatus::Deferred
    );
    assert_eq!(deferred_paused.changed_session.accumulated_seconds, 20);
    assert_eq!(deferred_paused.changed_session.actual_seconds, Some(7));
    assert_eq!(
        deferred_paused.changed_session.move_start,
        Some(second_move_start)
    );
    assert_eq!(
        deferred_paused.changed_session.move_end,
        Some(second_move_end)
    );
    assert_eq!(open_session_count(pool, scope).await, 0);

    let history = execution.history(10).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].id, second_session);
    assert_eq!(history[1].id, first_session);
    assert!(history.iter().all(|session| {
        session.status == ExecutionStatus::Deferred
            && session.move_start.is_some()
            && session.move_end.is_some()
    }));
    let deferred_outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND event_type = 'execution.deferred'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(deferred_outbox_count, 2);
    assert_execution_side_effects(pool, scope, 5).await;
    for failed_key in [INVALID_KEY, STALE_DEFER_KEY] {
        assert_eq!(
            execution_idempotency_count(pool, scope, failed_key).await,
            0,
            "failed defer leaked idempotency reservation for {failed_key}"
        );
    }

    for invalid_update in [
        "UPDATE execution_sessions SET move_end = NULL WHERE workspace_id = $1 AND id = $2",
        "UPDATE execution_sessions SET ended_at = updated_at - interval '1 second' \
         WHERE workspace_id = $1 AND id = $2",
        "UPDATE execution_sessions SET move_start = ended_at WHERE workspace_id = $1 AND id = $2",
        "UPDATE execution_sessions SET move_end = move_start + interval '24 hours 1 second' \
         WHERE workspace_id = $1 AND id = $2",
    ] {
        assert!(
            sqlx::query(invalid_update)
                .bind(scope.workspace_id)
                .bind(first_session)
                .execute(pool)
                .await
                .is_err(),
            "database must reject malformed durable defer windows"
        );
    }
}

fn one_concurrent_start_wins(
    result_a: Result<dayweave_api::execution::ExecutionMutation, ExecutionServiceError>,
    result_b: Result<dayweave_api::execution::ExecutionMutation, ExecutionServiceError>,
    start_a: ExecutionCommand,
    start_b: ExecutionCommand,
) -> (Uuid, ExecutionCommand, &'static str) {
    match (result_a, result_b) {
        (Ok(winner), Err(loser)) => {
            assert_revision_conflict(&loser);
            (winner.changed_session.id, start_b, CONCURRENT_KEY_B)
        }
        (Err(loser), Ok(winner)) => {
            assert_revision_conflict(&loser);
            (winner.changed_session.id, start_a, CONCURRENT_KEY_A)
        }
        (left, right) => panic!("exactly one concurrent start must win: {left:?}, {right:?}"),
    }
}

fn assert_revision_conflict(error: &ExecutionServiceError) {
    assert!(matches!(
        error,
        ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    ));
}

async fn create_item(service: &ItemService, id: Uuid, title: &str, marker: u8) {
    service
        .create(
            NewItem {
                id,
                is_sensitive: false,
                kind: ItemKind::Task,
                status: ItemStatus::Planned,
                title: title.to_owned(),
                notes: Some("PostgreSQL execution integration fixture".to_owned()),
                timezone_name: "Europe/Madrid".to_owned(),
                duration_seconds: Some(3600),
                deadline_at: None,
                earliest_start_at: None,
                recurrence: None,
                flexible_constraints: json!({"energy": "deep"}),
                split_policy: SplitPolicy::Indivisible,
                importance: 80,
                urgency: 60,
                parent_id: None,
                sibling_order: 0,
            },
            ItemIdempotencyKey {
                key: format!("execution-item-{marker:03}"),
                fingerprint: [marker; 32],
            },
        )
        .await
        .unwrap();
}

async fn postgres_now(pool: &PgPool) -> DateTime<Utc> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .expect("read PostgreSQL fixture time");
    DateTime::from_timestamp_micros(now.timestamp_micros())
        .expect("PostgreSQL fixture time remains representable at microsecond precision")
}

async fn assert_fixture_idempotency_expiry_is_future(pool: &PgPool, base: DateTime<Utc>) {
    let expires_at = base + Duration::hours(24);
    let is_future: bool = sqlx::query_scalar("SELECT $1::timestamptz > clock_timestamp()")
        .bind(expires_at)
        .fetch_one(pool)
        .await
        .expect("compare fixture idempotency expiry with PostgreSQL time");
    assert!(
        is_future,
        "fixture item idempotency expiry must be later than PostgreSQL creation time"
    );
}

fn start_command(session_id: Uuid, item_id: Uuid, device_id: Uuid) -> ExecutionCommand {
    ExecutionCommand::Start(StartExecution {
        session_id,
        item_id,
        item_revision: 1,
        occurrence_id: None,
        session_index: 0,
        planned_block_id: None,
        device_id,
    })
}

fn execution_idempotency(key: &str, marker: u8) -> ExecutionIdempotencyKey {
    ExecutionIdempotencyKey {
        key: key.to_owned(),
        fingerprint: [marker; 32],
    }
}

async fn open_session_count(pool: &PgPool, scope: DatabaseScope) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM execution_sessions WHERE workspace_id = $1 \
         AND state IN ('active', 'paused')",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn execution_idempotency_count(pool: &PgPool, scope: DatabaseScope, key: &str) -> i64 {
    let key_hash: [u8; 32] = Sha256::digest(key.as_bytes()).into();
    sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'execution.command' AND key_hash = $2",
    )
    .bind(scope.workspace_id)
    .bind(key_hash.as_slice())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn assert_execution_side_effects(pool: &PgPool, scope: DatabaseScope, expected: i64) {
    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND aggregate_type = 'execution_session'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let idempotency_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'execution.command'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let completed_idempotency_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'execution.command' AND state = 'completed' \
         AND response_json IS NOT NULL",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let session_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM execution_sessions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .unwrap();

    assert_eq!(outbox_count, expected);
    assert_eq!(idempotency_count, expected);
    assert_eq!(completed_idempotency_count, expected);
    assert_eq!(session_count, 2);
}

#[derive(Debug)]
struct TestClock(RwLock<DateTime<Utc>>);

impl TestClock {
    fn new(now: DateTime<Utc>) -> Self {
        Self(RwLock::new(now))
    }

    fn set(&self, now: DateTime<Utc>) {
        *self.0.write().expect("test clock write lock") = now;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.read().expect("test clock read lock")
    }
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn create(database_url: &str) -> Self {
        let options = PgConnectOptions::from_str(database_url)
            .expect("valid DAYWEAVE_TEST_DATABASE_URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        let schema = format!("dayweave_execution_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated test schema");
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .after_connect(move |connection, _| {
                let statement = format!("SET search_path TO {connection_schema}");
                Box::pin(async move {
                    connection.execute(AssertSqlSafe(statement)).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .expect("connect isolated test pool");
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
            .expect("drop isolated test schema");
        self.admin.close().await;
    }
}

async fn seed_scope(pool: &PgPool, subject: &str, slug: &str) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'Execution test owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(subject)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'Execution test workspace', 'Europe/Madrid')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(slug)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
    scope
}
