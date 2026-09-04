use std::{
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    execution::{
        DeferAssessmentRequest, DeferExecution, ExecutionCommand, ExecutionDomainError,
        ExecutionIdempotencyKey, ExecutionRepositoryError, ExecutionService, ExecutionServiceError,
        ExecutionSession, ExecutionStatus, FinishExecution, PauseExecution, ResumeExecution,
        StartExecution,
    },
    items::{
        IdempotencyKey as ItemIdempotencyKey, ItemKind, ItemRepositoryError, ItemService,
        ItemServiceError, ItemStatus, NewItem, ReplaceItem, SplitPolicy,
    },
    persistence::{DatabaseScope, MIGRATOR, PostgresExecutionRepository, PostgresItemRepository},
    proposals::Clock,
    scheduling::{
        ComposeScheduleRequest, PostgresSchedulingRepository, PublishScheduleSpec, ScheduleAccess,
        compose_canonical_schedule,
    },
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

type DeferClaimAuthorizationRow = (
    i16,
    String,
    Uuid,
    Uuid,
    Vec<u8>,
    Vec<u8>,
    Option<Vec<u8>>,
    i64,
    i64,
    i64,
);

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
async fn assessed_defer_is_durable_approval_bound_and_replayable() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; assessed Defer test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let scenario = tokio::spawn(run_assessed_defer(test_database.pool.clone())).await;

    test_database.destroy().await;
    scenario.expect("PostgreSQL assessed Defer scenario task succeeds");
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

#[tokio::test]
async fn terminal_item_projection_and_execution_start_serialize_without_deadlock() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL execution race test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let scenario = tokio::spawn(run_terminal_projection_races(test_database.pool.clone())).await;

    test_database.destroy().await;
    scenario.expect("PostgreSQL execution race scenario task succeeds");
}

#[tokio::test]
async fn deferred_start_requires_immutable_schedule_attestation() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; deferred Start test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let scenario = tokio::spawn(run_deferred_start_attestation(test_database.pool.clone())).await;

    test_database.destroy().await;
    scenario.expect("PostgreSQL deferred Start scenario task succeeds");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end scenario exercises API, repository, and DB execution seals.
async fn semantic_container_without_own_effort_is_not_executable() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; container Start test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(
        pool,
        "execution-container-owner",
        "execution-container-workspace",
    )
    .await;
    let now = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(now));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let item_id = Uuid::new_v4();
    let created = items
        .create(
            NewItem {
                id: item_id,
                is_sensitive: false,
                kind: ItemKind::Project,
                status: ItemStatus::Planned,
                title: "Container without own effort".to_owned(),
                notes: None,
                timezone_name: "UTC".to_owned(),
                duration_kind: None,
                duration_seconds: Some(3_600),
                duration_min_seconds: None,
                duration_max_seconds: None,
                duration_source: None,
                deadline_kind: None,
                deadline_date: None,
                deadline_at: None,
                deadline_strength: None,
                deadline_soft_weight: None,
                earliest_start_at: None,
                recurrence: None,
                flexible_constraints: json!({}),
                has_own_effort: Some(false),
                split_policy: SplitPolicy::Indivisible,
                importance: 50,
                urgency: 50,
                parent_id: None,
                sibling_order: 0,
                blocked_reason_kind: None,
                blocked_by_item_id: None,
                blocked_reason: None,
            },
            ItemIdempotencyKey {
                key: "execution-container-item-001".to_owned(),
                fingerprint: [0xC1; 32],
            },
        )
        .await
        .expect("create semantic container");
    assert!(!created.item.is_executable);

    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let rejected = execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id,
                item_revision: created.item.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-container-start-001", 0xC2),
        )
        .await
        .expect_err("default Project cannot Start");
    assert!(matches!(rejected, ExecutionServiceError::ItemNotExecutable));
    assert!(
        raw_active_start(
            pool,
            scope,
            item_id,
            i64::try_from(created.item.revision).unwrap(),
            None,
            0,
            None,
            now,
        )
        .await
        .is_err(),
        "database semantic seal must reject a bypassed Start"
    );

    let mut add_own_effort = item_replacement(&created.item, ItemStatus::Planned);
    add_own_effort.flexible_constraints = json!({"has_own_effort": true});
    add_own_effort.has_own_effort = Some(true);
    let executable = items
        .replace(
            item_id,
            created.item.revision,
            add_own_effort,
            ItemIdempotencyKey {
                key: "execution-container-own-effort-001".to_owned(),
                fingerprint: [0xC3; 32],
            },
        )
        .await
        .expect("add explicit own component");
    assert!(executable.item.is_executable);
    let session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: executable.item.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-container-own-start-001", 0xC4),
        )
        .await
        .expect("explicit Project own component can Start while it is a leaf");

    let mut remove_own_effort = item_replacement(&executable.item, ItemStatus::Planned);
    remove_own_effort.flexible_constraints = json!({});
    remove_own_effort.has_own_effort = Some(false);
    let conflict = items
        .replace(
            item_id,
            executable.item.revision,
            remove_own_effort,
            ItemIdempotencyKey {
                key: "execution-container-remove-own-effort-001".to_owned(),
                fingerprint: [0xC5; 32],
            },
        )
        .await
        .expect_err("active own component cannot be removed");
    assert!(matches!(
        conflict,
        ItemServiceError::Repository(ItemRepositoryError::ActiveExecutionConflict {
            item_id: conflicted_item,
            session_id: conflicted_session,
        }) if conflicted_item == item_id && conflicted_session == session_id
    ));

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle proves all three hierarchy transitions share the execution fence.
async fn active_parent_rejects_child_create_reparent_and_restore() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; active-parent hierarchy test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(
        pool,
        "execution-active-parent-owner",
        "execution-active-parent-workspace",
    )
    .await;
    let now = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(now));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let parent_id = Uuid::new_v4();
    let movable_id = Uuid::new_v4();
    let restorable_id = Uuid::new_v4();
    create_item(&items, parent_id, "Executing parent", 0xD1).await;
    create_item(&items, movable_id, "Movable child", 0xD2).await;
    let restorable = items
        .create(
            execution_item(restorable_id, "Restorable child", Some(parent_id)),
            ItemIdempotencyKey {
                key: "execution-active-parent-restorable-create-001".to_owned(),
                fingerprint: [0xD3; 32],
            },
        )
        .await
        .expect("create restorable child");
    let deleted = items
        .trash(
            restorable_id,
            restorable.item.revision,
            ItemIdempotencyKey {
                key: "execution-active-parent-restorable-trash-001".to_owned(),
                fingerprint: [0xD4; 32],
            },
        )
        .await
        .expect("trash child so parent is a leaf");
    let parent = items.get(parent_id).await.expect("load refreshed parent");
    assert!(parent.is_executable);

    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock,
    );
    let session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id: parent_id,
                item_revision: parent.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-active-parent-start-001", 0xD5),
        )
        .await
        .expect("start parent");

    let create_error = items
        .create(
            execution_item(Uuid::new_v4(), "New child", Some(parent_id)),
            ItemIdempotencyKey {
                key: "execution-active-parent-child-create-001".to_owned(),
                fingerprint: [0xD6; 32],
            },
        )
        .await
        .expect_err("child create must not close active parent");
    let movable = items.get(movable_id).await.expect("load movable child");
    let mut reparent = item_replacement(&movable, movable.status);
    reparent.parent_id = Some(parent_id);
    let reparent_error = items
        .replace(
            movable_id,
            movable.revision,
            reparent,
            ItemIdempotencyKey {
                key: "execution-active-parent-reparent-001".to_owned(),
                fingerprint: [0xD7; 32],
            },
        )
        .await
        .expect_err("reparent must not close active parent");
    let restore_error = items
        .restore(
            restorable_id,
            deleted.item.revision,
            ItemIdempotencyKey {
                key: "execution-active-parent-restore-001".to_owned(),
                fingerprint: [0xD8; 32],
            },
        )
        .await
        .expect_err("restore must not close active parent");

    for error in [create_error, reparent_error, restore_error] {
        assert!(matches!(
            error,
            ItemServiceError::Repository(ItemRepositoryError::ActiveExecutionConflict {
                item_id: conflicted_item,
                session_id: conflicted_session,
            }) if conflicted_item == parent_id && conflicted_session == session_id
        ));
    }
    assert!(items.get(parent_id).await.unwrap().is_executable);
    assert_eq!(items.get(movable_id).await.unwrap().parent_id, None);
    assert!(items.get(restorable_id).await.is_err());

    test_database.destroy().await;
}

async fn run_terminal_projection_races(pool: PgPool) {
    MIGRATOR.run(&pool).await.expect("migrations apply");
    start_holds_execution_state_before_terminal_projection(&pool).await;
    terminal_projection_holds_execution_state_before_start(&pool).await;
    start_holds_execution_state_before_trash(&pool).await;
    trash_holds_execution_state_before_start(&pool).await;
}

#[allow(clippy::too_many_lines)] // One lifecycle proves the assessment, approval, claim, and replay contract together.
async fn run_assessed_defer(pool: PgPool) {
    const START_KEY: &str = "execution-assessed-defer-start-001";
    const PAUSE_KEY: &str = "execution-assessed-defer-pause-001";
    const STALE_KEY: &str = "execution-assessed-defer-stale-001";
    const MISSING_APPROVAL_KEY: &str = "execution-assessed-defer-unapproved-001";
    const DEFER_KEY: &str = "execution-assessed-defer-commit-001";

    MIGRATOR.run(&pool).await.expect("migrations apply");
    let scope = seed_scope(
        &pool,
        "execution-assessed-defer-owner",
        "execution-assessed-defer-workspace",
    )
    .await;
    let base = postgres_now(&pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Approval-bound 601-second defer", 201).await;
    let item = items.get(item_id).await.expect("load assessment item");
    let (source_block_id, move_start) = publish_v5_defer_policy(
        &pool,
        scope,
        "execution-assessed-defer-owner",
        &items,
        &schedules,
        base,
        item_id,
        item.revision,
    )
    .await;

    let snapshot_shape: (String, String, bool) = sqlx::query_as(
        "SELECT detail.result_snapshot ->> 'schema_version', \
                detail.result_snapshot ->> 'scheduler_publication_schema', \
                detail.result_snapshot ? 'planning_request' \
           FROM schedule_revision_details AS detail \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = detail.workspace_id \
            AND revision.id = detail.schedule_revision_id \
          WHERE revision.workspace_id = $1 AND revision.state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&pool)
    .await
    .expect("load private v5 planning capsule");
    assert_eq!(snapshot_shape.0, "5");
    assert_eq!(snapshot_shape.1, "dayweave-scheduler-publication/5");
    assert!(snapshot_shape.2);

    // The fixture starts 601 seconds before the database wall clock and pauses
    // at it. This records exact observed progress without pushing the durable
    // protocol clock beyond the assessment's five-minute expiry horizon.
    clock.set(base - Duration::seconds(601));
    let session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: item.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(START_KEY, 202),
        )
        .await
        .expect("start the exact published v5 source block");
    clock.set(base);
    let paused = execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Choose an exact later window".to_owned()),
            }),
            execution_idempotency(PAUSE_KEY, 203),
        )
        .await
        .expect("pause after exactly 601 observed seconds");
    assert_eq!(paused.changed_session.status, ExecutionStatus::Paused);
    assert_eq!(paused.changed_session.accumulated_seconds, 601);

    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start,
            actual_seconds: Some(601),
        })
        .await
        .expect("assess the fixed-block-overlapping replacement");
    assert_eq!(assessment.actual_seconds, 601);
    assert_eq!(assessment.credited_source_seconds, 660);
    assert_eq!(assessment.planned_duration_seconds, 3_600);
    assert_eq!(assessment.remaining_duration_seconds, 2_940);
    assert_eq!(assessment.move_end, move_start + Duration::minutes(49));
    assert!(assessment.approval_required);
    assert!(!assessment.violations.is_empty());

    let assessment_digest = decode_sha256_digest(&assessment.assessment_digest);
    let environment_digest = decode_sha256_digest(&assessment.environment_digest);
    let stored_assessment: (i64, i64, i64, i64, i64, i32, Vec<u8>, Vec<u8>, bool) = sqlx::query_as(
        "SELECT credited_before_seconds, effective_actual_seconds, \
                    credited_after_seconds, credited_source_seconds, \
                    remaining_duration_seconds, scheduler_slot_seconds, \
                    environment_digest, assessment_digest, approval_required \
               FROM execution_defer_assessments \
              WHERE workspace_id = $1 AND user_id = $2 AND assessment_digest = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(assessment_digest.as_slice())
    .fetch_one(&pool)
    .await
    .expect("load exact private defer assessment");
    assert_eq!(stored_assessment.0, 0);
    assert_eq!(stored_assessment.1, 601);
    assert_eq!(stored_assessment.2, 601);
    assert_eq!(stored_assessment.3, 660);
    assert_eq!(stored_assessment.4, 2_940);
    assert_eq!(stored_assessment.5, 300);
    assert_eq!(stored_assessment.6, environment_digest);
    assert_eq!(stored_assessment.7, assessment_digest);
    assert!(stored_assessment.8);
    let expiring_unapplied_digest =
        insert_short_lived_unapplied_assessment(&pool, scope, &assessment_digest).await;

    // Command protocol time must itself fall within the short assessment
    // lifetime; production uses the wall clock, while this fixture controls it.
    clock.set(assessment.expires_at - Duration::minutes(1));
    let stale_digest = format!("sha256:{}", "00".repeat(32));
    let stale = execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start: assessment.move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(601),
                assessment_digest: Some(stale_digest.clone()),
                approved_assessment_digest: Some(stale_digest),
            }),
            execution_idempotency(STALE_KEY, 204),
        )
        .await
        .expect_err("an unknown canonical assessment digest is stale");
    assert!(matches!(
        stale,
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferAssessmentStale)
    ));

    let unapproved = execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start: assessment.move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(601),
                assessment_digest: Some(assessment.assessment_digest.clone()),
                approved_assessment_digest: None,
            }),
            execution_idempotency(MISSING_APPROVAL_KEY, 205),
        )
        .await
        .expect_err("a conflicting assessment requires its exact approval digest");
    assert!(matches!(
        unapproved,
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferApprovalRequired)
    ));
    assert_eq!(
        execution_idempotency_count(&pool, scope, STALE_KEY).await,
        0
    );
    assert_eq!(
        execution_idempotency_count(&pool, scope, MISSING_APPROVAL_KEY).await,
        0
    );

    let defer_command = ExecutionCommand::Defer(DeferExecution {
        session_id,
        move_start: assessment.move_start,
        move_end: assessment.move_end,
        actual_seconds: Some(601),
        assessment_digest: Some(assessment.assessment_digest.clone()),
        approved_assessment_digest: Some(assessment.assessment_digest.clone()),
    });
    let deferred = execution
        .command(
            2,
            defer_command.clone(),
            execution_idempotency(DEFER_KEY, 206),
        )
        .await
        .expect("commit the exactly approved replacement");
    assert!(!deferred.replayed);
    assert_eq!(deferred.revision, 3);
    assert_eq!(deferred.changed_session.status, ExecutionStatus::Deferred);
    assert_eq!(deferred.changed_session.actual_seconds, Some(601));

    let claim: DeferClaimAuthorizationRow = sqlx::query_as(
        "SELECT authorization_schema_version, authorization_kind, assessment_id, \
                authorized_by_user_id, environment_digest, assessment_digest, \
                approved_assessment_digest, consumed_by_source_seconds, \
                remaining_duration_seconds, \
                EXTRACT(EPOCH FROM (move_end - move_start))::bigint \
           FROM execution_defer_replacement_claims \
          WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .expect("load exact v1 replacement claim");
    assert_eq!(claim.0, 1);
    assert_eq!(claim.1, "explicit_approval");
    let stored_assessment_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM execution_defer_assessments \
          WHERE workspace_id = $1 AND assessment_digest = $2",
    )
    .bind(scope.workspace_id)
    .bind(assessment_digest.as_slice())
    .fetch_one(&pool)
    .await
    .expect("load assessment identity");
    assert_eq!(claim.2, stored_assessment_id);
    assert_eq!(claim.3, scope.user_id);
    assert_eq!(claim.4, environment_digest);
    assert_eq!(claim.5, assessment_digest);
    assert_eq!(claim.6.as_deref(), Some(assessment_digest.as_slice()));
    assert_eq!(claim.7, 660);
    assert_eq!(claim.8, 2_940);
    assert_eq!(claim.9, 2_940);

    // Replay is resolved before revision, assessment-expiry, and command-time
    // validation. Move the service clock beyond both the assessment and its
    // target while the 24-hour command receipt remains live.
    clock.set(assessment.move_end + Duration::seconds(1));
    assert!(clock.now() > assessment.expires_at);
    let replay = execution
        .command(2, defer_command, execution_idempotency(DEFER_KEY, 206))
        .await
        .expect("exact receipt replays after assessment and target expiry");
    assert!(replay.replayed);
    assert_eq!(replay.revision, deferred.revision);
    assert_eq!(replay.changed_session, deferred.changed_session);
    let claim_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM execution_defer_replacement_claims \
          WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .expect("count replayed replacement claims");
    assert_eq!(claim_count, 1);

    tokio::time::sleep(StdDuration::from_millis(300)).await;
    PostgresExecutionRepository::new(pool.clone(), scope)
        .maintain_defer_assessment_retention()
        .await
        .expect("prune expired unapplied assessment evidence");
    let retained_assessments: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE assessment_digest = $2), \
                COUNT(*) FILTER (WHERE assessment_digest = $3) \
           FROM execution_defer_assessments WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(assessment_digest.as_slice())
    .bind(expiring_unapplied_digest.as_slice())
    .fetch_one(&pool)
    .await
    .expect("inspect assessment retention result");
    assert_eq!(retained_assessments, (1, 0));

    assert_defer_clock_domains(&pool).await;
    assert_fractional_prior_credit_rounds_the_aggregate(&pool).await;
}

#[allow(clippy::too_many_lines)] // One fixture must compare the DB wall clock and protocol clock.
async fn assert_defer_clock_domains(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-defer-clock-owner",
        "execution-defer-clock-workspace",
    )
    .await;
    let database_base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(database_base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Defer clock domains", 208).await;
    let (source_block_id, _) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-defer-clock-owner",
        &items,
        &schedules,
        database_base,
        item_id,
        1,
    )
    .await;

    let protocol_time = database_base + Duration::hours(1);
    let session_id = Uuid::new_v4();
    clock.set(protocol_time - Duration::seconds(1));
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-defer-clock-start-001", 209),
        )
        .await
        .expect("start future-protocol clock fixture");
    clock.set(protocol_time);
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Prove database and protocol clock separation".to_owned()),
            }),
            execution_idempotency("execution-defer-clock-pause-001", 210),
        )
        .await
        .expect("pause future-protocol clock fixture");
    let protocol_updated_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT updated_at FROM execution_state WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .expect("load monotonic protocol transition time");
    assert_eq!(protocol_updated_at, protocol_time);

    let database_now = postgres_now(pool).await;
    let after_expiry_but_before_protocol =
        align_up_to_five_minutes(database_now) + Duration::minutes(10);
    assert!(after_expiry_but_before_protocol > database_now + Duration::minutes(5));
    assert!(after_expiry_but_before_protocol <= protocol_updated_at);
    let too_early = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start: after_expiry_but_before_protocol,
            actual_seconds: Some(1),
        })
        .await
        .expect_err("target after DB expiry but before protocol transition is invalid");
    assert!(matches!(
        too_early,
        ExecutionServiceError::Repository(ExecutionRepositoryError::InvalidCommand(
            ExecutionDomainError::InvalidDefer
        ))
    ));

    let valid_target = align_up_to_five_minutes(protocol_updated_at) + Duration::minutes(10);
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start: valid_target,
            actual_seconds: Some(1),
        })
        .await
        .expect("target after both clock bounds is assessable");
    assert!(assessment.move_start > assessment.expires_at);
    assert!(assessment.move_start > protocol_updated_at);
    assert!(clock.now() > assessment.expires_at);

    let deferred = execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start: assessment.move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(assessment.actual_seconds),
                assessment_digest: Some(assessment.assessment_digest.clone()),
                approved_assessment_digest: assessment
                    .approval_required
                    .then_some(assessment.assessment_digest),
            }),
            execution_idempotency("execution-defer-clock-commit-001", 211),
        )
        .await
        .expect("DB-wall-clock-live assessment commits despite a future service clock");
    assert_eq!(deferred.revision, 3);
}

#[allow(clippy::too_many_lines)] // The setup creates real prior and in-flight source evidence.
async fn assert_fractional_prior_credit_rounds_the_aggregate(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-defer-rounding-owner",
        "execution-defer-rounding-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Aggregate minute-credit rounding", 212).await;
    let (first_source_block_id, _) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-defer-rounding-owner",
        &items,
        &schedules,
        base,
        item_id,
        1,
    )
    .await;

    let first_session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id: first_session_id,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(first_source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-defer-rounding-first-start-001", 213),
        )
        .await
        .expect("start prior-credit source");
    clock.set(base + Duration::seconds(61));
    execution
        .command(
            1,
            ExecutionCommand::Complete(FinishExecution {
                session_id: first_session_id,
                actual_seconds: Some(61),
            }),
            execution_idempotency("execution-defer-rounding-first-complete-001", 214),
        )
        .await
        .expect("record 61 exact prior seconds");

    let (second_source_block_id, move_start) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-defer-rounding-owner",
        &items,
        &schedules,
        clock.now(),
        item_id,
        1,
    )
    .await;
    let second_session_index: i32 = sqlx::query_scalar(
        "SELECT (block.constraint_snapshot ->> 'session_index')::integer \
           FROM schedule_blocks AS block \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = block.workspace_id \
            AND revision.id = block.schedule_revision_id \
          WHERE revision.workspace_id = $1 AND revision.state = 'published' \
            AND block.source_block_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(second_source_block_id)
    .fetch_one(pool)
    .await
    .expect("load second source session index");
    assert_eq!(second_session_index, 1);

    let second_session_id = Uuid::new_v4();
    clock.set(base + Duration::seconds(62));
    execution
        .command(
            2,
            ExecutionCommand::Start(StartExecution {
                session_id: second_session_id,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 1,
                planned_block_id: Some(second_source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-defer-rounding-second-start-001", 215),
        )
        .await
        .expect("start source after fractional prior credit");
    clock.set(base + Duration::seconds(121));
    execution
        .command(
            3,
            ExecutionCommand::Pause(PauseExecution {
                session_id: second_session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess aggregate rounding".to_owned()),
            }),
            execution_idempotency("execution-defer-rounding-second-pause-001", 216),
        )
        .await
        .expect("pause after 59 source seconds");
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 4,
            session_id: second_session_id,
            move_start,
            actual_seconds: Some(59),
        })
        .await
        .expect("assess source using aggregate minute rounding");
    assert_eq!(assessment.credited_source_seconds, 0);
    assert_eq!(
        assessment.remaining_duration_seconds,
        assessment.planned_duration_seconds
    );
    let stored: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT credited_before_seconds, effective_actual_seconds, \
                credited_after_seconds, credited_source_seconds, \
                planned_duration_seconds, remaining_duration_seconds \
           FROM execution_defer_assessments \
          WHERE workspace_id = $1 AND assessment_digest = $2",
    )
    .bind(scope.workspace_id)
    .bind(decode_sha256_digest(&assessment.assessment_digest).as_slice())
    .fetch_one(pool)
    .await
    .expect("load aggregate-rounding assessment evidence");
    assert_eq!(stored.0, 61);
    assert_eq!(stored.1, 59);
    assert_eq!(stored.2, 120);
    assert_eq!(stored.3, 0);
    assert_eq!(stored.4, stored.5);

    execution
        .command(
            4,
            ExecutionCommand::Defer(DeferExecution {
                session_id: second_session_id,
                move_start: assessment.move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(59),
                assessment_digest: Some(assessment.assessment_digest.clone()),
                approved_assessment_digest: assessment
                    .approval_required
                    .then_some(assessment.assessment_digest),
            }),
            execution_idempotency("execution-defer-rounding-commit-001", 217),
        )
        .await
        .expect("commit aggregate-rounded zero-source-credit defer");
    let claim: (i64, i64) = sqlx::query_as(
        "SELECT consumed_by_source_seconds, remaining_duration_seconds \
           FROM execution_defer_replacement_claims \
          WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(second_session_id)
    .fetch_one(pool)
    .await
    .expect("load aggregate-rounded claim");
    assert_eq!(claim, (0, stored.5));
}

#[allow(clippy::too_many_arguments)] // Publication setup keeps each evidence input explicit.
async fn publish_v5_defer_policy(
    pool: &PgPool,
    scope: DatabaseScope,
    subject: &str,
    items: &ItemService,
    schedules: &PostgresSchedulingRepository,
    base: DateTime<Utc>,
    item_id: Uuid,
    item_revision: u64,
) -> (Uuid, DateTime<Utc>) {
    let horizon_start = truncate_to_minute(base);
    let availability_start = align_up_to_five_minutes(base) + Duration::minutes(10);
    let move_start = align_up_to_five_minutes(base) + Duration::hours(4);
    let horizon_end = horizon_start + Duration::hours(36);
    let request: ComposeScheduleRequest = serde_json::from_value(json!({
        "as_of": base,
        "horizon_start": horizon_start,
        "horizon_end": horizon_end,
        "timezone_name": "Europe/Madrid",
        "availability": [{
            "start": availability_start,
            "end": horizon_end,
            "contexts": [],
            "location": null,
            "energy": "deep"
        }],
        "fixed_blocks": [{
            "id": Uuid::new_v4(),
            "is_sensitive": false,
            "title": "Approval-required target obstacle",
            "start": move_start,
            "end": move_start + Duration::hours(1),
            "source": "manual"
        }],
        "previous_assignments": [],
        "config": {
            "slot_granularity_minutes": 5,
            "stability_weight": 4,
            "default_soft_weight": 100
        },
        "recurrence_context": {}
    }))
    .expect("valid dynamic v5 compose request");
    let preview = compose_canonical_schedule(items, schedules, request)
        .await
        .expect("compose authoritative v5 policy");
    let source_blocks = preview
        .plan
        .blocks
        .iter()
        .filter(|block| block.item_id.is_some_and(|id| id.0 == item_id))
        .collect::<Vec<_>>();
    let source_block = source_blocks
        .first()
        .expect("assessment source must compose at least one planned block");
    let source_block_id = source_block.id;
    let access = ScheduleAccess {
        subject: subject.to_owned(),
        include_sensitive: false,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [207; 32],
                input_digest: decode_sha256_digest(&preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: preview,
                published_at: base,
            },
        )
        .await
        .expect("publish authoritative v5 planning policy");
    let stored_revision: i64 = sqlx::query_scalar(
        "SELECT (result_snapshot -> 'compose' -> 'source_item_revisions' ->> $2)::bigint \
           FROM schedule_revision_details AS detail \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = detail.workspace_id \
            AND revision.id = detail.schedule_revision_id \
          WHERE revision.workspace_id = $1 AND revision.state = 'published'",
    )
    .bind(scope.workspace_id)
    .bind(item_id.to_string())
    .fetch_one(pool)
    .await
    .expect("load published item revision");
    assert_eq!(stored_revision, i64::try_from(item_revision).unwrap());
    (source_block_id, move_start)
}

async fn current_published_source_block(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
) -> Uuid {
    let blocks: Vec<Uuid> = sqlx::query_scalar(
        "SELECT block.source_block_id FROM schedule_blocks AS block \
         JOIN schedule_revisions AS revision \
           ON revision.workspace_id = block.workspace_id \
          AND revision.id = block.schedule_revision_id \
         WHERE revision.workspace_id = $1 AND revision.state = 'published' \
           AND block.item_id = $2 ORDER BY block.ordinal",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .fetch_all(pool)
    .await
    .expect("load current published item block");
    let [source_block_id] = blocks.as_slice() else {
        panic!("fixture item must have exactly one current published block");
    };
    *source_block_id
}

#[allow(clippy::too_many_lines)]
async fn run_deferred_start_attestation(pool: PgPool) {
    MIGRATOR.run(&pool).await.expect("migrations apply");
    let scope = seed_scope(
        &pool,
        "execution-attestation-owner",
        "execution-attestation-workspace",
    )
    .await;
    let base = postgres_now(&pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_a = Uuid::new_v4();
    let item_b = Uuid::new_v4();
    create_item(&items, item_a, "Attested deferred slot", 91).await;
    create_item(&items, item_b, "Superseded attestation slot", 92).await;

    let (source_block_a, move_start_a) = publish_v5_defer_policy(
        &pool,
        scope,
        "execution-attestation-owner",
        &items,
        &schedules,
        base,
        item_a,
        1,
    )
    .await;
    let occurrence_a = None;
    let first_session_a = Uuid::new_v4();
    let first_start_a = ExecutionCommand::Start(StartExecution {
        session_id: first_session_a,
        item_id: item_a,
        item_revision: 1,
        occurrence_id: occurrence_a,
        session_index: 0,
        planned_block_id: Some(source_block_a),
        device_id: Uuid::new_v4(),
    });
    execution
        .command(
            0,
            first_start_a.clone(),
            execution_idempotency("execution-attestation-first-start-001", 93),
        )
        .await
        .expect("attested first Start remains valid");
    clock.set(base + Duration::seconds(10));
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id: first_session_a,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Choose an attested replacement".to_owned()),
            }),
            execution_idempotency("execution-attestation-first-pause-001", 190),
        )
        .await
        .expect("pause before assessing first semantic slot");
    let assessment_a = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id: first_session_a,
            move_start: move_start_a,
            actual_seconds: Some(10),
        })
        .await
        .expect("assess first semantic slot");
    let move_end_a = assessment_a.move_end;
    let deferred_a = execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id: first_session_a,
                move_start: move_start_a,
                move_end: move_end_a,
                actual_seconds: Some(10),
                assessment_digest: Some(assessment_a.assessment_digest.clone()),
                approved_assessment_digest: assessment_a
                    .approval_required
                    .then_some(assessment_a.assessment_digest),
            }),
            execution_idempotency("execution-attestation-first-defer-001", 94),
        )
        .await
        .expect("defer first semantic slot")
        .changed_session;

    let historical = execution
        .command(
            0,
            first_start_a,
            execution_idempotency("execution-attestation-first-start-001", 93),
        )
        .await
        .expect("exact historical Start retry replays ahead of the guard");
    assert!(historical.replayed);
    assert_eq!(historical.revision, 1);
    assert_eq!(execution.snapshot().await.unwrap().revision, 3);

    let missing_key = "execution-attestation-missing-block-001";
    let missing = execution
        .command(
            3,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id: item_a,
                item_revision: 1,
                occurrence_id: occurrence_a,
                session_index: 0,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(missing_key, 95),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        missing,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_failed_start_rolled_back(&pool, scope, missing_key, 3, 1, 3).await;

    let wrong_block = Uuid::new_v4();
    let wrong_key = "execution-attestation-wrong-block-001";
    let wrong = execution
        .command(
            3,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id: item_a,
                item_revision: 1,
                occurrence_id: occurrence_a,
                session_index: 0,
                planned_block_id: Some(wrong_block),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(wrong_key, 96),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        wrong,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_failed_start_rolled_back(&pool, scope, wrong_key, 3, 1, 3).await;

    let raw_without_binding = raw_active_start(
        &pool,
        scope,
        item_a,
        1,
        occurrence_a,
        0,
        None,
        base + Duration::seconds(11),
    )
    .await;
    assert!(
        raw_without_binding.is_err(),
        "raw Start must hit the defensive semantic trigger"
    );

    // More than a public history page of newer rows must not hide the exact semantic head.
    // They share the item, revision, and occurrence while differing only in session_index.
    insert_unrelated_terminal_history(&pool, scope, item_a, occurrence_a, base).await;
    insert_terminal_history_row(
        &pool,
        scope,
        item_a,
        Some(Uuid::new_v4()),
        7,
        base + Duration::hours(1),
    )
    .await;

    let replacement_session_a = Uuid::new_v4();
    let (revision_a, exact_block_a, replacement_index_a) =
        create_deferred_placement_draft(&pool, scope, &deferred_a).await;
    let exact_start_a = ExecutionCommand::Start(StartExecution {
        session_id: replacement_session_a,
        item_id: item_a,
        item_revision: 1,
        occurrence_id: occurrence_a,
        session_index: replacement_index_a,
        planned_block_id: Some(exact_block_a),
        device_id: Uuid::new_v4(),
    });
    let exact_key_a = "execution-attestation-exact-block-001";
    assert!(
        raw_active_start(
            &pool,
            scope,
            item_a,
            1,
            occurrence_a,
            i32::from(replacement_index_a),
            Some(exact_block_a),
            base + Duration::seconds(12),
        )
        .await
        .is_err(),
        "raw Start cannot consume an unpublished binding"
    );
    let unpublished = execution
        .command(
            3,
            exact_start_a.clone(),
            execution_idempotency(exact_key_a, 97),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        unpublished,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_failed_start_rolled_back(&pool, scope, exact_key_a, 3, 103, 3).await;

    publish_schedule_revision(&pool, scope, revision_a).await;
    let published_wrong_key = "execution-attestation-published-wrong-block-001";
    let published_wrong = execution
        .command(
            3,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id: item_a,
                item_revision: 1,
                occurrence_id: occurrence_a,
                session_index: replacement_index_a,
                planned_block_id: Some(wrong_block),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(published_wrong_key, 103),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        published_wrong,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_failed_start_rolled_back(&pool, scope, published_wrong_key, 3, 103, 3).await;
    let started_a = execution
        .command(
            3,
            exact_start_a.clone(),
            execution_idempotency(exact_key_a, 97),
        )
        .await
        .expect("exact published binding permits Start");
    assert_eq!(started_a.revision, 4);
    let origin_a: (Uuid, Uuid, i64, i64) = sqlx::query_as(
        "SELECT schedule_revision_id, source_block_id, execution_epoch, \
         planned_duration_seconds FROM execution_session_schedule_origins \
         WHERE workspace_id = $1 AND execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(replacement_session_a)
    .fetch_one(&pool)
    .await
    .expect("exact current published Start records its origin");
    assert_eq!(
        origin_a,
        (
            revision_a,
            exact_block_a,
            1,
            i64::try_from(assessment_a.remaining_duration_seconds).unwrap(),
        )
    );
    let consumed_source_a: Uuid = sqlx::query_scalar(
        "SELECT source_deferred_session_id FROM execution_defer_replacement_consumptions \
         WHERE workspace_id = $1 AND replacement_execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(replacement_session_a)
    .fetch_one(&pool)
    .await
    .expect("exact replacement Start consumes its claim");
    assert_eq!(consumed_source_a, first_session_a);
    clock.set(base + Duration::seconds(20));
    execution
        .command(
            4,
            ExecutionCommand::Complete(FinishExecution {
                session_id: replacement_session_a,
                actual_seconds: Some(10),
            }),
            execution_idempotency("execution-attestation-complete-001", 98),
        )
        .await
        .expect("complete attested replacement");

    let exact_replay = execution
        .command(3, exact_start_a, execution_idempotency(exact_key_a, 97))
        .await
        .expect("successful attested Start replays after the session completes");
    assert!(exact_replay.replayed);
    assert_eq!(exact_replay.revision, 4);
    assert_eq!(exact_replay.changed_session.id, replacement_session_a);
    let replayed_origin_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_session_schedule_origins \
         WHERE workspace_id = $1 AND execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(replacement_session_a)
    .fetch_one(&pool)
    .await
    .expect("count replayed Start origin");
    assert_eq!(replayed_origin_count, 1);
    let after_exact_replay = execution.snapshot().await.unwrap();
    assert_eq!(after_exact_replay.revision, 5);
    assert!(after_exact_replay.active_session.is_none());

    let completed_key = "execution-attestation-completed-head-001";
    let completed = execution
        .command(
            5,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id: item_a,
                item_revision: 1,
                occurrence_id: occurrence_a,
                session_index: replacement_index_a,
                planned_block_id: Some(exact_block_a),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(completed_key, 99),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        completed,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_failed_start_rolled_back(&pool, scope, completed_key, 5, 104, 5).await;
    assert!(
        raw_active_start(
            &pool,
            scope,
            item_a,
            1,
            occurrence_a,
            i32::from(replacement_index_a),
            Some(exact_block_a),
            base + Duration::seconds(21),
        )
        .await
        .is_err(),
        "raw Start cannot resurrect a completed semantic head"
    );

    let (source_block_b, move_start_b) = publish_v5_defer_policy(
        &pool,
        scope,
        "execution-attestation-owner",
        &items,
        &schedules,
        clock.now(),
        item_b,
        1,
    )
    .await;
    let first_session_b = Uuid::new_v4();
    execution
        .command(
            5,
            ExecutionCommand::Start(StartExecution {
                session_id: first_session_b,
                item_id: item_b,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_b),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-attestation-second-start-001", 100),
        )
        .await
        .expect("first Start for second semantic slot");
    clock.set(base + Duration::seconds(30));
    execution
        .command(
            6,
            ExecutionCommand::Pause(PauseExecution {
                session_id: first_session_b,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Choose a superseded replacement".to_owned()),
            }),
            execution_idempotency("execution-attestation-second-pause-001", 191),
        )
        .await
        .expect("pause before assessing second semantic slot");
    let assessment_b = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 7,
            session_id: first_session_b,
            move_start: move_start_b,
            actual_seconds: Some(10),
        })
        .await
        .expect("assess second semantic slot");
    let deferred_b = execution
        .command(
            7,
            ExecutionCommand::Defer(DeferExecution {
                session_id: first_session_b,
                move_start: move_start_b,
                move_end: assessment_b.move_end,
                actual_seconds: Some(10),
                assessment_digest: Some(assessment_b.assessment_digest.clone()),
                approved_assessment_digest: assessment_b
                    .approval_required
                    .then_some(assessment_b.assessment_digest),
            }),
            execution_idempotency("execution-attestation-second-defer-001", 101),
        )
        .await
        .expect("defer second semantic slot")
        .changed_session;
    let (revision_b, exact_block_b, replacement_index_b) =
        create_deferred_placement_draft(&pool, scope, &deferred_b).await;
    publish_schedule_revision(&pool, scope, revision_b).await;
    assert_ne!(revision_a, revision_b);
    supersede_schedule_revision(&pool, scope, revision_b).await;

    let superseded_key = "execution-attestation-superseded-block-001";
    let replacement_b = execution
        .command(
            8,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id: item_b,
                item_revision: 1,
                occurrence_id: None,
                session_index: replacement_index_b,
                planned_block_id: Some(exact_block_b),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(superseded_key, 102),
        )
        .await
        .expect_err("superseded replacement binding is no longer current");
    assert!(matches!(
        replacement_b,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_failed_start_rolled_back(&pool, scope, superseded_key, 8, 105, 8).await;
    assert_eq!(execution_outbox_count(&pool, scope).await, 8);
    assert_eq!(execution.snapshot().await.unwrap().revision, 8);

    assert_attested_defer_uses_origin_duration(&pool).await;
}

#[allow(clippy::too_many_lines)] // One deterministic race plus retry and later exact-replay proof.
async fn start_holds_execution_state_before_terminal_projection(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-race-start-owner",
        "execution-race-start-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let execution = Arc::new(ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    ));
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Start wins terminal projection race", 80).await;
    let item = items.get(item_id).await.expect("race item");
    ensure_execution_state(pool, scope).await;

    // Hold the row Start needs after it acquires execution_state. Once NOWAIT proves Start owns
    // that state lock, launch the terminal projection behind it and release the row.
    let mut item_blocker = pool.begin().await.expect("begin item row blocker");
    sqlx::query("SELECT id FROM items WHERE workspace_id = $1 ORDER BY id FOR UPDATE")
        .bind(scope.workspace_id)
        .fetch_all(&mut *item_blocker)
        .await
        .expect("hold canonical item rows");
    let session_id = Uuid::new_v4();
    let start_task = {
        let execution = execution.clone();
        tokio::spawn(async move {
            execution
                .command(
                    0,
                    start_command(session_id, item_id, Uuid::new_v4()),
                    execution_idempotency("execution-race-start-wins-001", 81),
                )
                .await
        })
    };
    wait_until_execution_state_is_locked(pool, scope).await;
    let terminal_task = {
        let items = items.clone();
        let replacement = item_replacement(&item, ItemStatus::Completed);
        tokio::spawn(async move {
            items
                .replace(
                    item_id,
                    item.revision,
                    replacement,
                    ItemIdempotencyKey {
                        key: "execution-race-terminal-loses-001".to_owned(),
                        fingerprint: [82; 32],
                    },
                )
                .await
        })
    };
    item_blocker
        .commit()
        .await
        .expect("release item row blocker");
    let (started, blocked) = tokio::time::timeout(StdDuration::from_secs(10), async {
        tokio::join!(start_task, terminal_task)
    })
    .await
    .expect("state-first race completes without deadlock");
    let started = started.expect("Start task joins").expect("Start wins");
    assert_eq!(started.changed_session.id, session_id);
    assert!(matches!(
        blocked.expect("terminal task joins"),
        Err(ItemServiceError::Repository(
            ItemRepositoryError::ActiveExecutionConflict {
                item_id: conflict_item,
                session_id: conflict_session,
            }
        )) if conflict_item == item_id && conflict_session == session_id
    ));
    assert_eq!(
        items.get(item_id).await.unwrap().status,
        ItemStatus::Planned
    );
    assert_eq!(
        item_idempotency_count(
            pool,
            scope,
            "items.replace",
            "execution-race-terminal-loses-001",
        )
        .await,
        0
    );

    clock.set(base + Duration::seconds(1));
    execution
        .command(
            1,
            ExecutionCommand::Complete(FinishExecution {
                session_id,
                actual_seconds: Some(1),
            }),
            execution_idempotency("execution-race-start-wins-close-001", 83),
        )
        .await
        .expect("close winning lease");
    let reconciled = items
        .replace(
            item_id,
            item.revision,
            item_replacement(&item, ItemStatus::Completed),
            ItemIdempotencyKey {
                key: "execution-race-terminal-loses-001".to_owned(),
                fingerprint: [82; 32],
            },
        )
        .await
        .expect("terminal projection retries after close");
    assert!(!reconciled.replayed);
    assert_eq!(reconciled.item.status, ItemStatus::Completed);

    let reopened = items
        .replace(
            item_id,
            reconciled.item.revision,
            item_replacement(&reconciled.item, ItemStatus::Planned),
            ItemIdempotencyKey {
                key: "execution-race-terminal-reopen-001".to_owned(),
                fingerprint: [87; 32],
            },
        )
        .await
        .expect("reopen reconciled item");
    let later_session_id = Uuid::new_v4();
    execution
        .command(
            2,
            ExecutionCommand::Start(StartExecution {
                session_id: later_session_id,
                item_id,
                item_revision: reopened.item.revision,
                occurrence_id: None,
                session_index: 1,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-race-terminal-later-start-001", 88),
        )
        .await
        .expect("later lease opens");
    let replay = items
        .replace(
            item_id,
            item.revision,
            item_replacement(&item, ItemStatus::Completed),
            ItemIdempotencyKey {
                key: "execution-race-terminal-loses-001".to_owned(),
                fingerprint: [82; 32],
            },
        )
        .await
        .expect("historical terminal response replays during later lease");
    assert!(replay.replayed);
    assert_eq!(replay.item.revision, reconciled.item.revision);
    assert_eq!(items.get(item_id).await.unwrap(), reopened.item);
}

async fn terminal_projection_holds_execution_state_before_start(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-race-terminal-owner",
        "execution-race-terminal-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let execution = Arc::new(ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock,
    ));
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Terminal projection wins Start race", 84).await;
    let item = items.get(item_id).await.expect("race item");
    ensure_execution_state(pool, scope).await;

    // Hold the canonical advisory item lock so terminal replacement pauses only after owning
    // execution_state. Start then queues behind state; releasing the item path lets terminal
    // commit first and Start must reject the now-stale item revision.
    let mut item_blocker = pool.begin().await.expect("begin advisory blocker");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .execute(&mut *item_blocker)
    .await
    .expect("hold canonical item advisory lock");
    let terminal_task = {
        let items = items.clone();
        let replacement = item_replacement(&item, ItemStatus::Completed);
        tokio::spawn(async move {
            items
                .replace(
                    item_id,
                    item.revision,
                    replacement,
                    ItemIdempotencyKey {
                        key: "execution-race-terminal-wins-001".to_owned(),
                        fingerprint: [85; 32],
                    },
                )
                .await
        })
    };
    wait_until_execution_state_is_locked(pool, scope).await;
    let session_id = Uuid::new_v4();
    let start_task = {
        let execution = execution.clone();
        tokio::spawn(async move {
            execution
                .command(
                    0,
                    start_command(session_id, item_id, Uuid::new_v4()),
                    execution_idempotency("execution-race-start-loses-001", 86),
                )
                .await
        })
    };
    item_blocker
        .commit()
        .await
        .expect("release canonical item advisory lock");
    let (terminal, start) = tokio::time::timeout(StdDuration::from_secs(10), async {
        tokio::join!(terminal_task, start_task)
    })
    .await
    .expect("item-first race completes without deadlock");
    let terminal = terminal
        .expect("terminal task joins")
        .expect("terminal projection wins");
    assert_eq!(terminal.item.status, ItemStatus::Completed);
    assert!(matches!(
        start.expect("Start task joins"),
        Err(ExecutionServiceError::Repository(
            ExecutionRepositoryError::ItemRevisionConflict
        ))
    ));
    assert!(execution.snapshot().await.unwrap().active_session.is_none());
    assert_eq!(
        execution_idempotency_count(pool, scope, "execution-race-start-loses-001").await,
        0
    );
}

#[allow(clippy::too_many_lines)] // One deterministic race plus retry, restore, and later exact replay.
async fn start_holds_execution_state_before_trash(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-race-trash-start-owner",
        "execution-race-trash-start-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let execution = Arc::new(ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    ));
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Start wins trash race", 89).await;
    let item = items.get(item_id).await.expect("trash race item");
    ensure_execution_state(pool, scope).await;

    let mut item_blocker = pool.begin().await.expect("begin trash item blocker");
    sqlx::query("SELECT id FROM items WHERE workspace_id = $1 ORDER BY id FOR UPDATE")
        .bind(scope.workspace_id)
        .fetch_all(&mut *item_blocker)
        .await
        .expect("hold canonical item rows before Start");
    let session_id = Uuid::new_v4();
    let start_task = {
        let execution = execution.clone();
        tokio::spawn(async move {
            execution
                .command(
                    0,
                    start_command(session_id, item_id, Uuid::new_v4()),
                    execution_idempotency("execution-race-trash-start-wins-001", 90),
                )
                .await
        })
    };
    wait_until_execution_state_is_locked(pool, scope).await;
    let trash_task = {
        let items = items.clone();
        tokio::spawn(async move {
            items
                .trash(
                    item_id,
                    item.revision,
                    ItemIdempotencyKey {
                        key: "execution-race-trash-loses-001".to_owned(),
                        fingerprint: [91; 32],
                    },
                )
                .await
        })
    };
    item_blocker
        .commit()
        .await
        .expect("release trash item blocker");
    let (started, blocked) = tokio::time::timeout(StdDuration::from_secs(10), async {
        tokio::join!(start_task, trash_task)
    })
    .await
    .expect("Start/trash race completes without deadlock");
    assert_eq!(
        started
            .expect("Start task joins")
            .expect("Start wins trash race")
            .changed_session
            .id,
        session_id
    );
    assert!(matches!(
        blocked.expect("trash task joins"),
        Err(ItemServiceError::Repository(
            ItemRepositoryError::ActiveExecutionConflict {
                item_id: conflict_item,
                session_id: conflict_session,
            }
        )) if conflict_item == item_id && conflict_session == session_id
    ));
    assert_eq!(
        item_idempotency_count(
            pool,
            scope,
            "items.delete",
            "execution-race-trash-loses-001",
        )
        .await,
        0
    );

    clock.set(base + Duration::seconds(1));
    execution
        .command(
            1,
            ExecutionCommand::Complete(FinishExecution {
                session_id,
                actual_seconds: Some(1),
            }),
            execution_idempotency("execution-race-trash-close-001", 92),
        )
        .await
        .expect("close lease before trash retry");
    let trashed = items
        .trash(
            item_id,
            item.revision,
            ItemIdempotencyKey {
                key: "execution-race-trash-loses-001".to_owned(),
                fingerprint: [91; 32],
            },
        )
        .await
        .expect("trash retries after lease closes");
    assert!(!trashed.replayed);
    let restored = items
        .restore(
            item_id,
            trashed.item.revision,
            ItemIdempotencyKey {
                key: "execution-race-trash-restore-001".to_owned(),
                fingerprint: [93; 32],
            },
        )
        .await
        .expect("restore for later replay");
    let later_session_id = Uuid::new_v4();
    execution
        .command(
            2,
            ExecutionCommand::Start(StartExecution {
                session_id: later_session_id,
                item_id,
                item_revision: restored.item.revision,
                occurrence_id: None,
                session_index: 1,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-race-trash-later-start-001", 94),
        )
        .await
        .expect("later lease opens after restore");
    let replay = items
        .trash(
            item_id,
            item.revision,
            ItemIdempotencyKey {
                key: "execution-race-trash-loses-001".to_owned(),
                fingerprint: [91; 32],
            },
        )
        .await
        .expect("historical trash response replays during later lease");
    assert!(replay.replayed);
    assert_eq!(replay.item, trashed.item);
    assert_eq!(items.get(item_id).await.unwrap(), restored.item);
}

async fn trash_holds_execution_state_before_start(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-race-trash-owner",
        "execution-race-trash-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let execution = Arc::new(ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock,
    ));
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Trash wins Start race", 95).await;
    let item = items.get(item_id).await.expect("trash race item");
    ensure_execution_state(pool, scope).await;

    let mut item_blocker = pool.begin().await.expect("begin trash advisory blocker");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .execute(&mut *item_blocker)
    .await
    .expect("hold canonical item advisory lock");
    let trash_task = {
        let items = items.clone();
        tokio::spawn(async move {
            items
                .trash(
                    item_id,
                    item.revision,
                    ItemIdempotencyKey {
                        key: "execution-race-trash-wins-001".to_owned(),
                        fingerprint: [96; 32],
                    },
                )
                .await
        })
    };
    wait_until_execution_state_is_locked(pool, scope).await;
    let session_id = Uuid::new_v4();
    let start_task = {
        let execution = execution.clone();
        tokio::spawn(async move {
            execution
                .command(
                    0,
                    start_command(session_id, item_id, Uuid::new_v4()),
                    execution_idempotency("execution-race-trash-start-loses-001", 97),
                )
                .await
        })
    };
    item_blocker
        .commit()
        .await
        .expect("release trash advisory blocker");
    let (trashed, start) = tokio::time::timeout(StdDuration::from_secs(10), async {
        tokio::join!(trash_task, start_task)
    })
    .await
    .expect("trash/Start race completes without deadlock");
    let trashed = trashed
        .expect("trash task joins")
        .expect("trash wins Start race");
    assert!(trashed.item.deleted_at.is_some());
    assert!(matches!(
        start.expect("Start task joins"),
        Err(ExecutionServiceError::Repository(
            ExecutionRepositoryError::ItemRevisionConflict
        ))
    ));
    assert!(execution.snapshot().await.unwrap().active_session.is_none());
    assert_eq!(
        execution_idempotency_count(pool, scope, "execution-race-trash-start-loses-001").await,
        0
    );
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
    let item_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, notes, \
         timezone_name, duration_seconds, scheduling_constraints, importance, urgency, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, 'task', 'planned', 'Legacy clock task', \
         'PostgreSQL execution integration fixture', 'Europe/Madrid', 3600, \
         '{\"energy\":\"deep\"}'::jsonb, 80, 60, $4, $4)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(latest)
    .execute(&pool)
    .await
    .expect("insert pre-structural legacy task without using the current repository");
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

    pool.execute(include_str!(
        "../migrations/0019_schedule_deferred_placements.sql"
    ))
    .await
    .expect("deferred placement migration applies");
    pool.execute(include_str!(
        "../migrations/0020_execution_progress_ledger.sql"
    ))
    .await
    .expect("execution progress ledger migration applies");
    for migration in [
        include_str!("../migrations/0021_execution_defer_approval.sql"),
        include_str!("../migrations/0022_google_schedule_publication.sql"),
        include_str!("../migrations/0023_google_task_provider_metadata.sql"),
        include_str!("../migrations/0024_structural_item_fields.sql"),
        include_str!("../migrations/0025_authoritative_dependency_graph.sql"),
        include_str!("../migrations/0026_habit_occurrence_ledger.sql"),
    ] {
        pool.execute(migration)
            .await
            .expect("post-ledger migration applies");
    }

    clock.set(legacy_active_start + Duration::seconds(10));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items,
        clock.clone(),
    );
    let completed = execution
        .command(
            3,
            ExecutionCommand::Complete(FinishExecution {
                session_id: legacy_active_session,
                actual_seconds: None,
            }),
            execution_idempotency("execution-upgrade-complete-001", 41),
        )
        .await
        .expect("post-upgrade legacy completion");
    assert_eq!(completed.changed_session.accumulated_seconds, 10);
    assert_eq!(
        completed.changed_session.updated_at,
        latest + Duration::microseconds(1)
    );

    let newer_session = Uuid::from_u128(3_000);
    let started = execution
        .command(
            4,
            ExecutionCommand::Start(StartExecution {
                session_id: newer_session,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 2,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
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
    assert_occurrence_scoped_physical_indices(&pool).await;
    assert_physical_index_reuse_is_cross_revision_and_epoch(&pool).await;
    assert_claim_allocation_preserves_published_split_high_water(&pool).await;
}

#[allow(clippy::too_many_lines)] // Both recurrence occurrences share one durable-policy lifecycle.
async fn assert_occurrence_scoped_physical_indices(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-occurrence-owner",
        "execution-occurrence-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Occurrence-scoped execution", 116).await;
    let item = items
        .get(item_id)
        .await
        .expect("load recurring fixture item");
    let mut recurring = item_replacement(&item, item.status);
    recurring.recurrence = Some(json!({"type": "daily", "times_per_day": 2}));
    let recurring = items
        .replace(
            item_id,
            item.revision,
            recurring,
            ItemIdempotencyKey {
                key: "execution-occurrence-recurrence-001".to_owned(),
                fingerprint: [195; 32],
            },
        )
        .await
        .expect("make fixture item recur twice per day")
        .item;
    let _ = publish_v5_defer_policy(
        pool,
        scope,
        "execution-occurrence-owner",
        &items,
        &schedules,
        base,
        item_id,
        recurring.revision,
    )
    .await;
    let source_blocks: Vec<(Uuid, Uuid, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT block.source_block_id, \
                (block.constraint_snapshot ->> 'occurrence_id')::uuid, \
                (block.constraint_snapshot ->> 'session_index')::integer, block.starts_at \
           FROM schedule_blocks AS block \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = block.workspace_id \
            AND revision.id = block.schedule_revision_id \
          WHERE revision.workspace_id = $1 AND revision.state = 'published' \
            AND block.item_id = $2 ORDER BY block.ordinal",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .fetch_all(pool)
    .await
    .expect("load exact recurring source blocks");
    let source_blocks = source_blocks.into_iter().take(2).collect::<Vec<_>>();
    assert_eq!(source_blocks.len(), 2);

    for (ordinal, (source_block_id, occurrence_id, session_index, source_start)) in
        source_blocks.iter().copied().enumerate()
    {
        let expected_revision = u64::try_from(ordinal * 3).expect("fixture revision fits u64");
        let session_id = Uuid::new_v4();
        execution
            .command(
                expected_revision,
                ExecutionCommand::Start(StartExecution {
                    session_id,
                    item_id,
                    item_revision: recurring.revision,
                    occurrence_id: Some(occurrence_id),
                    session_index: u16::try_from(session_index).expect("session index fits u16"),
                    planned_block_id: Some(source_block_id),
                    device_id: Uuid::new_v4(),
                }),
                execution_idempotency(
                    &format!("execution-occurrence-start-{ordinal:03}"),
                    u8::try_from(117 + ordinal).expect("fixture marker fits u8"),
                ),
            )
            .await
            .expect("Start occurrence-scoped execution");
        clock.set(base + Duration::seconds(i64::try_from(ordinal + 1).unwrap()));
        execution
            .command(
                expected_revision + 1,
                ExecutionCommand::Pause(PauseExecution {
                    session_id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: Some("Assess one recurrence occurrence".to_owned()),
                }),
                execution_idempotency(
                    &format!("execution-occurrence-pause-{ordinal:03}"),
                    u8::try_from(197 + ordinal).expect("fixture marker fits u8"),
                ),
            )
            .await
            .expect("Pause occurrence-scoped execution");
        // Reuse the published start as a known-valid target inside this exact
        // occurrence window. A fixed offset can cross a clipped first window
        // when the fixture runs late in the workspace-local day.
        let move_start = source_start;
        let assessment = execution
            .assess_defer(DeferAssessmentRequest {
                expected_revision: expected_revision + 2,
                session_id,
                move_start,
                actual_seconds: Some(0),
            })
            .await
            .expect("Assess occurrence-scoped execution");
        execution
            .command(
                expected_revision + 2,
                ExecutionCommand::Defer(DeferExecution {
                    session_id,
                    move_start,
                    move_end: assessment.move_end,
                    actual_seconds: Some(0),
                    assessment_digest: Some(assessment.assessment_digest.clone()),
                    approved_assessment_digest: assessment
                        .approval_required
                        .then_some(assessment.assessment_digest),
                }),
                execution_idempotency(
                    &format!("execution-occurrence-defer-{ordinal:03}"),
                    u8::try_from(119 + ordinal).expect("fixture marker fits u8"),
                ),
            )
            .await
            .expect("Defer occurrence-scoped execution");
    }

    let claims: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT occurrence_id, replacement_session_index \
         FROM execution_defer_replacement_claims WHERE workspace_id = $1 \
         ORDER BY occurrence_id",
    )
    .bind(scope.workspace_id)
    .fetch_all(pool)
    .await
    .expect("load occurrence-scoped claims");
    assert_eq!(claims.len(), 2);
    assert!(claims.iter().all(|(_, index)| *index == 1));
    let expected_occurrences = source_blocks
        .iter()
        .map(|(_, occurrence_id, _, _)| *occurrence_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        claims
            .iter()
            .map(|(occurrence, _)| *occurrence)
            .collect::<std::collections::BTreeSet<_>>(),
        expected_occurrences
    );
}

#[allow(clippy::too_many_lines)] // One lifecycle proves permanent physical ownership across both revision dimensions.
async fn assert_physical_index_reuse_is_cross_revision_and_epoch(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-physical-reuse-owner",
        "execution-physical-reuse-workspace",
    )
    .await;
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
    let item_id = Uuid::new_v4();
    let first_session = Uuid::new_v4();
    create_item(&items, item_id, "Cross-revision physical index", 121).await;
    execution
        .command(
            0,
            start_command(first_session, item_id, Uuid::new_v4()),
            execution_idempotency("execution-physical-reuse-first-start-001", 122),
        )
        .await
        .expect("Start first physical index");
    clock.set(base + Duration::seconds(1));
    execution
        .command(
            1,
            ExecutionCommand::Complete(FinishExecution {
                session_id: first_session,
                actual_seconds: Some(1),
            }),
            execution_idempotency("execution-physical-reuse-complete-001", 123),
        )
        .await
        .expect("complete first physical index");

    let item = items.get(item_id).await.expect("load physical reuse item");
    let mut replacement = item_replacement(&item, item.status);
    "Cross-revision physical index edited".clone_into(&mut replacement.title);
    let edited = items
        .replace(
            item_id,
            item.revision,
            replacement,
            ItemIdempotencyKey {
                key: "execution-physical-reuse-edit-001".to_owned(),
                fingerprint: [124; 32],
            },
        )
        .await
        .expect("benign item revision advances");
    let revision_reuse_key = "execution-physical-reuse-revision-001";
    let revision_reuse = execution
        .command(
            2,
            ExecutionCommand::Start(StartExecution {
                session_id: Uuid::new_v4(),
                item_id,
                item_revision: edited.item.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency(revision_reuse_key, 125),
        )
        .await
        .expect_err("physical index cannot be reused after benign revision");
    assert!(matches!(
        revision_reuse,
        ExecutionServiceError::Repository(ExecutionRepositoryError::ScheduleStale)
    ));
    assert_eq!(
        execution_idempotency_count(pool, scope, revision_reuse_key).await,
        0
    );

    sqlx::query(
        "UPDATE items SET revision = revision + 1, execution_epoch = execution_epoch + 1, \
         updated_at = $3 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(base + Duration::seconds(2))
    .execute(pool)
    .await
    .expect("simulate explicit progress epoch reset");
    let epoch_reuse = raw_active_start_with_epoch(
        pool,
        scope,
        item_id,
        i64::try_from(edited.item.revision + 1).expect("item revision fits bigint"),
        2,
        None,
        0,
        None,
        base + Duration::seconds(3),
    )
    .await;
    assert!(
        epoch_reuse.is_err(),
        "raw Start cannot reuse an old-epoch index"
    );
}

#[allow(clippy::too_many_lines)] // The published split fixture proves the repository allocation fence.
async fn assert_claim_allocation_preserves_published_split_high_water(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-published-high-water-owner",
        "execution-published-high-water-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    create_item(&items, item_id, "Published split high-water", 126).await;
    let item = items.get(item_id).await.expect("load split fixture item");
    let mut split = item_replacement(&item, item.status);
    split.split_policy = SplitPolicy::Splittable {
        minimum_chunk_seconds: 1_200,
        maximum_chunk_seconds: 1_200,
    };
    let split = items
        .replace(
            item_id,
            item.revision,
            split,
            ItemIdempotencyKey {
                key: "execution-published-high-water-split-001".to_owned(),
                fingerprint: [196; 32],
            },
        )
        .await
        .expect("make high-water fixture exactly three-way splittable")
        .item;
    let (source_block_id, move_start) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-published-high-water-owner",
        &items,
        &schedules,
        base,
        item_id,
        split.revision,
    )
    .await;
    let published_indices: Vec<i32> = sqlx::query_scalar(
        "SELECT (block.constraint_snapshot ->> 'session_index')::integer \
           FROM schedule_blocks AS block \
           JOIN schedule_revisions AS revision \
             ON revision.workspace_id = block.workspace_id \
            AND revision.id = block.schedule_revision_id \
          WHERE revision.workspace_id = $1 AND revision.state = 'published' \
            AND block.item_id = $2 ORDER BY block.ordinal",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .fetch_all(pool)
    .await
    .expect("load published split indices");
    assert_eq!(published_indices, vec![0, 1, 2]);

    let session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: split.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-published-high-water-start-001", 127),
        )
        .await
        .expect("Start first published split block");
    clock.set(base + Duration::seconds(1));
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess split high-water allocation".to_owned()),
            }),
            execution_idempotency("execution-published-high-water-pause-001", 199),
        )
        .await
        .expect("pause first published split block");
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start,
            actual_seconds: Some(0),
        })
        .await
        .expect("assess zero-credit split defer");
    execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(0),
                assessment_digest: Some(assessment.assessment_digest.clone()),
                approved_assessment_digest: assessment
                    .approval_required
                    .then_some(assessment.assessment_digest),
            }),
            execution_idempotency("execution-published-high-water-defer-001", 128),
        )
        .await
        .expect("zero-credit attested Defer keeps exact remaining window");
    let claim: (i32, i64, i64) = sqlx::query_as(
        "SELECT replacement_session_index, consumed_by_source_seconds, \
         remaining_duration_seconds FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("load published high-water replacement claim");
    assert_eq!(claim, (3, 0, 20 * 60));
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
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let first_item = Uuid::new_v4();
    let second_item = Uuid::new_v4();
    create_item(&items, first_item, "Defer active task", 31).await;
    create_item(&items, second_item, "Defer paused task", 32).await;
    let (first_source_block, first_move_start) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-defer-owner",
        &items,
        &schedules,
        base,
        first_item,
        1,
    )
    .await;
    let second_source_block = current_published_source_block(pool, scope, second_item).await;

    let first_session = Uuid::from_u128(2_000);
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id: first_session,
                item_id: first_item,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(first_source_block),
                device_id: Uuid::new_v4(),
            }),
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
                assessment_digest: None,
                approved_assessment_digest: None,
            }),
            execution_idempotency(INVALID_KEY, 34),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        invalid,
        ExecutionServiceError::Domain(ExecutionDomainError::InvalidDefer)
    ));

    let stale_defer = ExecutionCommand::Defer(DeferExecution {
        session_id: first_session,
        move_start: first_move_start,
        move_end: first_move_start + Duration::hours(1),
        actual_seconds: Some(45),
        assessment_digest: None,
        approved_assessment_digest: None,
    });
    let stale = execution
        .command(0, stale_defer, execution_idempotency(STALE_DEFER_KEY, 35))
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    ));

    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id: first_session,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess the exact remainder".to_owned()),
            }),
            execution_idempotency("execution-defer-first-pause-001", 192),
        )
        .await
        .expect("pause first defer fixture");
    let first_assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id: first_session,
            move_start: first_move_start,
            actual_seconds: None,
        })
        .await
        .expect("assess first exact defer");
    let first_move_end = first_assessment.move_end;
    let defer_active = ExecutionCommand::Defer(DeferExecution {
        session_id: first_session,
        move_start: first_move_start,
        move_end: first_move_end,
        actual_seconds: Some(first_assessment.actual_seconds),
        assessment_digest: Some(first_assessment.assessment_digest.clone()),
        approved_assessment_digest: first_assessment
            .approval_required
            .then_some(first_assessment.assessment_digest),
    });
    let deferred = execution
        .command(
            2,
            defer_active.clone(),
            execution_idempotency(DEFER_KEY, 36),
        )
        .await
        .unwrap();
    assert_eq!(deferred.revision, 3);
    assert!(deferred.active_session.is_none());
    assert_eq!(deferred.changed_session.status, ExecutionStatus::Deferred);
    assert_eq!(deferred.changed_session.accumulated_seconds, 45);
    assert_eq!(deferred.changed_session.actual_seconds, Some(45));
    assert_eq!(deferred.changed_session.move_start, Some(first_move_start));
    assert_eq!(deferred.changed_session.move_end, Some(first_move_end));
    assert_eq!(open_session_count(pool, scope).await, 0);
    let first_claim: (i32, i64, String, i64, i64, i64) = sqlx::query_as(
        "SELECT replacement_session_index, execution_epoch, planned_duration_source, \
         planned_duration_seconds, consumed_by_source_seconds, remaining_duration_seconds \
         FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session)
    .fetch_one(pool)
    .await
    .expect("first Defer replacement claim");
    assert_eq!(
        first_claim,
        (1, 1, "published_origin".to_owned(), 3600, 60, 3540)
    );

    clock.set(first_move_end + Duration::seconds(1));
    let replay = execution
        .command(2, defer_active, execution_idempotency(DEFER_KEY, 36))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.revision, 3);
    assert_eq!(replay.changed_session, deferred.changed_session);
    let replayed_claim_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session)
    .fetch_one(pool)
    .await
    .expect("count replayed Defer claim");
    assert_eq!(replayed_claim_count, 1);

    // A later session keeps causal order even if the host clock rolls backward and its UUID sorts
    // before the older terminal row.
    let rollback_start = base - Duration::days(30);
    clock.set(rollback_start);
    let second_session = Uuid::from_u128(1_000);
    let started_second = execution
        .command(
            3,
            ExecutionCommand::Start(StartExecution {
                session_id: second_session,
                item_id: second_item,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(second_source_block),
                device_id: Uuid::new_v4(),
            }),
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
            4,
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
    let second_move_start = first_move_start + Duration::hours(2);
    let second_assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 5,
            session_id: second_session,
            move_start: second_move_start,
            actual_seconds: Some(7),
        })
        .await
        .expect("assess paused exact defer after host-clock rollback");
    let second_move_end = second_assessment.move_end;
    let deferred_paused = execution
        .command(
            5,
            ExecutionCommand::Defer(DeferExecution {
                session_id: second_session,
                move_start: second_move_start,
                move_end: second_move_end,
                actual_seconds: Some(7),
                assessment_digest: Some(second_assessment.assessment_digest.clone()),
                approved_assessment_digest: second_assessment
                    .approval_required
                    .then_some(second_assessment.assessment_digest),
            }),
            execution_idempotency("execution-defer-paused-001", 39),
        )
        .await
        .unwrap();
    assert_eq!(deferred_paused.revision, 6);
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
    let replacement_indices: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT item_id, replacement_session_index \
         FROM execution_defer_replacement_claims WHERE workspace_id = $1 ORDER BY item_id",
    )
    .bind(scope.workspace_id)
    .fetch_all(pool)
    .await
    .expect("load independent replacement indices");
    assert_eq!(replacement_indices.len(), 2);
    assert!(
        replacement_indices
            .iter()
            .all(|(_, replacement_index)| *replacement_index == 1),
        "different work units independently allocate the same fresh physical index"
    );

    let claimed_source_mutation = sqlx::query(
        "UPDATE execution_sessions SET pause_reason = 'tampered' \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session)
    .execute(pool)
    .await
    .expect_err("claimed deferred source is immutable");
    assert!(
        claimed_source_mutation
            .to_string()
            .contains("claimed deferred execution sessions are immutable")
    );

    let epoch_before: i64 =
        sqlx::query_scalar("SELECT execution_epoch FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(scope.workspace_id)
            .bind(first_item)
            .fetch_one(pool)
            .await
            .expect("load item execution epoch");
    let epoch_after: i64 = sqlx::query_scalar(
        "UPDATE items SET title = title || ' revised', revision = revision + 1, \
         updated_at = updated_at + interval '1 microsecond' \
         WHERE workspace_id = $1 AND id = $2 RETURNING execution_epoch",
    )
    .bind(scope.workspace_id)
    .bind(first_item)
    .fetch_one(pool)
    .await
    .expect("apply benign item revision");
    assert_eq!(epoch_after, epoch_before);

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
    assert_execution_side_effects(pool, scope, 6).await;
    for failed_key in [INVALID_KEY, STALE_DEFER_KEY] {
        assert_eq!(
            execution_idempotency_count(pool, scope, failed_key).await,
            0,
            "failed defer leaked idempotency reservation for {failed_key}"
        );
    }

    assert_concurrent_defer_allocates_one_claim(pool).await;
    assert_index_exhaustion_rolls_back(pool).await;

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

#[allow(clippy::too_many_lines)] // The race requires a complete assessed source fixture.
async fn assert_concurrent_defer_allocates_one_claim(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-defer-race-owner",
        "execution-defer-race-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    create_item(&items, item_id, "Concurrent defer claim", 103).await;
    let (source_block_id, move_start) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-defer-race-owner",
        &items,
        &schedules,
        base,
        item_id,
        1,
    )
    .await;
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-defer-race-start-001", 104),
        )
        .await
        .expect("start concurrent defer fixture");
    clock.set(base + Duration::seconds(5));
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Race exact approval".to_owned()),
            }),
            execution_idempotency("execution-defer-race-pause-001", 193),
        )
        .await
        .expect("pause concurrent defer fixture");
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start,
            actual_seconds: Some(5),
        })
        .await
        .expect("assess concurrent defer fixture");
    let command = ExecutionCommand::Defer(DeferExecution {
        session_id,
        move_start,
        move_end: assessment.move_end,
        actual_seconds: Some(5),
        assessment_digest: Some(assessment.assessment_digest.clone()),
        approved_assessment_digest: assessment
            .approval_required
            .then_some(assessment.assessment_digest),
    });
    let (left, right) = tokio::join!(
        execution.command(
            2,
            command.clone(),
            execution_idempotency("execution-defer-race-left-001", 105),
        ),
        execution.command(
            2,
            command,
            execution_idempotency("execution-defer-race-right-001", 106),
        )
    );
    match (left, right) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => {
            assert_eq!(winner.changed_session.status, ExecutionStatus::Deferred);
            assert!(matches!(
                loser,
                ExecutionServiceError::Repository(ExecutionRepositoryError::RevisionConflict {
                    expected: 2,
                    actual: 3
                })
            ));
        }
        (left, right) => panic!("exactly one concurrent Defer must win: {left:?}, {right:?}"),
    }
    let claims: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("count concurrent Defer claims");
    assert_eq!(claims, 1);
}

#[allow(clippy::too_many_lines)] // The exhaustion guard needs real published execution evidence.
async fn assert_index_exhaustion_rolls_back(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-index-exhausted-owner",
        "execution-index-exhausted-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    create_item(&items, item_id, "Exhaust replacement index", 107).await;
    let (source_block_id, move_start) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-index-exhausted-owner",
        &items,
        &schedules,
        base,
        item_id,
        1,
    )
    .await;
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-index-exhausted-start-001", 108),
        )
        .await
        .expect("start attested index-exhaustion fixture");
    clock.set(base + Duration::seconds(5));
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess an exhausted replacement index".to_owned()),
            }),
            execution_idempotency("execution-index-exhausted-pause-001", 194),
        )
        .await
        .expect("pause index-exhaustion fixture");
    insert_terminal_history_row(
        pool,
        scope,
        item_id,
        None,
        i32::from(u16::MAX),
        base - Duration::seconds(1),
    )
    .await;
    let exhausted = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start,
            actual_seconds: Some(5),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        exhausted,
        ExecutionServiceError::Repository(ExecutionRepositoryError::IndexExhausted)
    ));
    let snapshot = execution
        .snapshot()
        .await
        .expect("snapshot after exhaustion");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(
        snapshot.active_session.as_ref().map(|session| session.id),
        Some(session_id)
    );
    assert_eq!(
        snapshot.active_session.map(|session| session.status),
        Some(ExecutionStatus::Paused)
    );
    let claims: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_defer_replacement_claims WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("count exhausted claims");
    assert_eq!(claims, 0);
    assert_eq!(execution_outbox_count(pool, scope).await, 2);
}

#[allow(clippy::too_many_lines)] // One schedule-origin fixture proves the full Start-to-Defer evidence chain.
async fn assert_attested_defer_uses_origin_duration(pool: &PgPool) {
    let scope = seed_scope(
        pool,
        "execution-origin-defer-owner",
        "execution-origin-defer-workspace",
    )
    .await;
    let base = postgres_now(pool).await;
    let clock = Arc::new(TestClock::new(base));
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        clock.clone(),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(pool.clone(), scope));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(pool.clone(), scope)),
        items.clone(),
        clock.clone(),
    );
    let item_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let fully_consumed_item_id = Uuid::new_v4();
    let fully_consumed_session_id = Uuid::new_v4();
    create_item(&items, item_id, "Attested defer duration", 110).await;
    create_item(
        &items,
        fully_consumed_item_id,
        "Fully consumed attested defer",
        130,
    )
    .await;
    let (source_block_id, move_start) = publish_v5_defer_policy(
        pool,
        scope,
        "execution-origin-defer-owner",
        &items,
        &schedules,
        base,
        item_id,
        1,
    )
    .await;
    let fully_consumed_source_block_id =
        current_published_source_block(pool, scope, fully_consumed_item_id).await;

    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id,
                item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-origin-defer-start-001", 111),
        )
        .await
        .expect("Start records current published origin");
    let origin: (i64, i64) = sqlx::query_as(
        "SELECT execution_epoch, planned_duration_seconds \
         FROM execution_session_schedule_origins \
         WHERE workspace_id = $1 AND execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("load Start schedule origin");
    assert_eq!(origin, (1, 3600));

    clock.set(base + Duration::seconds(600));
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess published-origin duration".to_owned()),
            }),
            execution_idempotency("execution-origin-defer-pause-001", 200),
        )
        .await
        .expect("pause published-origin duration fixture");
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id,
            move_start,
            actual_seconds: Some(600),
        })
        .await
        .expect("assess exact published-origin remainder");
    assert_eq!(assessment.planned_duration_seconds, 3_600);
    assert_eq!(assessment.credited_source_seconds, 600);
    assert_eq!(assessment.remaining_duration_seconds, 3_000);
    let mismatch_key = "execution-origin-defer-duration-mismatch-001";
    let mismatch = execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start,
                move_end: assessment.move_end - Duration::seconds(1),
                actual_seconds: Some(600),
                assessment_digest: Some(assessment.assessment_digest.clone()),
                approved_assessment_digest: assessment
                    .approval_required
                    .then_some(assessment.assessment_digest.clone()),
            }),
            execution_idempotency(mismatch_key, 131),
        )
        .await
        .expect_err("assessment authorization binds the exact remaining window");
    assert!(matches!(
        mismatch,
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferAssessmentStale)
    ));
    assert_eq!(
        execution_idempotency_count(pool, scope, mismatch_key).await,
        0
    );
    assert_eq!(
        execution
            .snapshot()
            .await
            .unwrap()
            .active_session
            .map(|active| active.id),
        Some(session_id)
    );
    let mismatched_claims: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("count rolled-back mismatched claims");
    assert_eq!(mismatched_claims, 0);
    execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(600),
                assessment_digest: Some(assessment.assessment_digest.clone()),
                approved_assessment_digest: assessment
                    .approval_required
                    .then_some(assessment.assessment_digest),
            }),
            execution_idempotency("execution-origin-defer-command-001", 112),
        )
        .await
        .expect("attested Defer records replacement claim");
    let claim: (String, i64, i64, i64, i32, i64) = sqlx::query_as(
        "SELECT planned_duration_source, planned_duration_seconds, \
         consumed_by_source_seconds, remaining_duration_seconds, \
         replacement_session_index, execution_epoch \
         FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("load attested Defer claim");
    assert_eq!(
        claim,
        ("published_origin".to_owned(), 3600, 600, 3000, 1, 1)
    );

    execution
        .command(
            3,
            ExecutionCommand::Start(StartExecution {
                session_id: fully_consumed_session_id,
                item_id: fully_consumed_item_id,
                item_revision: 1,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(fully_consumed_source_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("execution-origin-fully-consumed-start-001", 132),
        )
        .await
        .expect("Start fully consumed attested fixture");
    clock.set(base + Duration::seconds(1_800));
    execution
        .command(
            4,
            ExecutionCommand::Pause(PauseExecution {
                session_id: fully_consumed_session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("No remainder should be deferrable".to_owned()),
            }),
            execution_idempotency("execution-origin-fully-consumed-pause-001", 201),
        )
        .await
        .expect("pause fully consumed attested fixture");
    let fully_consumed = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 5,
            session_id: fully_consumed_session_id,
            move_start: move_start + Duration::hours(2),
            actual_seconds: Some(3_600),
        })
        .await
        .expect_err("fully consumed attested block has no defer replacement");
    assert!(matches!(
        fully_consumed,
        ExecutionServiceError::Repository(ExecutionRepositoryError::DeferDurationConflict)
    ));
    let fully_consumed_claims: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(fully_consumed_session_id)
    .fetch_one(pool)
    .await
    .expect("count fully consumed rolled-back claims");
    assert_eq!(fully_consumed_claims, 0);
    execution
        .command(
            5,
            ExecutionCommand::Complete(FinishExecution {
                session_id: fully_consumed_session_id,
                actual_seconds: Some(3_600),
            }),
            execution_idempotency("execution-origin-fully-consumed-complete-001", 134),
        )
        .await
        .expect("close fully consumed rollback fixture");

    let immutable_origin = sqlx::query(
        "UPDATE execution_session_schedule_origins SET created_at = created_at \
         WHERE workspace_id = $1 AND execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .execute(pool)
    .await
    .expect_err("schedule origin is immutable");
    assert!(
        immutable_origin
            .to_string()
            .contains("execution schedule origins are immutable")
    );
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
            execution_item(id, title, None),
            ItemIdempotencyKey {
                key: format!("execution-item-{marker:03}"),
                fingerprint: [marker; 32],
            },
        )
        .await
        .unwrap();
}

fn execution_item(id: Uuid, title: &str, parent_id: Option<Uuid>) -> NewItem {
    NewItem {
        id,
        is_sensitive: false,
        kind: ItemKind::Task,
        status: ItemStatus::Planned,
        title: title.to_owned(),
        notes: Some("PostgreSQL execution integration fixture".to_owned()),
        timezone_name: "Europe/Madrid".to_owned(),
        duration_kind: None,
        duration_seconds: Some(3600),
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: None,
        deadline_date: None,
        deadline_at: None,
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: json!({"energy": "deep"}),
        has_own_effort: None,
        split_policy: SplitPolicy::Indivisible,
        importance: 80,
        urgency: 60,
        parent_id,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

fn item_replacement(item: &dayweave_api::items::Item, status: ItemStatus) -> ReplaceItem {
    ReplaceItem {
        is_sensitive: item.is_sensitive,
        kind: item.kind,
        status,
        title: item.title.clone(),
        notes: item.notes.clone(),
        timezone_name: item.timezone_name.clone(),
        duration_kind: Some(item.duration_kind),
        duration_seconds: item.duration_seconds,
        duration_min_seconds: item.duration_min_seconds,
        duration_max_seconds: item.duration_max_seconds,
        duration_source: item.duration_source,
        deadline_kind: Some(item.deadline_kind),
        deadline_date: item.deadline_date,
        deadline_at: item.deadline_at,
        deadline_strength: item.deadline_strength,
        deadline_soft_weight: item.deadline_soft_weight,
        earliest_start_at: item.earliest_start_at,
        recurrence: item.recurrence.clone(),
        flexible_constraints: item.flexible_constraints.clone(),
        has_own_effort: Some(item.has_own_effort),
        split_policy: item.split_policy.clone(),
        importance: item.importance,
        urgency: item.urgency,
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
        blocked_reason_kind: item.blocked_reason_kind,
        blocked_by_item_id: item.blocked_by_item_id,
        blocked_reason: item.blocked_reason.clone(),
    }
}

async fn ensure_execution_state(pool: &PgPool, scope: DatabaseScope) {
    sqlx::query("INSERT INTO execution_state (workspace_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(scope.workspace_id)
        .execute(pool)
        .await
        .expect("materialize execution state mutex");
}

async fn wait_until_execution_state_is_locked(pool: &PgPool, scope: DatabaseScope) {
    tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let mut probe = pool.begin().await.expect("begin execution lock probe");
            let result = sqlx::query(
                "SELECT workspace_id FROM execution_state WHERE workspace_id = $1 FOR UPDATE NOWAIT",
            )
            .bind(scope.workspace_id)
            .fetch_one(&mut *probe)
            .await;
            match result {
                Ok(_) => {
                    probe.rollback().await.expect("release execution lock probe");
                    tokio::task::yield_now().await;
                }
                Err(error) if postgres_error_code(&error).as_deref() == Some("55P03") => {
                    probe.rollback().await.expect("rollback failed lock probe");
                    break;
                }
                Err(error) => panic!("unexpected execution lock probe failure: {error}"),
            }
        }
    })
    .await
    .expect("operation acquires execution_state before timeout");
}

fn postgres_error_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
}

async fn postgres_now(pool: &PgPool) -> DateTime<Utc> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .expect("read PostgreSQL fixture time");
    DateTime::from_timestamp_micros(now.timestamp_micros())
        .expect("PostgreSQL fixture time remains representable at microsecond precision")
}

fn truncate_to_minute(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(value.timestamp().div_euclid(60) * 60, 0)
        .expect("fixture minute remains representable")
}

fn align_up_to_five_minutes(value: DateTime<Utc>) -> DateTime<Utc> {
    let seconds = value.timestamp();
    let aligned = seconds
        .div_euclid(300)
        .checked_add(1)
        .and_then(|slot| slot.checked_mul(300))
        .expect("fixture grid remains representable");
    DateTime::from_timestamp(aligned, 0).expect("fixture grid timestamp remains representable")
}

async fn insert_short_lived_unapplied_assessment(
    pool: &PgPool,
    scope: DatabaseScope,
    source_digest: &[u8; 32],
) -> [u8; 32] {
    let assessment_id = Uuid::new_v4();
    let assessment_digest = [0x5a; 32];
    let inserted = sqlx::query(
        "INSERT INTO execution_defer_assessments (id, workspace_id, user_id, schema_version, \
           execution_state_revision, source_execution_session_id, \
           source_execution_session_revision, source_schedule_revision_id, source_block_id, \
           current_schedule_revision_id, current_schedule_revision_number, \
           current_publication_hash, item_id, source_item_revision, current_item_revision, \
           execution_epoch, occurrence_id, source_session_index, replacement_session_index, \
           planned_duration_seconds, credited_before_seconds, effective_actual_seconds, \
           credited_after_seconds, credited_source_seconds, remaining_duration_seconds, \
           scheduler_slot_seconds, target_start, target_end, environment_digest, \
           assessment_digest, approval_required, private_context, violations, assessed_at, \
           expires_at) \
         SELECT $4, workspace_id, user_id, schema_version, execution_state_revision, \
           source_execution_session_id, source_execution_session_revision, \
           source_schedule_revision_id, source_block_id, current_schedule_revision_id, \
           current_schedule_revision_number, current_publication_hash, item_id, \
           source_item_revision, current_item_revision, execution_epoch, occurrence_id, \
           source_session_index, replacement_session_index, planned_duration_seconds, \
           credited_before_seconds, effective_actual_seconds, credited_after_seconds, \
           credited_source_seconds, remaining_duration_seconds, scheduler_slot_seconds, \
           target_start, target_end, environment_digest, $5, approval_required, \
           jsonb_set(private_context, '{candidate,placement_id}', to_jsonb($4::text), false), \
           violations, statement_timestamp(), \
           statement_timestamp() + interval '250 milliseconds' \
         FROM execution_defer_assessments \
         WHERE workspace_id = $1 AND user_id = $2 AND assessment_digest = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(source_digest.as_slice())
    .bind(assessment_id)
    .bind(assessment_digest.as_slice())
    .execute(pool)
    .await
    .expect("insert a short-lived unapplied assessment fixture")
    .rows_affected();
    assert_eq!(inserted, 1);
    assessment_digest
}

fn decode_sha256_digest(value: &str) -> [u8; 32] {
    let hex = value
        .strip_prefix("sha256:")
        .expect("fixture digest has canonical prefix")
        .as_bytes();
    assert_eq!(hex.len(), 64);
    let mut output = [0_u8; 32];
    for (index, pair) in hex.chunks_exact(2).enumerate() {
        let nibble = |byte| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("fixture digest is lowercase hexadecimal"),
        };
        output[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    output
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

#[allow(clippy::too_many_lines)] // The fixture mirrors every immutable replacement-placement field.
async fn create_deferred_placement_draft(
    pool: &PgPool,
    scope: DatabaseScope,
    deferred: &ExecutionSession,
) -> (Uuid, Uuid, u16) {
    assert_eq!(deferred.status, ExecutionStatus::Deferred);
    let move_start = deferred.move_start.expect("deferred move start");
    let move_end = deferred.move_end.expect("deferred move end");
    let (replacement_session_index, remaining_duration_seconds, execution_epoch): (i32, i64, i64) =
        sqlx::query_as(
            "SELECT replacement_session_index, remaining_duration_seconds, execution_epoch \
             FROM execution_defer_replacement_claims WHERE workspace_id = $1 \
             AND source_deferred_session_id = $2 AND actionable",
        )
        .bind(scope.workspace_id)
        .bind(deferred.id)
        .fetch_one(pool)
        .await
        .expect("load actionable replacement claim");
    let replacement_session_index_u16 =
        u16::try_from(replacement_session_index).expect("replacement index fits u16");
    let (item_revision, current_execution_epoch): (i64, i64) = sqlx::query_as(
        "SELECT revision, execution_epoch FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(deferred.item_id)
    .fetch_one(pool)
    .await
    .expect("load current replacement item identity");
    assert_eq!(current_execution_epoch, execution_epoch);
    let parent_revision_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_optional(pool)
    .await
    .expect("load current schedule revision");
    let revision_number: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_number), 0) + 1 FROM schedule_revisions \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("allocate schedule revision number");
    let schedule_revision_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, \
         parent_revision_id, state, horizon_start, horizon_end, timezone_name, solver_version, \
         input_digest, created_by_user_id) VALUES ($1, $2, $3, $4, 'draft', $5, $6, \
         'Europe/Madrid', 'execution-attestation-test', $7, $8)",
    )
    .bind(schedule_revision_id)
    .bind(scope.workspace_id)
    .bind(revision_number)
    .bind(parent_revision_id)
    .bind(move_start)
    .bind(move_end)
    .bind(vec![u8::try_from(revision_number).unwrap_or(255); 32])
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("insert schedule draft");
    let mut result_snapshot = json!({
        "fixture": "deferred_start_attestation",
        "compose": {"source_item_revisions": {}}
    });
    let item_key = deferred.item_id.to_string();
    result_snapshot["compose"]["source_item_revisions"][item_key.as_str()] = json!(item_revision);
    sqlx::query(
        "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
         result_snapshot) VALUES ($1, $2, $3, $4)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(schedule_revision_id)
    .bind(result_snapshot)
    .execute(pool)
    .await
    .expect("insert schedule detail");

    let source_block_id = Uuid::new_v4();
    let block_evidence = json!({
        "source_block_id": source_block_id,
        "occurrence_id": deferred.occurrence_id,
        "session_index": replacement_session_index,
        "core_kind": "pinned"
    });
    sqlx::query(
        "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, schedule_revision_id, \
         item_id, block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal, \
         is_fixed, constraint_snapshot) VALUES ($1, $2, $3, $4, $5, 'pinned', \
         'Deferred execution placement', $6, $7, 'Europe/Madrid', 0, true, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(source_block_id)
    .bind(scope.workspace_id)
    .bind(schedule_revision_id)
    .bind(deferred.item_id)
    .bind(move_start)
    .bind(move_end)
    .bind(block_evidence)
    .execute(pool)
    .await
    .expect("insert exact pinned schedule block");
    sqlx::query(
        "INSERT INTO schedule_defer_replacement_placements (workspace_id, \
         schedule_revision_id, source_deferred_session_id, source_block_id, item_id, \
         item_revision, execution_epoch, occurrence_id, replacement_session_index, \
         remaining_duration_seconds, move_start, move_end) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(scope.workspace_id)
    .bind(schedule_revision_id)
    .bind(deferred.id)
    .bind(source_block_id)
    .bind(deferred.item_id)
    .bind(item_revision)
    .bind(execution_epoch)
    .bind(deferred.occurrence_id)
    .bind(replacement_session_index)
    .bind(remaining_duration_seconds)
    .bind(move_start)
    .bind(move_end)
    .execute(pool)
    .await
    .expect("insert defer replacement placement evidence");

    (
        schedule_revision_id,
        source_block_id,
        replacement_session_index_u16,
    )
}

async fn publish_schedule_revision(
    pool: &PgPool,
    scope: DatabaseScope,
    schedule_revision_id: Uuid,
) {
    let parent_revision_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT parent_revision_id FROM schedule_revisions \
         WHERE workspace_id = $1 AND id = $2 AND state = 'draft'",
    )
    .bind(scope.workspace_id)
    .bind(schedule_revision_id)
    .fetch_one(pool)
    .await
    .expect("load draft parent schedule revision");
    let published_at = postgres_now(pool).await;
    let mut transaction = pool.begin().await.expect("begin schedule seal");
    if let Some(parent_revision_id) = parent_revision_id {
        sqlx::query(
            "UPDATE schedule_revisions SET state = 'superseded', superseded_at = $3 \
             WHERE workspace_id = $1 AND id = $2 AND state = 'published'",
        )
        .bind(scope.workspace_id)
        .bind(parent_revision_id)
        .bind(published_at)
        .execute(&mut *transaction)
        .await
        .expect("supersede prior schedule revision");
    }
    sqlx::query(
        "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
         WHERE workspace_id = $1 AND id = $2 AND state = 'draft'",
    )
    .bind(scope.workspace_id)
    .bind(schedule_revision_id)
    .bind(published_at)
    .execute(&mut *transaction)
    .await
    .expect("publish deferred placement schedule");
    transaction.commit().await.expect("seal schedule");
}

async fn supersede_schedule_revision(pool: &PgPool, scope: DatabaseScope, revision_id: Uuid) {
    let updated = sqlx::query(
        "UPDATE schedule_revisions SET state = 'superseded', \
         superseded_at = GREATEST(published_at, clock_timestamp()) \
         WHERE workspace_id = $1 AND id = $2 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .execute(pool)
    .await
    .expect("supersede attested revision")
    .rows_affected();
    assert_eq!(updated, 1);
}

#[allow(clippy::too_many_arguments)] // Mirrors the exact raw execution row identity in trigger tests.
async fn raw_active_start(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    item_revision: i64,
    occurrence_id: Option<Uuid>,
    session_index: i32,
    planned_block_id: Option<Uuid>,
    started_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    raw_active_start_with_epoch(
        pool,
        scope,
        item_id,
        item_revision,
        1,
        occurrence_id,
        session_index,
        planned_block_id,
        started_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)] // Mirrors the exact raw execution row identity in trigger tests.
async fn raw_active_start_with_epoch(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    item_revision: i64,
    execution_epoch: i64,
    occurrence_id: Option<Uuid>,
    session_index: i32,
    planned_block_id: Option<Uuid>,
    started_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         execution_epoch, occurrence_id, session_index, planned_block_id, source_device_id, state, \
         revision, accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
         'active', 1, 0, NULL, $10, $10, $10, NULL, NULL, NULL, NULL, NULL, NULL, $10, $10)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(item_revision)
    .bind(execution_epoch)
    .bind(occurrence_id)
    .bind(session_index)
    .bind(planned_block_id)
    .bind(Uuid::new_v4())
    .bind(started_at)
    .execute(pool)
    .await
    .map(|_| ())
}

async fn insert_unrelated_terminal_history(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    occurrence_id: Option<Uuid>,
    base: DateTime<Utc>,
) {
    for offset in 0_i32..101 {
        insert_terminal_history_row(
            pool,
            scope,
            item_id,
            occurrence_id,
            1_000 + offset,
            base + Duration::minutes(1) + Duration::microseconds(i64::from(offset)),
        )
        .await;
    }
}

async fn insert_terminal_history_row(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    occurrence_id: Option<Uuid>,
    session_index: i32,
    updated_at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, occurrence_id, \
         session_index, planned_block_id, source_device_id, state, revision, accumulated_seconds, \
         actual_seconds, started_at, running_since, observed_running_since, paused_at, pause_until, \
         pause_reason, move_start, move_end, ended_at, created_at, updated_at) VALUES ($1, $2, $3, \
         1, $4, $5, NULL, $6, 'completed', 1, 0, 0, $7, NULL, NULL, NULL, NULL, NULL, NULL, NULL, \
         $7, $7, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(occurrence_id)
    .bind(session_index)
    .bind(Uuid::new_v4())
    .bind(updated_at)
    .execute(pool)
    .await
    .expect("insert unrelated terminal history");
}

async fn assert_failed_start_rolled_back(
    pool: &PgPool,
    scope: DatabaseScope,
    key: &str,
    expected_revision: i64,
    expected_sessions: i64,
    expected_outbox: i64,
) {
    assert_eq!(execution_idempotency_count(pool, scope, key).await, 0);
    let revision: i64 =
        sqlx::query_scalar("SELECT revision FROM execution_state WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .expect("load execution revision after failed Start");
    let sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM execution_sessions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .expect("count sessions after failed Start");
    assert_eq!(revision, expected_revision);
    assert_eq!(sessions, expected_sessions);
    assert_eq!(execution_outbox_count(pool, scope).await, expected_outbox);
}

async fn execution_outbox_count(pool: &PgPool, scope: DatabaseScope) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND aggregate_type = 'execution_session'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("count execution outbox messages")
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

async fn item_idempotency_count(
    pool: &PgPool,
    scope: DatabaseScope,
    namespace: &str,
    key: &str,
) -> i64 {
    let key_hash: [u8; 32] = Sha256::digest(key.as_bytes()).into();
    sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = $2 AND key_hash = $3",
    )
    .bind(scope.workspace_id)
    .bind(namespace)
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
