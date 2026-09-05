use std::{str::FromStr, sync::Arc, time::Duration as StdDuration};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use chrono::{DateTime, Duration, Timelike as _, Utc};
use dayweave_api::{
    AppState,
    auth::{Authenticator, RuntimeAuthenticator, Scope},
    config::AuthMode,
    credential_auth::{
        CredentialKind, CredentialRepository, DEVICE_CLIENT_CONTRACT_VERSION, DeviceClientKind,
        DeviceEnrollmentSpec, GeneratedCredential,
    },
    habits::{
        HabitAnalyticsBucket, HabitDeltaChange, HabitIdempotency, HabitIdempotencyKey,
        HabitMissedExplicitAction, HabitMissedReconcileCommand, HabitMissedResolutionAction,
        HabitMissedResolveCommand, HabitOccurrence, HabitOutcomeCommand, HabitOutcomeInput,
        HabitOutcomeStatus, HabitPauseResumeCommand, HabitPauseStartCommand, HabitRepository,
        HabitRepositoryError, HabitService,
    },
    http::router,
    items::{IdempotencyKey, ItemKind, ItemService, ItemStatus, NewItem, ReplaceItem, SplitPolicy},
    persistence::{
        DatabaseScope, MIGRATOR, PostgresCredentialRepository, PostgresHabitRepository,
        PostgresItemRepository,
    },
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
    scheduling::{
        ComposeScheduleError, ComposeScheduleRequest, PostgresSchedulingRepository,
        PublishScheduleSpec, ScheduleAccess, SchedulePublicationError, compose_canonical_schedule,
    },
};
use dayweave_core::{
    ItemId, OccurrenceId, OccurrenceState, RecurrenceException, RecurrenceExceptionAction,
    RecurrenceExceptionSelector, RecurrenceMoveSource,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions as _, Executor as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tower::ServiceExt as _;
use uuid::Uuid;

const PRIVATE_NOTE: &str = "SYNTHETIC-PRIVATE-POSTGRES-HABIT-NOTE";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn published_habit_evidence_drives_audited_cas_delta_pause_and_recomposition() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; habit PostgreSQL test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool, "habit-owner").await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    ));
    let habit_id = Uuid::new_v4();
    items
        .create(habit(habit_id), item_idempotency("habit-create", 1))
        .await
        .expect("create canonical habit");

    let schedules = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let access = ScheduleAccess {
        subject: "auth0|habit-owner".to_owned(),
        include_sensitive: true,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };
    let request = compose_request();
    let first_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose habit occurrence");
    assert_eq!(first_preview.plan.occurrences.len(), 1);
    let planner_occurrence_id = first_preview.plan.occurrences[0].id.0;
    let publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [2; 32],
                input_digest: digest_bytes(&first_preview.input_digest),
                timezone_name: request.timezone_name.clone(),
                manual_placement_approvals: Vec::new(),
                result: first_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish authoritative habit occurrence");

    let repository = Arc::new(PostgresHabitRepository::new(database.pool.clone(), scope));
    let service = Arc::new(HabitService::new(
        repository.clone(),
        items.clone(),
        Arc::new(SystemClock),
    ));
    let occurrences = service
        .list_occurrences(
            habit_id,
            "2025-10-26".parse().unwrap(),
            "2025-10-26".parse().unwrap(),
            None,
            100,
        )
        .await
        .expect("list published evidence")
        .occurrences;
    assert_eq!(occurrences.len(), 1);
    let evidence = &occurrences[0].evidence;
    assert_ne!(evidence.id, evidence.planner_occurrence_id);
    assert_eq!(evidence.planner_occurrence_id, planner_occurrence_id);
    assert_eq!(
        evidence.source_schedule_revision_id,
        publication.revision.id
    );
    assert_eq!(evidence.source_item_revision, 1);
    assert_eq!(evidence.policy_fingerprint.len(), 71);
    assert_eq!(evidence.local_date.to_string(), "2025-10-26");
    assert_eq!(evidence.timezone_name, "Europe/Paris");
    assert_eq!(evidence.expected_duration_seconds, Some(1_800));
    assert_eq!(evidence.expected_quantity, Some(20));
    assert_eq!(evidence.expected_unit.as_deref(), Some("pages"));
    let evidence_id = evidence.id;

    // A persisted habit makes the preview path authoritative, but it must not
    // erase lifecycle state for an unrelated recurring task. Exercise the
    // full PostgreSQL-backed composition path because occurrence IDs carry no
    // embedded owner and a global replacement regresses only in mixed graphs.
    let recurring_task_id = Uuid::new_v4();
    items
        .create(
            recurring_task(recurring_task_id),
            item_idempotency("recurring-task-create", 10),
        )
        .await
        .expect("create recurring task beside habit");
    let mixed_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose mixed recurring graph");
    let recurring_task_occurrence = mixed_preview
        .plan
        .occurrences
        .iter()
        .find(|occurrence| occurrence.series_item_id.0 == recurring_task_id)
        .expect("recurring task occurrence")
        .id;
    let mut completed_task_request = request.clone();
    completed_task_request
        .recurrence_context
        .completed_occurrence_ids
        .insert(recurring_task_occurrence);
    let completed_task_preview =
        compose_canonical_schedule(&items, &schedules, completed_task_request)
            .await
            .expect("compose caller-completed recurring task beside authoritative habit");
    assert!(
        completed_task_preview
            .plan
            .blocks
            .iter()
            .all(|block| block.occurrence_id != Some(recurring_task_occurrence)),
        "authoritative habit hydration must preserve non-habit completion state"
    );
    assert!(
        completed_task_preview
            .plan
            .blocks
            .iter()
            .any(|block| block.occurrence_id.map(|id| id.0) == Some(planner_occurrence_id)),
        "the adjacent unresolved habit must remain schedulable"
    );

    let stored_sensitive: bool = sqlx::query_scalar(
        "SELECT is_sensitive FROM habit_occurrence_evidence WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("sensitivity snapshot");
    assert!(stored_sensitive);

    let unknown = service
        .put_outcome(
            habit_id,
            Uuid::new_v4(),
            partial_command(Uuid::new_v4(), 0, PRIVATE_NOTE),
            habit_key("habit-unknown", None),
        )
        .await;
    assert!(
        matches!(
            &unknown,
            Err(dayweave_api::habits::HabitServiceError::Repository(
                HabitRepositoryError::OccurrenceNotFound(_)
            ))
        ),
        "unexpected arbitrary-id result: {unknown:?}"
    );
    let planner_id_is_not_write_authority = service
        .put_outcome(
            habit_id,
            planner_occurrence_id,
            partial_command(Uuid::new_v4(), 0, PRIVATE_NOTE),
            habit_key("habit-planner-id", None),
        )
        .await;
    assert!(
        matches!(
            &planner_id_is_not_write_authority,
            Err(dayweave_api::habits::HabitServiceError::Repository(
                HabitRepositoryError::OccurrenceNotFound(_)
            ))
        ),
        "unexpected planner-id result: {planner_id_is_not_write_authority:?}"
    );

    // This preview observes only the schedule-publication evidence change. A
    // later outcome must invalidate it at publication, not silently omit the
    // newly recorded lifecycle fact.
    let stale_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose before habit outcome");

    // Publication must retain the habit-change-head fence through commit. Hold
    // the habit projection lock, queue a publication that has already captured
    // this preview, then advance the durable head before releasing the lock.
    // Without the shared lock the stale schedule can commit before observing
    // this concurrent projection change.
    let mut habit_blocker = database.pool.begin().await.expect("habit lock transaction");
    let habit_blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *habit_blocker)
        .await
        .expect("habit blocker pid");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.habits.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .execute(&mut *habit_blocker)
    .await
    .expect("hold habit projection lock");
    let concurrent_repository = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let concurrent_access = access.clone();
    let concurrent_request = request.clone();
    let concurrent_preview = stale_preview.clone();
    let concurrent_publish = tokio::spawn(async move {
        concurrent_repository
            .publish(
                &concurrent_access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [102; 32],
                    input_digest: digest_bytes(&concurrent_preview.input_digest),
                    timezone_name: concurrent_request.timezone_name,
                    manual_placement_approvals: Vec::new(),
                    result: concurrent_preview,
                    published_at: postgres_now(),
                },
            )
            .await
    });
    wait_for_blocked_queries(&database.pool, habit_blocker_pid, 1).await;
    sqlx::query(
        "INSERT INTO habit_changes (workspace_id, change_kind, entity_id, component_revision, \
         payload, changed_at) VALUES ($1, 'occurrence_upsert', $2, 1, $3, $4)",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .bind(
        serde_json::to_value(HabitDeltaChange::OccurrenceUpsert {
            occurrence: occurrences[0].clone(),
        })
        .expect("encode concurrent habit projection"),
    )
    .bind(postgres_now())
    .execute(&mut *habit_blocker)
    .await
    .expect("advance habit head while publication waits");
    habit_blocker
        .commit()
        .await
        .expect("release habit projection lock");
    assert!(matches!(
        concurrent_publish.await.expect("join queued publication"),
        Err(SchedulePublicationError::StaleComposition)
    ));

    let operation_id = Uuid::new_v4();
    let partial_command = partial_command(operation_id, 0, PRIVATE_NOTE);
    let partial = service
        .put_outcome(
            habit_id,
            evidence_id,
            partial_command.clone(),
            habit_key("habit-partial", None),
        )
        .await
        .expect("record partial outcome");
    assert!(!partial.replayed);
    assert_eq!(partial.value.outcome.as_ref().unwrap().revision, 1);
    assert_eq!(partial.value.outcome.as_ref().unwrap().quantity, Some(-2));

    let replay = service
        .put_outcome(
            habit_id,
            evidence_id,
            partial_command.clone(),
            habit_key("habit-partial", None),
        )
        .await;
    // Exact key+operation replay survives the transaction boundary and returns
    // the original projection rather than advancing it again.
    assert!(replay.expect("durable replay").replayed);

    let partial_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose authoritative partial progress");
    let remaining_minutes: i64 = partial_preview
        .plan
        .blocks
        .iter()
        .filter(|block| block.occurrence_id.map(|id| id.0) == Some(planner_occurrence_id))
        .map(|block| (block.end - block.start).whole_minutes())
        .sum();
    assert_eq!(
        remaining_minutes, 15,
        "50% progress on an immutable 30-minute habit must reserve only 15 minutes"
    );

    let stale_publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [3; 32],
                input_digest: digest_bytes(&stale_preview.input_digest),
                timezone_name: request.timezone_name.clone(),
                manual_placement_approvals: Vec::new(),
                result: stale_preview,
                published_at: postgres_now(),
            },
        )
        .await;
    assert!(matches!(
        stale_publication,
        Err(SchedulePublicationError::StaleComposition)
    ));

    let head_before_republish = repository.delta_head().await.expect("habit head");
    let observed_before_republish: DateTime<Utc> = sqlx::query_scalar(
        "SELECT last_published_at FROM habit_occurrence_evidence \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("initial evidence observation");
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [4; 32],
                input_digest: digest_bytes(&partial_preview.input_digest),
                timezone_name: request.timezone_name.clone(),
                manual_placement_approvals: Vec::new(),
                result: partial_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish current partial-progress schedule");
    assert_eq!(
        repository
            .delta_head()
            .await
            .expect("habit head after publish"),
        head_before_republish,
        "re-publishing unchanged occurrence evidence must not create a habit delta"
    );
    let observed_after_republish: DateTime<Utc> = sqlx::query_scalar(
        "SELECT last_published_at FROM habit_occurrence_evidence \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("stable evidence observation");
    assert_eq!(observed_after_republish, observed_before_republish);

    let left = service.put_outcome(
        habit_id,
        evidence_id,
        terminal_command(Uuid::new_v4(), 1, HabitOutcomeStatus::Completed, 10_000),
        habit_key("habit-terminal-left", None),
    );
    let right = service.put_outcome(
        habit_id,
        evidence_id,
        terminal_command(Uuid::new_v4(), 1, HabitOutcomeStatus::Skipped, 5_000),
        habit_key("habit-terminal-right", None),
    );
    let (left, right) = tokio::join!(left, right);
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let rejected = if left.is_err() { left } else { right };
    assert!(matches!(
        rejected,
        Err(dayweave_api::habits::HabitServiceError::Repository(
            HabitRepositoryError::RevisionConflict { actual: 2, .. }
        ))
    ));

    let current = service
        .list_occurrences(
            habit_id,
            "2025-10-26".parse().unwrap(),
            "2025-10-26".parse().unwrap(),
            None,
            100,
        )
        .await
        .expect("current projection")
        .occurrences
        .pop()
        .expect("occurrence");
    assert_eq!(current.outcome.as_ref().unwrap().revision, 2);
    assert!(matches!(
        current.outcome.as_ref().unwrap().status,
        HabitOutcomeStatus::Completed | HabitOutcomeStatus::Skipped
    ));

    let recomposed = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose terminal lifecycle projection");
    assert!(
        recomposed
            .plan
            .blocks
            .iter()
            .all(|block| block.occurrence_id.map(|id| id.0) != Some(planner_occurrence_id)),
        "completed/skipped authoritative occurrence must not be scheduled again"
    );

    let analytics = service
        .analytics(
            habit_id,
            "2025-10-26".parse().unwrap(),
            "2025-10-26".parse().unwrap(),
            HabitAnalyticsBucket::Day,
        )
        .await
        .expect("analytics");
    assert_eq!(analytics.totals.expected, 1);
    assert_eq!(analytics.trends.len(), 1);
    assert_eq!(
        analytics.totals.adherence_basis_points,
        current
            .outcome
            .as_ref()
            .expect("terminal")
            .progress_basis_points
    );

    let pause_id = Uuid::new_v4();
    let started_at = postgres_now() - Duration::minutes(5);
    let pause = service
        .create_pause(
            habit_id,
            HabitPauseStartCommand {
                operation_id: Uuid::new_v4(),
                pause_id,
                expected_revision: 0,
                started_at,
            },
            habit_key("habit-pause", None),
        )
        .await
        .expect("open pause");
    assert_eq!(pause.value.revision, 1);
    assert!(pause.value.preserves_streak);
    let duplicate = service
        .create_pause(
            habit_id,
            HabitPauseStartCommand {
                operation_id: Uuid::new_v4(),
                pause_id: Uuid::new_v4(),
                expected_revision: 0,
                started_at: postgres_now(),
            },
            habit_key("habit-pause-overlap", None),
        )
        .await;
    assert!(matches!(
        duplicate,
        Err(dayweave_api::habits::HabitServiceError::Repository(
            HabitRepositoryError::OpenPauseConflict(_)
        ))
    ));
    let resumed = service
        .resume_pause(
            habit_id,
            pause_id,
            HabitPauseResumeCommand {
                operation_id: Uuid::new_v4(),
                expected_revision: 1,
                ended_at: postgres_now(),
            },
            habit_key("habit-pause-resume", None),
        )
        .await
        .expect("close pause");
    assert_eq!(resumed.value.revision, 2);
    assert!(resumed.value.ended_at.is_some());

    let versions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM habit_occurrence_versions WHERE workspace_id = $1 AND occurrence_evidence_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("version count");
    assert_eq!(versions, 2);
    let audit_text: String = sqlx::query_scalar(
        "SELECT COALESCE(string_agg(metadata::text, ''), '') FROM audit_operations \
         WHERE workspace_id = $1 AND entity_type IN ('habit_occurrence', 'habit_pause')",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("audit text");
    assert!(!audit_text.contains(PRIVATE_NOTE));
    let outbox_text: String = sqlx::query_scalar(
        "SELECT COALESCE(string_agg(payload::text, ''), '') FROM outbox_messages \
         WHERE workspace_id = $1 AND aggregate_type = 'habit'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("outbox text");
    assert!(!outbox_text.contains(PRIVATE_NOTE));

    assert!(
        sqlx::query("DELETE FROM habit_occurrence_versions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .execute(&database.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE habit_occurrence_evidence SET timezone_name = 'UTC' WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(evidence_id)
        .execute(&database.pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM habit_pauses WHERE workspace_id = $1 AND id = $2")
            .bind(scope.workspace_id)
            .bind(pause_id)
            .execute(&database.pool)
            .await
            .is_err()
    );

    // A malformed completion outside the requested horizon must never become a trusted rolling
    // recurrence anchor. Keeping its window outside the request independently exercises the
    // DISTINCT ON completion-anchor query rather than the active terminal-occurrence query.
    let active_corrupted_evidence_id = Uuid::new_v4();
    let active_corrupted_planner_id =
        Uuid::new_v5(&habit_id, b"active-corrupted-authoritative-evidence");
    let inserted = sqlx::query(
        "INSERT INTO habit_occurrence_evidence (id, workspace_id, habit_id, planner_occurrence_id, \
         source_schedule_revision_id, source_item_revision, policy_fingerprint, recurrence_identity, \
         nominal_start, nominal_end, window_start, window_end, local_date, timezone_name, \
         expected_duration_seconds, expected_quantity, expected_unit, is_sensitive, created_at, \
         last_published_at) SELECT $3, workspace_id, habit_id, $4, source_schedule_revision_id, \
         source_item_revision, policy_fingerprint, $5, \
         '2040-01-01T09:00:00Z'::timestamptz, '2040-01-01T09:30:00Z'::timestamptz, \
         '2040-01-01T08:00:00Z'::timestamptz, '2040-01-01T10:00:00Z'::timestamptz, \
         '2040-01-01'::date, timezone_name, expected_duration_seconds, expected_quantity, \
         expected_unit, is_sensitive, created_at, last_published_at \
         FROM habit_occurrence_evidence WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .bind(active_corrupted_evidence_id)
    .bind(active_corrupted_planner_id)
    .bind(json!({"type":"custom"}))
    .execute(&database.pool)
    .await
    .expect("seed active corrupted evidence row");
    assert_eq!(inserted.rows_affected(), 1);
    let custom_ordinal: i64 = sqlx::query_scalar(
        "SELECT recurrence_ordinal FROM habit_occurrence_evidence \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(active_corrupted_evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("read legacy custom recurrence ordinal");
    assert_eq!(custom_ordinal, 0);
    sqlx::query(
        "INSERT INTO habit_occurrence_outcomes (workspace_id, occurrence_evidence_id, revision, \
         status, progress_basis_points, quantity, unit, actual_seconds, note, occurred_at, updated_at) \
         VALUES ($1,$2,1,'completed',10000,NULL,NULL,NULL,NULL,$3,$3)",
    )
    .bind(scope.workspace_id)
    .bind(active_corrupted_evidence_id)
    .bind(postgres_now() + Duration::days(1))
    .execute(&database.pool)
    .await
    .expect("seed active corrupted completion outcome");
    let corrupted_preview = compose_canonical_schedule(&items, &schedules, request.clone()).await;
    assert!(
        matches!(
            corrupted_preview,
            Err(ComposeScheduleError::ExecutionEvidenceUnavailable)
        ),
        "out-of-horizon corrupted completion must fail anchor hydration: {corrupted_preview:?}"
    );

    items
        .trash(habit_id, 1, item_idempotency("habit-soft-delete", 9))
        .await
        .expect("soft-delete habit after durable receipt");
    let historical_replay = service
        .put_outcome(
            habit_id,
            evidence_id,
            partial_command,
            habit_key("habit-partial", None),
        )
        .await
        .expect("historical replay bypasses current item lifecycle gate");
    assert!(historical_replay.replayed);
    assert_eq!(historical_replay.value.outcome.unwrap().revision, 1);

    let other_scope = seed_scope(&database.pool, "other-habit-owner").await;
    let isolated = PostgresHabitRepository::new(database.pool.clone(), other_scope);
    assert_eq!(isolated.delta_head().await.expect("isolated head"), 0);
    assert!(
        isolated
            .list_occurrences(
                habit_id,
                "2025-10-26".parse().unwrap(),
                "2025-10-26".parse().unwrap(),
                None,
                100,
            )
            .await
            .expect("isolated list")
            .0
            .is_empty()
    );

    // Repository hydration must fail closed even if an operator or older binary bypassed the
    // current publication admission contract.
    let valid_occurrence = occurrences[0].clone();
    let corrupted_evidence_id = Uuid::new_v4();
    let corrupted_planner_id = Uuid::new_v5(&habit_id, b"corrupted-authoritative-evidence");
    let inserted = sqlx::query(
        "INSERT INTO habit_occurrence_evidence (id, workspace_id, habit_id, planner_occurrence_id, \
         source_schedule_revision_id, source_item_revision, policy_fingerprint, recurrence_identity, \
         nominal_start, nominal_end, window_start, window_end, local_date, timezone_name, \
         expected_duration_seconds, expected_quantity, expected_unit, is_sensitive, created_at, \
         last_published_at) SELECT $3, workspace_id, habit_id, $4, source_schedule_revision_id, \
         source_item_revision, policy_fingerprint, $5, nominal_start, nominal_end, window_start, \
         window_end, local_date, timezone_name, expected_duration_seconds, expected_quantity, \
         expected_unit, is_sensitive, created_at, last_published_at \
         FROM habit_occurrence_evidence WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .bind(corrupted_evidence_id)
    .bind(corrupted_planner_id)
    .bind(json!({"type":"custom"}))
    .execute(&database.pool)
    .await
    .expect("seed corrupted evidence row");
    assert_eq!(inserted.rows_affected(), 1);
    assert!(matches!(
        repository
            .list_occurrences(
                habit_id,
                "2025-10-26".parse().unwrap(),
                "2025-10-26".parse().unwrap(),
                None,
                100,
            )
            .await,
        Err(HabitRepositoryError::Internal)
    ));

    let delta_head = repository.delta_head().await.expect("delta head");
    let mut corrupted_delta = serde_json::to_value(HabitDeltaChange::OccurrenceUpsert {
        occurrence: valid_occurrence.clone(),
    })
    .expect("delta JSON");
    corrupted_delta["occurrence"]["evidence"]["identity"] = json!({"type":"custom"});
    sqlx::query(
        "INSERT INTO habit_changes (workspace_id, change_kind, entity_id, component_revision, \
         payload, changed_at) VALUES ($1, 'occurrence_upsert', $2, 1, $3, $4)",
    )
    .bind(scope.workspace_id)
    .bind(evidence_id)
    .bind(corrupted_delta)
    .bind(postgres_now())
    .execute(&database.pool)
    .await
    .expect("seed corrupted delta");
    assert!(matches!(
        repository.delta(delta_head, 100).await,
        Err(HabitRepositoryError::Internal)
    ));

    let receipt_identity = HabitIdempotency {
        namespace: "habit-corrupted-receipt",
        key_hash: [91; 32],
        request_fingerprint: [92; 32],
        operation_id: Uuid::new_v4(),
        actor_session_id: None,
    };
    let mut corrupted_receipt = json!({
        "type": "occurrence",
        "value": valid_occurrence,
    });
    corrupted_receipt["value"]["evidence"]["identity"] = json!({"type":"custom"});
    sqlx::query(
        "INSERT INTO habit_operation_receipts (workspace_id, namespace, key_hash, operation_id, \
         request_fingerprint, response_json, completed_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(scope.workspace_id)
    .bind(receipt_identity.namespace)
    .bind(receipt_identity.key_hash.as_slice())
    .bind(receipt_identity.operation_id)
    .bind(receipt_identity.request_fingerprint.as_slice())
    .bind(corrupted_receipt)
    .bind(postgres_now())
    .execute(&database.pool)
    .await
    .expect("seed corrupted receipt");
    assert!(matches!(
        repository.replay_outcome(&receipt_identity).await,
        Err(HabitRepositoryError::Internal)
    ));

    database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn missed_policies_persist_bind_hydrate_and_publish_without_rewriting_evidence() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; habit missed-policy test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool, "habit-missed-owner").await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    ));
    let policies = [
        ("skip", Uuid::new_v4()),
        ("carry", Uuid::new_v4()),
        ("reduce_frequency", Uuid::new_v4()),
        ("ask", Uuid::new_v4()),
    ];
    for (index, (policy, id)) in policies.iter().enumerate() {
        let mut item = habit(*id);
        item.flexible_constraints["habit_missed_policy"] = json!(policy);
        item.sibling_order = u32::try_from(index).expect("small index");
        items
            .create(
                item,
                item_idempotency(
                    &format!("habit-missed-create-{index}"),
                    u8::try_from(index + 70).expect("small marker"),
                ),
            )
            .await
            .expect("create policy habit");
    }
    let schedules = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let access = ScheduleAccess {
        subject: "auth0|habit-missed-owner".to_owned(),
        include_sensitive: true,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };
    let old_date = (postgres_now() - Duration::days(2))
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive();
    let old_request = compose_request_for_local_day(&old_date.to_string());
    let old_preview = compose_canonical_schedule(&items, &schedules, old_request.clone())
        .await
        .expect("compose overdue policy occurrences");
    assert_eq!(old_preview.plan.occurrences.len(), policies.len());
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [71; 32],
                input_digest: digest_bytes(&old_preview.input_digest),
                timezone_name: old_request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: old_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish overdue policy occurrences");

    let repository = Arc::new(PostgresHabitRepository::new(database.pool.clone(), scope));
    let service = HabitService::new(repository.clone(), items.clone(), Arc::new(SystemClock));
    let mut old_occurrences = std::collections::BTreeMap::new();
    for (_, habit_id) in policies {
        let occurrence = service
            .list_occurrences(habit_id, old_date, old_date, None, 10)
            .await
            .expect("list old occurrence")
            .occurrences
            .into_iter()
            .next()
            .expect("old occurrence");
        old_occurrences.insert(habit_id, occurrence);
    }
    let carry_before = &old_occurrences[&policies[1].1].evidence;
    let immutable_before = (
        carry_before.nominal_start,
        carry_before.nominal_end,
        carry_before.window_start,
        carry_before.window_end,
        carry_before.source_schedule_revision_id,
        carry_before.policy_fingerprint.clone(),
    );

    // The source contains genuinely private partial evidence before missed
    // reconciliation. Skip hydration must suppress the partial overlay, while
    // receipts/audit/outbox remain content-free.
    service
        .put_outcome(
            policies[0].1,
            old_occurrences[&policies[0].1].evidence.id,
            partial_command(Uuid::new_v4(), 0, PRIVATE_NOTE),
            habit_key("habit-missed-private-partial", None),
        )
        .await
        .expect("record private partial missed source");

    let operation_id = Uuid::new_v4();
    let reconciled = service
        .reconcile_missed(
            HabitMissedReconcileCommand { operation_id },
            4,
            habit_key("habit-missed-reconcile-001", None),
        )
        .await
        .expect("reconcile missed policies");
    assert!(!reconciled.replayed);
    assert_eq!(reconciled.value.resolutions.len(), 4);
    assert!(!reconciled.value.has_more);
    let by_habit = reconciled
        .value
        .resolutions
        .iter()
        .map(|resolution| (resolution.habit_id, resolution))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(matches!(
        by_habit[&policies[0].1].action,
        HabitMissedResolutionAction::Skip
    ));
    let (carry_start, carry_end) = match by_habit[&policies[1].1].action {
        HabitMissedResolutionAction::Carry {
            window_start,
            window_end,
        } => (window_start, window_end),
        ref other => panic!("expected carry resolution, got {other:?}"),
    };
    assert!(matches!(
        by_habit[&policies[2].1].action,
        HabitMissedResolutionAction::ReductionPending
    ));
    assert!(matches!(
        by_habit[&policies[3].1].action,
        HabitMissedResolutionAction::DecisionRequired
    ));

    let replayed = service
        .reconcile_missed(
            HabitMissedReconcileCommand { operation_id },
            4,
            habit_key("habit-missed-reconcile-001", None),
        )
        .await
        .expect("replay missed reconciliation");
    assert!(replayed.replayed);
    assert_eq!(replayed.value, reconciled.value);
    // Build an old immutable-receipt fixture without changing production
    // retention behavior. Changed responses live in the permanent ledger and
    // must replay even after both the 12-hour minimum lease and nominal
    // 24-hour empty-scan expiry have elapsed.
    sqlx::query(
        "ALTER TABLE habit_operation_receipts DISABLE TRIGGER habit_operation_receipts_immutable",
    )
    .execute(&database.pool)
    .await
    .expect("open immutable receipt fixture setup");
    sqlx::query(
        "UPDATE habit_operation_receipts \
         SET completed_at = clock_timestamp() - INTERVAL '25 hours' \
         WHERE workspace_id = $1 AND namespace = 'habits.missed.reconcile' \
           AND operation_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(operation_id)
    .execute(&database.pool)
    .await
    .expect("age changed reconcile receipt fixture");
    sqlx::query(
        "ALTER TABLE habit_operation_receipts ENABLE TRIGGER habit_operation_receipts_immutable",
    )
    .execute(&database.pool)
    .await
    .expect("close immutable receipt fixture setup");
    let aged_replay = service
        .reconcile_missed(
            HabitMissedReconcileCommand { operation_id },
            4,
            habit_key("habit-missed-reconcile-001", None),
        )
        .await
        .expect("replay changed reconciliation after the empty-scan retention window");
    assert!(aged_replay.replayed);
    assert_eq!(aged_replay.value, reconciled.value);

    // A sparse future publication does not prove that its first visible row is
    // the immediate next occurrence after the missed source. Keep reduction
    // pending until a publication whose horizon starts at the server clock
    // closes that gap.
    let sparse_request = compose_request_for_local_day("2027-01-15");
    let sparse_preview = compose_canonical_schedule(&items, &schedules, sparse_request.clone())
        .await
        .expect("compose sparse future reduction candidates");
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [72; 32],
                input_digest: digest_bytes(&sparse_preview.input_digest),
                timezone_name: sparse_request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: sparse_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish sparse future candidates");
    let sparse_reconcile = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-sparse-future", None),
        )
        .await
        .expect("retain pending reduction across a sparse future publication");
    assert!(sparse_reconcile.value.resolutions.is_empty());
    assert!(!sparse_reconcile.value.has_more);

    let target_date = postgres_now()
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive();
    let future_request = compose_request_for_local_day(&target_date.to_string());
    let future_preview = compose_canonical_schedule(&items, &schedules, future_request.clone())
        .await
        .expect("compose future reduction targets");
    assert_eq!(future_preview.plan.occurrences.len(), policies.len());
    let reduction_target = *future_preview
        .plan
        .occurrences
        .iter()
        .find(|occurrence| occurrence.series_item_id.0 == policies[2].1)
        .expect("future reduction target");
    let reduction_target_id = reduction_target.id.0;
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [74; 32],
                input_digest: digest_bytes(&future_preview.input_digest),
                timezone_name: future_request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: future_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish future reduction targets");
    let bound_reduction = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-bind-reduction", None),
        )
        .await
        .expect("bind pending reduction to admitted future occurrence");
    assert_eq!(bound_reduction.value.resolutions.len(), 1);
    assert!(!bound_reduction.value.has_more);
    assert!(matches!(
        &bound_reduction.value.resolutions[0].action,
        HabitMissedResolutionAction::ReduceFrequency {
            suppressed_planner_occurrence_ids,
        } if suppressed_planner_occurrence_ids == &[reduction_target_id]
    ));
    let reduction_target_evidence_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM habit_occurrence_evidence WHERE workspace_id = $1 \
         AND habit_id = $2 AND planner_occurrence_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(policies[2].1)
    .bind(reduction_target_id)
    .fetch_one(&database.pool)
    .await
    .expect("query admitted reduction target evidence");
    assert!(
        sqlx::query(
            "INSERT INTO habit_missed_resolutions (workspace_id, occurrence_evidence_id, habit_id, \
             source_planner_occurrence_id, revision, configured_policy, action, \
             suppressed_planner_occurrence_ids, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,1,'reduce_frequency','reduce_frequency',ARRAY[$4]::uuid[],$5,$5)",
        )
        .bind(scope.workspace_id)
        .bind(reduction_target_evidence_id)
        .bind(policies[2].1)
        .bind(reduction_target_id)
        .bind(postgres_now())
        .execute(&database.pool)
        .await
        .is_err(),
        "the database projection must reject suppressing its own source occurrence"
    );
    let unauthorized_move_date = target_date + Duration::days(3);
    let mut unauthorized_move_request =
        compose_request_for_local_day(&unauthorized_move_date.to_string());
    unauthorized_move_request
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: ItemId(policies[2].1),
            selector: RecurrenceExceptionSelector::Occurrence {
                id: OccurrenceId(reduction_target_id),
            },
            action: RecurrenceExceptionAction::Move {
                start: reduction_target.window_start + time::Duration::days(3),
                end: reduction_target.window_end + time::Duration::days(3),
                source: RecurrenceMoveSource {
                    item_revision: 1,
                    identity: reduction_target.identity,
                    nominal_start: reduction_target.nominal_start,
                    nominal_end: reduction_target.nominal_end,
                    local_date: reduction_target.local_date,
                    ordinal: reduction_target.ordinal,
                },
            },
        });
    let authoritative_reduction =
        compose_canonical_schedule(&items, &schedules, unauthorized_move_request)
            .await
            .expect("authoritative reduction overrides caller move");
    assert!(
        authoritative_reduction
            .plan
            .occurrences
            .iter()
            .all(|occurrence| occurrence.id.0 != reduction_target_id),
        "a caller move cannot resurrect the authoritative reduction target"
    );

    let second_target_date = target_date.succ_opt().expect("second target date");
    let two_day_start = target_date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let two_day_end = second_target_date
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let two_day_request = compose_request_for_bounds(
        two_day_start,
        two_day_start,
        two_day_end,
        two_day_start + Duration::hours(6),
        two_day_end - Duration::hours(4),
    );
    let two_day_preview = compose_canonical_schedule(&items, &schedules, two_day_request.clone())
        .await
        .expect("compose two current reduction targets");
    let second_reduction_target = *two_day_preview
        .plan
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.series_item_id.0 == policies[2].1
                && occurrence
                    .local_date
                    .is_some_and(|date| date.to_string() == second_target_date.to_string())
        })
        .expect("second reduction target");
    let second_reduction_target_id = second_reduction_target.id.0;
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [75; 32],
                input_digest: digest_bytes(&two_day_preview.input_digest),
                timezone_name: two_day_request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: two_day_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish two current reduction targets");
    service
        .put_outcome(
            policies[2].1,
            reduction_target_evidence_id,
            partial_command(Uuid::new_v4(), 0, PRIVATE_NOTE),
            habit_key("habit-missed-reduction-target-partial", None),
        )
        .await
        .expect("record partial reduction target");
    compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_local_day(&target_date.to_string()),
    )
    .await
    .expect("partial target takes precedence over stale authoritative reduction");
    let repended = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reduction-repending", None),
        )
        .await
        .expect("re-pend reduction whose target became partial");
    assert!(matches!(
        repended.value.resolutions.as_slice(),
        [resolution]
            if resolution.habit_id == policies[2].1
                && resolution.revision == 3
                && matches!(resolution.action, HabitMissedResolutionAction::ReductionPending)
    ));
    assert!(
        !repended.value.has_more,
        "a partial immediate target leaves no actionable continuation: {:?}",
        repended.value
    );
    let exact_target_pending = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reduction-exact-target-pending", None),
        )
        .await
        .expect("keep reduction pending behind its partial immediate target");
    assert!(exact_target_pending.value.resolutions.is_empty());
    assert!(!exact_target_pending.value.has_more);
    assert_ne!(
        second_reduction_target_id, reduction_target_id,
        "a later eligible target exists but must not replace the exact immediate target"
    );
    service
        .put_outcome(
            policies[2].1,
            reduction_target_evidence_id,
            HabitOutcomeCommand {
                operation_id: Uuid::new_v4(),
                expected_revision: 1,
                outcome: HabitOutcomeInput {
                    status: HabitOutcomeStatus::Unresolved,
                    progress_basis_points: 0,
                    quantity: None,
                    unit: None,
                    actual_seconds: None,
                    note: None,
                    occurred_at: postgres_now(),
                },
            },
            habit_key("habit-missed-reduction-target-reopen", None),
        )
        .await
        .expect("correct the exact target back to unresolved");
    let rebound_to_exact = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reduction-bind-exact", None),
        )
        .await
        .expect("bind the exact target once it becomes eligible");
    assert!(matches!(
        rebound_to_exact.value.resolutions.as_slice(),
        [resolution]
            if resolution.habit_id == policies[2].1
                && resolution.revision == 4
                && matches!(
                    &resolution.action,
                    HabitMissedResolutionAction::ReduceFrequency {
                        suppressed_planner_occurrence_ids,
                    } if suppressed_planner_occurrence_ids == &[reduction_target_id]
                )
    ));
    let mut harmless_reduction_edit = habit_replacement_with_missed_policy("reduce_frequency");
    harmless_reduction_edit.title = "Renamed durable habit".to_owned();
    harmless_reduction_edit.importance = 81;
    harmless_reduction_edit.sibling_order = 2;
    items
        .replace(
            policies[2].1,
            1,
            harmless_reduction_edit,
            item_idempotency("habit-missed-reduction-harmless-edit", 97),
        )
        .await
        .expect("apply fingerprint-preserving reduction edit");
    let preserved_reduction = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reduction-harmless-reconcile", None),
        )
        .await
        .expect("preserve bound reduction after harmless edit");
    assert!(preserved_reduction.value.resolutions.is_empty());
    let preserved_preview = compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_local_day(&target_date.to_string()),
    )
    .await
    .expect("hydrate reduction after harmless edit");
    assert_eq!(
        preserved_preview
            .plan
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id.0 == reduction_target_id)
            .expect("current reduction target remains materialized as evidence")
            .state,
        OccurrenceState::Skipped,
        "policy-compatible current membership remains suppressed after an unrelated item edit"
    );

    let ask = &old_occurrences[&policies[3].1];
    items
        .replace(
            policies[3].1,
            1,
            habit_replacement_with_missed_policy("skip"),
            item_idempotency("habit-missed-ask-policy-edit", 93),
        )
        .await
        .expect("edit ask policy before explicit decision");
    let resolve_operation_id = Uuid::new_v4();
    let resolve_command = HabitMissedResolveCommand {
        operation_id: resolve_operation_id,
        expected_revision: 1,
        action: HabitMissedExplicitAction::Carry,
    };
    let resolved = service
        .resolve_missed(
            policies[3].1,
            ask.evidence.id,
            resolve_command.clone(),
            habit_key("habit-missed-resolve-ask", None),
        )
        .await
        .expect("cancel explicit decision that raced a policy edit");
    assert_eq!(resolved.value.revision, 2);
    assert!(matches!(
        resolved.value.action,
        HabitMissedResolutionAction::Cancelled {
            reason: dayweave_api::habits::HabitMissedCancellationReason::SourceObsolete,
            resume_action: dayweave_api::habits::HabitMissedResumeAction::Carry,
        }
    ));
    let resolve_replay = service
        .resolve_missed(
            policies[3].1,
            ask.evidence.id,
            resolve_command,
            habit_key("habit-missed-resolve-ask", None),
        )
        .await
        .expect("replay cancelled explicit decision");
    assert!(resolve_replay.replayed);
    assert_eq!(resolve_replay.value, resolved.value);
    items
        .replace(
            policies[3].1,
            2,
            habit_replacement_with_missed_policy("ask"),
            item_idempotency("habit-missed-ask-policy-restore", 94),
        )
        .await
        .expect("restore ask policy after race");
    let restored_explicit = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-resolve-restore", None),
        )
        .await
        .expect("restore selected carry after policy correction");
    assert!(matches!(
        restored_explicit.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 3
                && matches!(resolution.action, HabitMissedResolutionAction::Carry { .. })
    ));

    let mut inactive_ask = habit_replacement_with_missed_policy("ask");
    inactive_ask.status = ItemStatus::Completed;
    items
        .replace(
            policies[3].1,
            3,
            inactive_ask,
            item_idempotency("habit-missed-ask-inactive", 95),
        )
        .await
        .expect("make prompted habit inactive");
    let inactive_cancel = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-ask-inactive-cancel", None),
        )
        .await
        .expect("durably cancel action for inactive habit");
    assert!(matches!(
        inactive_cancel.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 4
                && matches!(
                    resolution.action,
                    HabitMissedResolutionAction::Cancelled {
                        reason: dayweave_api::habits::HabitMissedCancellationReason::SourceObsolete,
                        resume_action: dayweave_api::habits::HabitMissedResumeAction::Carry,
                    }
                )
    ));
    items
        .replace(
            policies[3].1,
            4,
            habit_replacement_with_missed_policy("ask"),
            item_idempotency("habit-missed-ask-reactivate", 96),
        )
        .await
        .expect("reactivate prompted habit");
    let active_restore = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-ask-reactivate-restore", None),
        )
        .await
        .expect("restore selected action after habit reactivation");
    assert!(matches!(
        active_restore.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 5
                && matches!(resolution.action, HabitMissedResolutionAction::Carry { .. })
    ));
    let stale = service
        .resolve_missed(
            policies[3].1,
            ask.evidence.id,
            HabitMissedResolveCommand {
                operation_id: Uuid::new_v4(),
                expected_revision: 1,
                action: HabitMissedExplicitAction::Carry,
            },
            habit_key("habit-missed-resolve-stale", None),
        )
        .await;
    assert!(matches!(
        stale,
        Err(dayweave_api::habits::HabitServiceError::Repository(
            HabitRepositoryError::RevisionConflict { actual: 5, .. }
        ))
    ));

    let left_clipped_start = carry_start + Duration::minutes(5);
    compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_bounds(
            left_clipped_start,
            left_clipped_start,
            carry_end + Duration::minutes(30),
            left_clipped_start,
            carry_end + Duration::minutes(30),
        ),
    )
    .await
    .expect("a carry crossing the left horizon edge is safely suppressed");
    let right_clipped_end = carry_end - Duration::minutes(5);
    compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_bounds(
            carry_start - Duration::minutes(30),
            carry_start - Duration::minutes(30),
            right_clipped_end,
            carry_start - Duration::minutes(30),
            right_clipped_end,
        ),
    )
    .await
    .expect("a carry crossing the right horizon edge is safely suppressed");

    let carry_request = compose_request_for_window(carry_start, carry_end);
    let carry_preview = compose_canonical_schedule(&items, &schedules, carry_request.clone())
        .await
        .expect("hydrate authoritative carry move");
    let carried_planner_id = old_occurrences[&policies[1].1]
        .evidence
        .planner_occurrence_id;
    assert!(carry_preview.plan.occurrences.iter().any(|occurrence| {
        occurrence.id.0 == carried_planner_id
            && occurrence.window_start.unix_timestamp() == carry_start.timestamp()
            && occurrence.window_end.unix_timestamp() == carry_end.timestamp()
    }));
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [73; 32],
                input_digest: digest_bytes(&carry_preview.input_digest),
                timezone_name: carry_request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: carry_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish carried occurrence without rewriting source evidence");

    let reconcile_receipts_before_noop: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM habit_operation_receipts \
         WHERE workspace_id = $1 AND namespace = 'habits.missed.reconcile'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count reconcile receipts before terminal scans");
    let ephemeral_receipts_before_noop: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count expiring reconcile receipts before terminal scans");
    let rolled_horizon_operation_id = Uuid::new_v4();
    let rolled_horizon = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: rolled_horizon_operation_id,
            },
            4,
            habit_key("habit-missed-reduction-horizon-roll", None),
        )
        .await
        .expect("retain reduction after its target rolls outside the current horizon");
    assert!(rolled_horizon.value.resolutions.is_empty());
    assert!(!rolled_horizon.value.has_more);
    assert!(!rolled_horizon.replayed);
    let rolled_horizon_retry = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: rolled_horizon_operation_id,
            },
            4,
            habit_key("habit-missed-reduction-horizon-roll", None),
        )
        .await
        .expect("repeat terminal no-op reconciliation");
    assert!(rolled_horizon_retry.replayed);
    assert!(rolled_horizon_retry.value.resolutions.is_empty());
    let another_noop = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reduction-horizon-roll-again", None),
        )
        .await
        .expect("run another terminal no-op reconciliation");
    assert!(!another_noop.replayed);
    assert!(another_noop.value.resolutions.is_empty());
    let ephemeral_receipts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count bounded terminal-scan receipts");
    assert_eq!(ephemeral_receipts, ephemeral_receipts_before_noop + 2);
    sqlx::query(
        "UPDATE idempotency_keys SET created_at = clock_timestamp() - INTERVAL '2 days', \
           expires_at = clock_timestamp() - INTERVAL '1 day' \
         WHERE workspace_id = $1 AND namespace = 'habits.missed.reconcile' \
           AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .execute(&database.pool)
    .await
    .expect("expire terminal-scan receipts");
    let after_expiry = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reduction-after-receipt-expiry", None),
        )
        .await
        .expect("replace expired terminal-scan receipts");
    assert!(!after_expiry.replayed);
    assert!(after_expiry.value.resolutions.is_empty());
    let retained_ephemeral_receipts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count cleaned terminal-scan receipts");
    assert_eq!(retained_ephemeral_receipts, 1);
    let empty_receipt_json: Value = sqlx::query_scalar(
        "SELECT response_json FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt' LIMIT 1",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("load terminal-scan receipt fixture");
    sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
           state, resource_type, resource_id, response_json, created_at, updated_at, expires_at) \
         SELECT $1, 'habits.missed.reconcile', \
           decode(lpad(to_hex(sequence), 64, '0'), 'hex'), \
           decode(lpad(to_hex(sequence + 5000), 64, '0'), 'hex'), 'completed', \
           'habit_missed_reconcile_receipt', \
           ('00000000-0000-5000-8000-' || lpad(to_hex(sequence), 12, '0'))::uuid, \
           $2, clock_timestamp(), clock_timestamp(), clock_timestamp() + INTERVAL '24 hours' \
         FROM generate_series(1, 4095) AS sequence",
    )
    .bind(scope.workspace_id)
    .bind(empty_receipt_json)
    .execute(&database.pool)
    .await
    .expect("fill recent terminal-scan receipt capacity");
    let capacity_result = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reconcile-young-capacity", None),
        )
        .await;
    assert!(matches!(
        capacity_result,
        Err(dayweave_api::habits::HabitServiceError::Repository(
            HabitRepositoryError::ReconcileReceiptCapacity
        ))
    ));
    let young_receipts_after_capacity: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count protected recent terminal-scan receipts");
    assert_eq!(young_receipts_after_capacity, 4_096);
    sqlx::query(
        "UPDATE idempotency_keys \
         SET created_at = clock_timestamp() - INTERVAL '13 hours', \
             updated_at = clock_timestamp() - INTERVAL '13 hours' \
         WHERE ctid IN ( \
           SELECT ctid FROM idempotency_keys WHERE workspace_id = $1 \
             AND namespace = 'habits.missed.reconcile' \
             AND resource_type = 'habit_missed_reconcile_receipt' \
           ORDER BY created_at, key_hash LIMIT 1)",
    )
    .bind(scope.workspace_id)
    .execute(&database.pool)
    .await
    .expect("age one still-unexpired receipt beyond the minimum lease");
    let pressure_eviction = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-reconcile-pressure-eviction", None),
        )
        .await
        .expect("replace exactly one receipt older than the minimum lease");
    assert!(!pressure_eviction.replayed);
    assert!(pressure_eviction.value.resolutions.is_empty());
    let receipts_after_pressure_eviction: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count receipts after bounded pressure eviction");
    assert_eq!(receipts_after_pressure_eviction, 4_096);
    let old_receipts_after_pressure_eviction: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt' \
         AND created_at <= clock_timestamp() - INTERVAL '12 hours'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("verify only the eligible pressure receipt was evicted");
    assert_eq!(old_receipts_after_pressure_eviction, 0);
    sqlx::query(
        "DELETE FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = 'habits.missed.reconcile' \
         AND resource_type = 'habit_missed_reconcile_receipt'",
    )
    .bind(scope.workspace_id)
    .execute(&database.pool)
    .await
    .expect("remove terminal-scan capacity fixtures");
    let reconcile_receipts_after_noop: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM habit_operation_receipts \
         WHERE workspace_id = $1 AND namespace = 'habits.missed.reconcile'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("count reconcile receipts after terminal scans");
    assert_eq!(
        reconcile_receipts_after_noop, reconcile_receipts_before_noop,
        "terminal no-op polling must not grow permanent receipt storage"
    );

    let ephemeral_collision_operation = Uuid::new_v4();
    let ephemeral_collision = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: ephemeral_collision_operation,
            },
            4,
            habit_key("habit-missed-reconcile-ephemeral-operation", None),
        )
        .await
        .expect("store an empty reconcile receipt for operation collision coverage");
    assert!(ephemeral_collision.value.resolutions.is_empty());
    assert!(matches!(
        service
            .put_outcome(
                policies[0].1,
                old_occurrences[&policies[0].1].evidence.id,
                terminal_command(
                    ephemeral_collision_operation,
                    1,
                    HabitOutcomeStatus::Completed,
                    10_000,
                ),
                habit_key("habit-missed-ephemeral-to-permanent-collision", None),
            )
            .await,
        Err(dayweave_api::habits::HabitServiceError::Repository(
            HabitRepositoryError::IdempotencyConflict
        ))
    ));

    let carry_after = service
        .list_occurrences(policies[1].1, old_date, old_date, None, 10)
        .await
        .expect("reload carried source")
        .occurrences
        .into_iter()
        .next()
        .expect("carried source occurrence");
    assert_eq!(
        (
            carry_after.evidence.nominal_start,
            carry_after.evidence.nominal_end,
            carry_after.evidence.window_start,
            carry_after.evidence.window_end,
            carry_after.evidence.source_schedule_revision_id,
            carry_after.evidence.policy_fingerprint,
        ),
        immutable_before
    );
    assert!(matches!(
        carry_after
            .missed_resolution
            .expect("carry resolution")
            .action,
        HabitMissedResolutionAction::Carry { .. }
    ));

    let completed_operation = Uuid::new_v4();
    service
        .put_outcome(
            policies[0].1,
            old_occurrences[&policies[0].1].evidence.id,
            terminal_command(
                completed_operation,
                1,
                HabitOutcomeStatus::Completed,
                10_000,
            ),
            habit_key("habit-missed-source-late-completion", None),
        )
        .await
        .expect("complete skipped source late");
    assert!(matches!(
        service
            .reconcile_missed(
                HabitMissedReconcileCommand {
                    operation_id: completed_operation,
                },
                4,
                habit_key("habit-missed-permanent-to-ephemeral-collision", None),
            )
            .await,
        Err(dayweave_api::habits::HabitServiceError::Repository(
            HabitRepositoryError::IdempotencyConflict
        ))
    ));
    let cancelled = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-source-cancelled", None),
        )
        .await
        .expect("cancel skip after terminal correction");
    assert!(matches!(
        cancelled.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 2
                && matches!(
                    resolution.action,
                    HabitMissedResolutionAction::Cancelled {
                        reason: dayweave_api::habits::HabitMissedCancellationReason::SourceCompleted,
                        resume_action: dayweave_api::habits::HabitMissedResumeAction::Skip,
                    }
                )
    ));
    service
        .put_outcome(
            policies[0].1,
            old_occurrences[&policies[0].1].evidence.id,
            partial_command(Uuid::new_v4(), 2, PRIVATE_NOTE),
            habit_key("habit-missed-source-partial-correction", None),
        )
        .await
        .expect("correct completed source back to partial");
    let restored = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            4,
            habit_key("habit-missed-source-restored", None),
        )
        .await
        .expect("restore configured skip after correction");
    assert!(matches!(
        restored.value.resolutions.as_slice(),
        [resolution]
            if resolution.revision == 3
                && matches!(resolution.action, HabitMissedResolutionAction::Skip)
    ));

    let private_audit_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_operations WHERE workspace_id = $1 \
         AND operation_type = 'habit.missed.resolved' \
         AND metadata::text LIKE '%' || $2 || '%'",
    )
    .bind(scope.workspace_id)
    .bind(PRIVATE_NOTE)
    .fetch_one(&database.pool)
    .await
    .expect("query missed audit privacy");
    assert_eq!(private_audit_rows, 0);
    let version_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM habit_missed_resolution_versions WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("query missed versions");
    assert_eq!(version_count, 13);
    let duplicate_component_coordinates: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM habit_changes WHERE workspace_id = $1 AND entity_id = $2 \
         AND component_revision = 1",
    )
    .bind(scope.workspace_id)
    .bind(old_occurrences[&policies[0].1].evidence.id)
    .fetch_one(&database.pool)
    .await
    .expect("query independent occurrence component revisions");
    assert!(
        duplicate_component_coordinates >= 2,
        "outcome and missed-resolution revision one are independent coordinates"
    );
    let invalid_aggregate_ordering: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM habit_changes WHERE workspace_id = $1 \
         AND (entity_revision <> sequence OR entity_revision <= 0)",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("verify aggregate delta ordering");
    assert_eq!(invalid_aggregate_ordering, 0);
    let invalid_outbox_ordering: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_messages message \
         LEFT JOIN habit_changes delta ON delta.workspace_id = message.workspace_id \
           AND delta.entity_id = message.aggregate_id \
           AND delta.sequence = message.aggregate_revision \
         WHERE message.workspace_id = $1 AND message.aggregate_type = 'habit' \
           AND (delta.sequence IS NULL \
             OR (message.payload->>'aggregate_revision')::bigint <> delta.sequence \
             OR (message.payload->>'change_sequence')::bigint <> delta.sequence \
             OR (message.payload->>'component_revision')::bigint <> delta.component_revision)",
    )
    .bind(scope.workspace_id)
    .fetch_one(&database.pool)
    .await
    .expect("verify outbox aggregate ordering");
    assert_eq!(invalid_outbox_ordering, 0);

    drop(service);
    drop(schedules);
    drop(items);
    database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Builds the two valid competing projections and exercises real composition.
async fn inactive_reduction_source_does_not_mask_the_targets_own_missed_action() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; reduction precedence test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool, "habit-reduction-precedence-owner").await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    ));
    let habit_id = Uuid::new_v4();
    let mut item = habit(habit_id);
    item.flexible_constraints["habit_missed_policy"] = json!("ask");
    items
        .create(
            item,
            item_idempotency("habit-reduction-precedence-create", 212),
        )
        .await
        .expect("create precedence habit");

    let source_date = (postgres_now() - Duration::days(3))
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive();
    let middle_date = source_date.succ_opt().expect("middle local day");
    let target_date = middle_date.succ_opt().expect("target local day");
    let horizon_start = source_date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let horizon_end = target_date
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let request = compose_request_for_bounds(
        horizon_start,
        horizon_start,
        horizon_end,
        horizon_start + Duration::hours(6),
        horizon_end - Duration::hours(4),
    );
    let schedules = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let access = ScheduleAccess {
        subject: "auth0|habit-reduction-precedence-owner".to_owned(),
        include_sensitive: true,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };
    let preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose two precedence occurrences");
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [213; 32],
                input_digest: digest_bytes(&preview.input_digest),
                timezone_name: request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish precedence occurrences");
    let evidence = sqlx::query_as::<_, (Uuid, Uuid, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT id, planner_occurrence_id, window_start, window_end \
         FROM habit_occurrence_evidence \
         WHERE workspace_id = $1 AND habit_id = $2 \
         ORDER BY nominal_start, recurrence_ordinal, planner_occurrence_id",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .fetch_all(&database.pool)
    .await
    .expect("load precedence evidence");
    assert_eq!(evidence.len(), 3);
    let (source_evidence_id, source_planner_id, source_window_start, source_window_end) =
        evidence[0];
    let (middle_evidence_id, middle_planner_id, _, _) = evidence[1];
    let (target_evidence_id, target_planner_id, _, _) = evidence[2];
    sqlx::query(
        "INSERT INTO habit_missed_resolutions (workspace_id, occurrence_evidence_id, habit_id, \
           source_planner_occurrence_id, revision, configured_policy, action, \
           suppressed_planner_occurrence_ids, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,1,'ask','decision_required','{}'::uuid[],$7,$7), \
                ($1,$6,$3,$5,1,'ask','decision_required','{}'::uuid[],$7,$7)",
    )
    .bind(scope.workspace_id)
    .bind(source_evidence_id)
    .bind(habit_id)
    .bind(source_planner_id)
    .bind(middle_planner_id)
    .bind(middle_evidence_id)
    .bind(postgres_now())
    .execute(&database.pool)
    .await
    .expect("seed initial decision projections");
    sqlx::query(
        "UPDATE habit_missed_resolutions SET revision = 2, action = 'reduce_frequency', \
           suppressed_planner_occurrence_ids = ARRAY[$3]::uuid[], \
           updated_at = clock_timestamp() \
         WHERE workspace_id = $1 AND occurrence_evidence_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(source_evidence_id)
    .bind(middle_planner_id)
    .execute(&database.pool)
    .await
    .expect("select reduction for the source prompt");
    sqlx::query(
        "UPDATE habit_missed_resolutions SET revision = 2, action = 'reduce_frequency', \
           suppressed_planner_occurrence_ids = ARRAY[$3]::uuid[], \
           updated_at = clock_timestamp() \
         WHERE workspace_id = $1 AND occurrence_evidence_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(middle_evidence_id)
    .bind(target_planner_id)
    .execute(&database.pool)
    .await
    .expect("select the middle occurrence's chained reduction");

    let child_id = Uuid::new_v4();
    let mut child = recurring_task(child_id);
    child.recurrence = None;
    child.parent_id = Some(habit_id);
    items
        .create(
            child,
            item_idempotency("habit-reduction-precedence-create-child", 214),
        )
        .await
        .expect("make the reduction source habit non-leaf");
    let active_child_target_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM habit_effective_reduction_targets( \
           $1, $2, (SELECT policy_fingerprint FROM habit_occurrence_evidence \
                    WHERE workspace_id = $1 AND id = $3))",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .bind(source_evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("evaluate reduction graph while a live child exists");
    assert_eq!(active_child_target_count, 0);
    items
        .trash(
            child_id,
            1,
            item_idempotency("habit-reduction-precedence-trash-child", 215),
        )
        .await
        .expect("trash the former child");
    let trashed_child_target_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM habit_effective_reduction_targets( \
           $1, $2, (SELECT policy_fingerprint FROM habit_occurrence_evidence \
                    WHERE workspace_id = $1 AND id = $3))",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .bind(source_evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("evaluate reduction graph after trashing the former child");
    assert_eq!(
        trashed_child_target_count, 1,
        "a trashed former child must not keep a habit classified as non-leaf"
    );

    let repository = Arc::new(PostgresHabitRepository::new(database.pool.clone(), scope));
    let service = HabitService::new(repository, items.clone(), Arc::new(SystemClock));
    let reduced_target_analytics = service
        .analytics(
            habit_id,
            middle_date,
            middle_date,
            HabitAnalyticsBucket::Day,
        )
        .await
        .expect("aggregate a reduced target without its out-of-range source");
    assert_eq!(reduced_target_analytics.totals.expected, 0);
    assert_eq!(reduced_target_analytics.totals.eligible, 0);
    assert_eq!(reduced_target_analytics.totals.missed, 0);
    assert!(reduced_target_analytics.trends.is_empty());

    let admitted_target = service
        .reconcile_missed(
            HabitMissedReconcileCommand {
                operation_id: Uuid::new_v4(),
            },
            10,
            habit_key("habit-reduction-precedence-admit-target", None),
        )
        .await
        .expect("admit target whose chained suppressor is not effective");
    assert!(matches!(
        admitted_target.value.resolutions.as_slice(),
        [resolution]
            if resolution.occurrence_evidence_id == target_evidence_id
                && matches!(resolution.action, HabitMissedResolutionAction::DecisionRequired)
    ));
    let chained_preview = compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_local_day(&target_date.to_string()),
    )
    .await
    .expect("compose an alternating reduction chain");
    assert_eq!(
        chained_preview
            .plan
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id.0 == target_planner_id)
            .expect("chain target remains materialized")
            .state,
        OccurrenceState::Generated,
        "A -> B suppresses B -> C instead of cascading into C"
    );

    service
        .put_outcome(
            habit_id,
            source_evidence_id,
            terminal_command(Uuid::new_v4(), 0, HabitOutcomeStatus::Completed, 10_000),
            habit_key("habit-reduction-precedence-complete-source", None),
        )
        .await
        .expect("complete the reduction source before reconciliation");
    let terminal_preview = compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_local_day(&target_date.to_string()),
    )
    .await
    .expect("compose with terminal-source precedence before reconciliation");
    assert_eq!(
        terminal_preview
            .plan
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id.0 == target_planner_id)
            .expect("target occurrence remains materialized")
            .state,
        OccurrenceState::Skipped,
        "B -> C becomes effective immediately when terminal A no longer suppresses B"
    );

    service
        .put_outcome(
            habit_id,
            source_evidence_id,
            HabitOutcomeCommand {
                operation_id: Uuid::new_v4(),
                expected_revision: 1,
                outcome: HabitOutcomeInput {
                    status: HabitOutcomeStatus::Unresolved,
                    progress_basis_points: 0,
                    quantity: None,
                    unit: None,
                    actual_seconds: None,
                    note: None,
                    occurred_at: postgres_now(),
                },
            },
            habit_key("habit-reduction-precedence-reopen-source", None),
        )
        .await
        .expect("correct the source back to unresolved");
    let corrected_preview = compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_local_day(&target_date.to_string()),
    )
    .await
    .expect("compose after reopening the reduction source");
    assert_eq!(
        corrected_preview
            .plan
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id.0 == target_planner_id)
            .expect("corrected chain target remains materialized")
            .state,
        OccurrenceState::Generated
    );

    let pause_id = Uuid::new_v4();
    service
        .create_pause(
            habit_id,
            HabitPauseStartCommand {
                operation_id: Uuid::new_v4(),
                pause_id,
                expected_revision: 0,
                started_at: source_window_start,
            },
            habit_key("habit-reduction-precedence-pause-source", None),
        )
        .await
        .expect("open a retroactive source pause");
    service
        .resume_pause(
            habit_id,
            pause_id,
            HabitPauseResumeCommand {
                operation_id: Uuid::new_v4(),
                expected_revision: 1,
                ended_at: source_window_end,
            },
            habit_key("habit-reduction-precedence-resume-source", None),
        )
        .await
        .expect("bound the retroactive source pause");
    let paused_preview = compose_canonical_schedule(
        &items,
        &schedules,
        compose_request_for_local_day(&target_date.to_string()),
    )
    .await
    .expect("compose with paused-source precedence before reconciliation");
    assert_eq!(
        paused_preview
            .plan
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id.0 == target_planner_id)
            .expect("paused chain target remains materialized")
            .state,
        OccurrenceState::Skipped,
        "B -> C becomes effective while its earlier suppressor A is paused"
    );

    drop(service);
    drop(schedules);
    drop(items);
    database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn ineffective_physical_reduction_reservation_does_not_spin_pending_pages() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; reduction reservation test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool, "habit-reduction-reservation-owner").await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    ));
    let habit_id = Uuid::new_v4();
    let mut item = habit(habit_id);
    item.flexible_constraints["habit_missed_policy"] = json!("reduce_frequency");
    items
        .create(
            item,
            item_idempotency("habit-reduction-reservation-create", 216),
        )
        .await
        .expect("create reservation habit");

    let first_date = postgres_now()
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive();
    let horizon_start = first_date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let horizon_end = first_date
        .checked_add_signed(Duration::days(3))
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let request = compose_request_for_bounds(
        horizon_start,
        horizon_start,
        horizon_end,
        horizon_start + Duration::hours(6),
        horizon_end - Duration::hours(4),
    );
    let schedules = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let access = ScheduleAccess {
        subject: "auth0|habit-reduction-reservation-owner".to_owned(),
        include_sensitive: true,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };
    let preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose three future reservation occurrences");
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [217; 32],
                input_digest: digest_bytes(&preview.input_digest),
                timezone_name: request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish reservation occurrences");
    let evidence = sqlx::query_as::<_, (Uuid, Uuid, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT id, planner_occurrence_id, nominal_start, nominal_end \
         FROM habit_occurrence_evidence \
         WHERE workspace_id = $1 AND habit_id = $2 \
         ORDER BY nominal_start, recurrence_ordinal, planner_occurrence_id",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .fetch_all(&database.pool)
    .await
    .expect("load reservation evidence");
    assert_eq!(evidence.len(), 3);
    let (first_evidence_id, first_planner_id, _, _) = evidence[0];
    let (middle_evidence_id, middle_planner_id, _, _) = evidence[1];
    let (target_evidence_id, target_planner_id, _, _) = evidence[2];

    let pending_evidence_id = Uuid::new_v4();
    let pending_planner_id = Uuid::new_v5(&habit_id, b"physically-reserved-pending-source");
    sqlx::query(
        "INSERT INTO habit_occurrence_evidence (id, workspace_id, habit_id, planner_occurrence_id, \
           source_schedule_revision_id, source_item_revision, policy_fingerprint, \
           recurrence_identity, nominal_start, nominal_end, window_start, window_end, local_date, \
           timezone_name, expected_duration_seconds, expected_quantity, expected_unit, \
           is_sensitive, created_at, last_published_at) \
         SELECT $3, workspace_id, habit_id, $4, source_schedule_revision_id, source_item_revision, \
           policy_fingerprint, $5, nominal_start, nominal_end, window_start, window_end, local_date, \
           timezone_name, expected_duration_seconds, expected_quantity, expected_unit, \
           is_sensitive, $6, $6 \
         FROM habit_occurrence_evidence WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(target_evidence_id)
    .bind(pending_evidence_id)
    .bind(pending_planner_id)
    .bind(json!({"type":"custom"}))
    .bind(postgres_now())
    .execute(&database.pool)
    .await
    .expect("seed an unpublished pending source between current occurrences");

    let seeded_at = postgres_now();
    sqlx::query(
        "INSERT INTO habit_missed_resolutions (workspace_id, occurrence_evidence_id, habit_id, \
           source_planner_occurrence_id, revision, configured_policy, action, \
           suppressed_planner_occurrence_ids, created_at, updated_at) \
         SELECT workspace_id, id, habit_id, planner_occurrence_id, 1, 'reduce_frequency', \
           'reduction_pending', '{}'::uuid[], $3, $3 \
         FROM habit_occurrence_evidence \
         WHERE workspace_id = $1 AND id = ANY($2::uuid[])",
    )
    .bind(scope.workspace_id)
    .bind(vec![
        first_evidence_id,
        middle_evidence_id,
        pending_evidence_id,
    ])
    .bind(seeded_at)
    .execute(&database.pool)
    .await
    .expect("seed pending reduction projections");
    for (index, (source_id, target_id)) in [
        (first_evidence_id, middle_planner_id),
        (middle_evidence_id, target_planner_id),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "UPDATE habit_missed_resolutions SET revision = 2, action = 'reduce_frequency', \
               suppressed_planner_occurrence_ids = ARRAY[$3]::uuid[], updated_at = $4 \
             WHERE workspace_id = $1 AND occurrence_evidence_id = $2",
        )
        .bind(scope.workspace_id)
        .bind(source_id)
        .bind(target_id)
        .bind(seeded_at + Duration::microseconds(i64::try_from(index + 1).unwrap()))
        .execute(&database.pool)
        .await
        .expect("bind alternating physical reservation edge");
    }
    let effective_targets: Vec<Uuid> = sqlx::query_scalar(
        "SELECT planner_occurrence_id FROM habit_effective_reduction_targets( \
           $1, $2, (SELECT policy_fingerprint FROM habit_occurrence_evidence \
                    WHERE workspace_id = $1 AND id = $3))",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .bind(first_evidence_id)
    .fetch_all(&database.pool)
    .await
    .expect("evaluate alternating reservation graph");
    assert_eq!(effective_targets, vec![middle_planner_id]);
    assert!(!effective_targets.contains(&target_planner_id));

    let repository = Arc::new(PostgresHabitRepository::new(database.pool.clone(), scope));
    let service = HabitService::new(repository, items.clone(), Arc::new(SystemClock));
    for index in 0..2 {
        let reconciled = service
            .reconcile_missed(
                HabitMissedReconcileCommand {
                    operation_id: Uuid::new_v4(),
                },
                10,
                habit_key(
                    &format!("habit-reduction-reservation-no-spin-{index}"),
                    None,
                ),
            )
            .await
            .expect("an ineffective physical reservation is non-actionable");
        assert!(reconciled.value.resolutions.is_empty());
        assert!(
            !reconciled.value.has_more,
            "an unavailable physical reservation must not advertise a spinning next page"
        );
    }
    let pending_projection: (i64, String) = sqlx::query_as(
        "SELECT revision, action FROM habit_missed_resolutions \
         WHERE workspace_id = $1 AND occurrence_evidence_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(pending_evidence_id)
    .fetch_one(&database.pool)
    .await
    .expect("read retained pending projection");
    assert_eq!(pending_projection, (1, "reduction_pending".to_owned()));
    assert_ne!(first_planner_id, pending_planner_id);

    drop(service);
    drop(schedules);
    drop(items);
    database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn http_habit_lifecycle_persists_and_recomposes_against_postgres() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; habit PostgreSQL test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool, "habit-http-owner").await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(
        database.pool.clone(),
        scope,
    ));
    let repository = Arc::new(PostgresHabitRepository::new(database.pool.clone(), scope));
    let (app, access_token) = postgres_habit_http_app(
        &database.pool,
        scope,
        items.clone(),
        repository,
        schedules.clone(),
    )
    .await;
    let habit_id = Uuid::new_v4();
    let created = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/items",
            Some(serde_json::to_value(habit(habit_id)).expect("serialize HTTP lifecycle habit")),
            Some("habit-http-create-001"),
            &access_token,
        ))
        .await
        .expect("HTTP habit creation");
    assert_eq!(created.status(), StatusCode::CREATED);

    let request = compose_request();
    let preview_response = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/schedule/preview",
            Some(serde_json::to_value(&request).expect("serialize schedule preview")),
            None,
            &access_token,
        ))
        .await
        .expect("HTTP schedule preview");
    assert_eq!(preview_response.status(), StatusCode::OK);
    let preview = habit_http_body_json(preview_response).await;
    let planner_occurrence_id = Uuid::parse_str(
        preview["plan"]["occurrences"][0]["id"]
            .as_str()
            .expect("planned habit occurrence id"),
    )
    .expect("planner occurrence UUID");
    let publish_body = habit_http_publish_body(&request, &preview);
    let published = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/schedule/publish",
            Some(publish_body),
            None,
            &access_token,
        ))
        .await
        .expect("HTTP schedule publication");
    assert_eq!(published.status(), StatusCode::OK);
    let published = habit_http_body_json(published).await;
    let published_revision_id = Uuid::parse_str(
        published["revision"]["id"]
            .as_str()
            .expect("published schedule revision id"),
    )
    .expect("published schedule revision UUID");

    let list = app
        .clone()
        .oneshot(habit_http_request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/occurrences?start_date=2025-10-26&end_date=2025-10-26&limit=100"
            ),
            None,
            None,
            &access_token,
        ))
        .await
        .expect("HTTP occurrence list");
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(list.headers()[header::CACHE_CONTROL], "no-store, max-age=0");
    let list = habit_http_body_json(list).await;
    assert_eq!(list["occurrences"].as_array().map(Vec::len), Some(1));
    let occurrence: HabitOccurrence = serde_json::from_value(list["occurrences"][0].clone())
        .expect("native-compatible occurrence wire shape");
    occurrence
        .evidence
        .validate()
        .expect("strict published occurrence evidence");
    assert_eq!(occurrence.evidence.habit_id, habit_id);
    assert_eq!(
        occurrence.evidence.planner_occurrence_id,
        planner_occurrence_id
    );
    assert_eq!(
        occurrence.evidence.source_schedule_revision_id,
        published_revision_id
    );
    let evidence_id = occurrence.evidence.id;

    let baseline_delta = app
        .clone()
        .oneshot(habit_http_request(
            "GET",
            "/v1/habits/occurrences/delta?limit=200",
            None,
            None,
            &access_token,
        ))
        .await
        .expect("HTTP baseline delta");
    assert_eq!(baseline_delta.status(), StatusCode::OK);
    let baseline_delta = habit_http_body_json(baseline_delta).await;
    let baseline_cursor = baseline_delta["next_cursor"]
        .as_str()
        .expect("opaque baseline cursor")
        .to_owned();
    // Evidence admission advances the habit ledger head, so this post-publication preview is the
    // exact baseline that the outcome mutation below must invalidate.
    let stale_preview_response = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/schedule/preview",
            Some(serde_json::to_value(&request).expect("serialize stale-fence preview")),
            None,
            &access_token,
        ))
        .await
        .expect("HTTP stale-fence baseline preview");
    assert_eq!(stale_preview_response.status(), StatusCode::OK);
    let stale_preview = habit_http_body_json(stale_preview_response).await;
    let stale_publish_body = habit_http_publish_body(&request, &stale_preview);

    let partial_operation_id = Uuid::new_v4();
    let partial_body = serde_json::to_value(partial_command(partial_operation_id, 0, PRIVATE_NOTE))
        .expect("serialize partial outcome");
    let partial = app
        .clone()
        .oneshot(habit_http_request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body.clone()),
            Some("habit-http-partial-001"),
            &access_token,
        ))
        .await
        .expect("HTTP partial outcome");
    assert_eq!(partial.status(), StatusCode::OK);
    assert_eq!(partial.headers()["idempotency-replayed"], "false");
    assert_eq!(
        partial.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let partial = habit_http_body_json(partial).await;
    assert_eq!(partial["occurrence"]["outcome"]["revision"], 1);
    assert_eq!(
        partial["occurrence"]["outcome"]["progress_basis_points"],
        5_000
    );

    let replay = app
        .clone()
        .oneshot(habit_http_request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(partial_body),
            Some("habit-http-partial-001"),
            &access_token,
        ))
        .await
        .expect("HTTP partial replay");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert_eq!(habit_http_body_json(replay).await["replayed"], true);

    let stale_publication = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/schedule/publish",
            Some(stale_publish_body),
            None,
            &access_token,
        ))
        .await
        .expect("HTTP stale publication response");
    assert_eq!(stale_publication.status(), StatusCode::CONFLICT);
    assert_eq!(
        habit_http_body_json(stale_publication).await["error"]["code"],
        "schedule_publication_stale"
    );

    let partial_preview_response = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/schedule/preview",
            Some(serde_json::to_value(&request).expect("serialize partial preview")),
            None,
            &access_token,
        ))
        .await
        .expect("HTTP partial recomposition");
    assert_eq!(partial_preview_response.status(), StatusCode::OK);
    let partial_preview = habit_http_body_json(partial_preview_response).await;
    assert_eq!(
        habit_http_scheduled_minutes(&partial_preview, planner_occurrence_id),
        15
    );

    let completed = app
        .clone()
        .oneshot(habit_http_request(
            "PUT",
            &format!("/v1/habits/{habit_id}/occurrences/{evidence_id}"),
            Some(
                serde_json::to_value(terminal_command(
                    Uuid::new_v4(),
                    1,
                    HabitOutcomeStatus::Completed,
                    10_000,
                ))
                .expect("serialize completed outcome"),
            ),
            Some("habit-http-completed-001"),
            &access_token,
        ))
        .await
        .expect("HTTP completed correction");
    assert_eq!(completed.status(), StatusCode::OK);
    assert_eq!(
        habit_http_body_json(completed).await["occurrence"]["outcome"]["revision"],
        2
    );

    let recomposed = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            "/v1/schedule/preview",
            Some(serde_json::to_value(&request).expect("serialize terminal preview")),
            None,
            &access_token,
        ))
        .await
        .expect("HTTP terminal recomposition");
    assert_eq!(recomposed.status(), StatusCode::OK);
    let recomposed = habit_http_body_json(recomposed).await;
    assert!(
        recomposed["plan"]["blocks"]
            .as_array()
            .expect("terminal schedule blocks")
            .iter()
            .all(|block| block["occurrence_id"] != planner_occurrence_id.to_string()),
        "HTTP-completed authoritative occurrence must not be scheduled again"
    );

    let analytics = app
        .clone()
        .oneshot(habit_http_request(
            "GET",
            &format!(
                "/v1/habits/{habit_id}/analytics?start_date=2025-10-26&end_date=2025-10-26&bucket=day"
            ),
            None,
            None,
            &access_token,
        ))
        .await
        .expect("HTTP habit analytics");
    assert_eq!(analytics.status(), StatusCode::OK);
    assert_eq!(
        analytics.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(analytics.headers()[header::PRAGMA], "no-cache");
    let analytics = habit_http_body_json(analytics).await;
    assert_eq!(analytics["analytics"]["expected"], 1);
    assert_eq!(analytics["analytics"]["completed"], 1);
    assert_eq!(analytics["analytics"]["adherence_basis_points"], 10_000);
    assert!(!analytics.to_string().contains(PRIVATE_NOTE));

    let pause_id = Uuid::new_v4();
    let started_at = postgres_now() - Duration::minutes(5);
    let started = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses"),
            Some(
                serde_json::to_value(HabitPauseStartCommand {
                    operation_id: Uuid::new_v4(),
                    pause_id,
                    expected_revision: 0,
                    started_at,
                })
                .expect("serialize pause start"),
            ),
            Some("habit-http-pause-001"),
            &access_token,
        ))
        .await
        .expect("HTTP habit pause");
    assert_eq!(started.status(), StatusCode::OK);
    let started = habit_http_body_json(started).await;
    assert_eq!(started["pause"]["revision"], 1);
    assert_eq!(started["pause"]["preserves_streak"], true);

    let resumed = app
        .clone()
        .oneshot(habit_http_request(
            "POST",
            &format!("/v1/habits/{habit_id}/pauses/{pause_id}/resume"),
            Some(
                serde_json::to_value(HabitPauseResumeCommand {
                    operation_id: Uuid::new_v4(),
                    expected_revision: 1,
                    ended_at: postgres_now(),
                })
                .expect("serialize pause resume"),
            ),
            Some("habit-http-pause-resume-001"),
            &access_token,
        ))
        .await
        .expect("HTTP habit resume");
    assert_eq!(resumed.status(), StatusCode::OK);
    assert_eq!(habit_http_body_json(resumed).await["pause"]["revision"], 2);

    let delta = app
        .clone()
        .oneshot(habit_http_request(
            "GET",
            &format!("/v1/habits/occurrences/delta?cursor={baseline_cursor}&limit=200"),
            None,
            None,
            &access_token,
        ))
        .await
        .expect("HTTP lifecycle delta");
    assert_eq!(delta.status(), StatusCode::OK);
    assert_eq!(
        delta.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let delta = habit_http_body_json(delta).await;
    let changes: Vec<HabitDeltaChange> =
        serde_json::from_value(delta["changes"].clone()).expect("strict habit delta changes");
    assert_eq!(changes.len(), 4);
    let [
        HabitDeltaChange::OccurrenceUpsert {
            occurrence: partial,
        },
        HabitDeltaChange::OccurrenceUpsert {
            occurrence: completed,
        },
        HabitDeltaChange::PauseUpsert { pause: started },
        HabitDeltaChange::PauseUpsert { pause: resumed },
    ] = changes.as_slice()
    else {
        panic!("habit lifecycle delta must preserve mutation order: {changes:?}");
    };
    assert_eq!(partial.evidence.id, evidence_id);
    assert_eq!(
        partial.outcome.as_ref().expect("partial outcome").revision,
        1
    );
    assert_eq!(completed.evidence.id, evidence_id);
    assert_eq!(
        completed
            .outcome
            .as_ref()
            .expect("completed outcome")
            .revision,
        2
    );
    assert_eq!(started.id, pause_id);
    assert_eq!(started.revision, 1);
    assert_eq!(resumed.id, pause_id);
    assert_eq!(resumed.revision, 2);
    assert_eq!(delta["has_more"], false);

    drop(app);
    drop(schedules);
    drop(items);
    database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn matching_republication_rejects_malformed_existing_evidence() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; habit PostgreSQL test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool, "habit-republication-owner").await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    ));
    let habit_id = Uuid::new_v4();
    items
        .create(
            habit(habit_id),
            item_idempotency("habit-republication-create", 31),
        )
        .await
        .expect("create canonical habit");
    let schedules = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let access = ScheduleAccess {
        subject: "auth0|habit-republication-owner".to_owned(),
        include_sensitive: true,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };
    let request = compose_request();
    let first_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose first occurrence");
    let planner_occurrence_id = first_preview.plan.occurrences[0].id.0;
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [32; 32],
                input_digest: digest_bytes(&first_preview.input_digest),
                timezone_name: request.timezone_name.clone(),
                manual_placement_approvals: Vec::new(),
                result: first_preview,
                published_at: postgres_now(),
            },
        )
        .await
        .expect("publish first occurrence");
    let republish_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose matching republication");

    // Simulate legacy or privileged corruption in a field that the old equality-only conflict
    // path did not compare. No outcome or version points at this fresh evidence row yet.
    sqlx::query(
        "ALTER TABLE habit_occurrence_evidence \
         DISABLE TRIGGER habit_occurrence_evidence_update_guard",
    )
    .execute(&database.pool)
    .await
    .expect("disable evidence guard in isolated test schema");
    sqlx::query("ALTER TABLE habit_occurrence_publications DISABLE TRIGGER ALL")
        .execute(&database.pool)
        .await
        .expect("disable publication history guards in isolated test schema");
    sqlx::query(
        "UPDATE habit_occurrence_publications SET occurrence_evidence_id = $3 \
         WHERE workspace_id = $1 AND occurrence_evidence_id = ( \
           SELECT id FROM habit_occurrence_evidence \
           WHERE workspace_id = $1 AND habit_id = $2 AND planner_occurrence_id = $3)",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .bind(planner_occurrence_id)
    .execute(&database.pool)
    .await
    .expect("move privileged publication reference with malformed evidence id");
    sqlx::query(
        "UPDATE habit_occurrence_evidence SET id = planner_occurrence_id \
         WHERE workspace_id = $1 AND habit_id = $2 AND planner_occurrence_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(habit_id)
    .bind(planner_occurrence_id)
    .execute(&database.pool)
    .await
    .expect("seed malformed matching evidence");
    sqlx::query("ALTER TABLE habit_occurrence_publications ENABLE TRIGGER ALL")
        .execute(&database.pool)
        .await
        .expect("restore publication history guards");
    sqlx::query(
        "ALTER TABLE habit_occurrence_evidence \
         ENABLE TRIGGER habit_occurrence_evidence_update_guard",
    )
    .execute(&database.pool)
    .await
    .expect("restore evidence guard");

    let republished = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [33; 32],
                input_digest: digest_bytes(&republish_preview.input_digest),
                timezone_name: request.timezone_name,
                manual_placement_approvals: Vec::new(),
                result: republish_preview,
                published_at: postgres_now(),
            },
        )
        .await;
    assert!(
        matches!(republished, Err(SchedulePublicationError::InvalidPayload)),
        "matching content must not promote malformed stored evidence: {republished:?}"
    );

    database.destroy().await;
}

async fn postgres_habit_http_app(
    pool: &PgPool,
    scope: DatabaseScope,
    items: Arc<ItemService>,
    habits: Arc<PostgresHabitRepository>,
    schedules: Arc<PostgresSchedulingRepository>,
) -> (Router, String) {
    let repository = Arc::new(PostgresCredentialRepository::new(pool.clone(), scope));
    let now = postgres_now();
    let enrollment = GeneratedCredential::generate(CredentialKind::Enrollment)
        .expect("generate habit HTTP enrollment credential");
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Habit HTTP PostgreSQL integration".to_owned(),
                scopes: vec![
                    Scope::ItemsRead,
                    Scope::ItemsWrite,
                    Scope::ScheduleRead,
                    Scope::ScheduleSimulate,
                    Scope::SchedulePublish,
                ],
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "habit-http-postgres-integration-1".to_owned(),
                client_capabilities: vec!["schedule-publication-journal-v1".to_owned()],
                created_at: now,
            },
            &enrollment.parsed().expect("parse enrollment credential"),
        )
        .await
        .expect("create habit HTTP device enrollment");
    let access = GeneratedCredential::generate(CredentialKind::DeviceAccess)
        .expect("generate habit HTTP access credential");
    let refresh = GeneratedCredential::generate(CredentialKind::DeviceRefresh)
        .expect("generate habit HTTP refresh credential");
    repository
        .consume_device_enrollment(
            &enrollment.parsed().expect("parse enrollment credential"),
            Uuid::new_v4(),
            &access.parsed().expect("parse access credential"),
            &refresh.parsed().expect("parse refresh credential"),
            now,
        )
        .await
        .expect("consume habit HTTP device enrollment");
    let access_token = access.expose().to_owned();
    let credential_repository: Arc<dyn CredentialRepository> = repository;
    let clock = Arc::new(SystemClock);
    let authenticator: Arc<dyn Authenticator> = Arc::new(RuntimeAuthenticator::new(
        None,
        credential_repository.clone(),
        clock.clone(),
    ));
    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        clock,
        StdDuration::from_hours(24),
    ));
    let readiness = Readiness::default();
    readiness.set_ready(true);
    let app = router(
        AppState::new(proposals, authenticator.clone(), readiness)
            .with_items(items)
            .with_habit_repository(habits)
            .with_postgres_scheduling(schedules, Arc::new(Vec::new()))
            .with_credential_auth(
                credential_repository,
                authenticator,
                AuthMode::CredentialOnly,
            ),
    );
    (app, access_token)
}

fn habit_http_request(
    method: &str,
    uri: &str,
    body: Option<Value>,
    idempotency_key: Option<&str>,
    access_token: &str,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"));
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("idempotency-key", idempotency_key);
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("valid habit HTTP request")
}

async fn habit_http_body_json(response: Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("habit HTTP response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("habit HTTP JSON response")
}

fn habit_http_publish_body(request: &ComposeScheduleRequest, preview: &Value) -> Value {
    json!({
        "idempotency_key": Uuid::new_v4(),
        "expected_input_digest": preview["input_digest"],
        "schedule": request,
    })
}

fn habit_http_scheduled_minutes(preview: &Value, occurrence_id: Uuid) -> i64 {
    preview["plan"]["blocks"]
        .as_array()
        .expect("schedule preview blocks")
        .iter()
        .filter(|block| block["occurrence_id"] == occurrence_id.to_string())
        .map(|block| {
            let start = DateTime::parse_from_rfc3339(
                block["start"].as_str().expect("schedule block start"),
            )
            .expect("RFC 3339 schedule block start");
            let end =
                DateTime::parse_from_rfc3339(block["end"].as_str().expect("schedule block end"))
                    .expect("RFC 3339 schedule block end");
            (end - start).num_minutes()
        })
        .sum()
}

fn habit(id: Uuid) -> NewItem {
    NewItem {
        id,
        is_sensitive: true,
        kind: ItemKind::Habit,
        status: ItemStatus::Planned,
        title: "Private durable habit".to_owned(),
        notes: Some("Private authoring note".to_owned()),
        timezone_name: "Europe/Paris".to_owned(),
        duration_kind: None,
        duration_seconds: Some(1_800),
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: None,
        deadline_date: None,
        deadline_at: None,
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: None,
        recurrence: Some(json!({"type":"daily","times_per_day":1})),
        flexible_constraints: json!({
            "habit_target":{"amount":20,"unit":"pages"},
            "preserves_streak_when_paused":true
        }),
        has_own_effort: None,
        split_policy: SplitPolicy::Indivisible,
        importance: 80,
        urgency: 60,
        parent_id: None,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

fn habit_replacement_with_missed_policy(policy: &str) -> ReplaceItem {
    ReplaceItem {
        is_sensitive: true,
        kind: ItemKind::Habit,
        status: ItemStatus::Planned,
        title: "Private durable habit".to_owned(),
        notes: Some("Private authoring note".to_owned()),
        timezone_name: "Europe/Paris".to_owned(),
        duration_kind: None,
        duration_seconds: Some(1_800),
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: None,
        deadline_date: None,
        deadline_at: None,
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: None,
        recurrence: Some(json!({"type":"daily","times_per_day":1})),
        flexible_constraints: json!({
            "habit_target":{"amount":20,"unit":"pages"},
            "preserves_streak_when_paused":true,
            "habit_missed_policy":policy
        }),
        has_own_effort: None,
        split_policy: SplitPolicy::Indivisible,
        importance: 80,
        urgency: 60,
        parent_id: None,
        sibling_order: 3,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

fn recurring_task(id: Uuid) -> NewItem {
    NewItem {
        id,
        is_sensitive: false,
        kind: ItemKind::Task,
        status: ItemStatus::Planned,
        title: "Recurring task beside a habit".to_owned(),
        notes: None,
        timezone_name: "Europe/Paris".to_owned(),
        duration_kind: None,
        duration_seconds: Some(600),
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: None,
        deadline_date: None,
        deadline_at: None,
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: None,
        recurrence: Some(json!({"type":"daily","times_per_day":1})),
        flexible_constraints: json!({}),
        has_own_effort: None,
        split_policy: SplitPolicy::Indivisible,
        importance: 50,
        urgency: 50,
        parent_id: None,
        sibling_order: 1,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

fn compose_request() -> ComposeScheduleRequest {
    serde_json::from_value(json!({
        "as_of":"2025-10-26T05:00:00Z",
        "horizon_start":"2025-10-25T22:00:00Z",
        "horizon_end":"2025-10-26T23:00:00Z",
        "timezone_name":"Europe/Paris",
        "availability":[{
            "start":"2025-10-26T06:00:00Z",
            "end":"2025-10-26T18:00:00Z",
            "contexts":[],
            "location":null,
            "energy":"deep"
        }],
        "fixed_blocks":[],
        "previous_assignments":[],
        "manual_placements":[],
        "manual_placement_releases":[],
        "config":{
            "slot_granularity_minutes":5,
            "stability_weight":4,
            "default_soft_weight":100
        },
        "recurrence_context":{}
    }))
    .expect("compose request")
}

fn compose_request_for_local_day(date: &str) -> ComposeScheduleRequest {
    let date = date.parse::<chrono::NaiveDate>().expect("local date");
    let local_start = date
        .and_hms_opt(0, 0, 0)
        .expect("local midnight")
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .expect("unambiguous local midnight")
        .with_timezone(&Utc);
    let local_end = date
        .succ_opt()
        .expect("next date")
        .and_hms_opt(0, 0, 0)
        .expect("next local midnight")
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .expect("unambiguous next local midnight")
        .with_timezone(&Utc);
    let availability_start = date
        .and_hms_opt(6, 0, 0)
        .expect("availability start")
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .expect("unambiguous availability start")
        .with_timezone(&Utc);
    let availability_end = date
        .and_hms_opt(20, 0, 0)
        .expect("availability end")
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .expect("unambiguous availability end")
        .with_timezone(&Utc);
    compose_request_for_bounds(
        local_start,
        local_start,
        local_end,
        availability_start,
        availability_end,
    )
}

fn compose_request_for_window(
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> ComposeScheduleRequest {
    let local_start = window_start
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("carry horizon local midnight")
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .expect("unambiguous carry horizon start")
        .with_timezone(&Utc);
    let local_end = window_end
        .with_timezone(&chrono_tz::Europe::Paris)
        .date_naive()
        .succ_opt()
        .expect("carry horizon next date")
        .and_hms_opt(0, 0, 0)
        .expect("carry horizon next midnight")
        .and_local_timezone(chrono_tz::Europe::Paris)
        .single()
        .expect("unambiguous carry horizon end")
        .with_timezone(&Utc);
    compose_request_for_bounds(
        window_start,
        local_start,
        local_end,
        window_start,
        window_end,
    )
}

fn compose_request_for_bounds(
    as_of: DateTime<Utc>,
    horizon_start: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
    availability_start: DateTime<Utc>,
    availability_end: DateTime<Utc>,
) -> ComposeScheduleRequest {
    serde_json::from_value(json!({
        "as_of": as_of,
        "horizon_start": horizon_start,
        "horizon_end": horizon_end,
        "timezone_name":"Europe/Paris",
        "availability":[{
            "start": availability_start,
            "end": availability_end,
            "contexts":[],
            "location":null,
            "energy":"deep"
        }],
        "fixed_blocks":[],
        "previous_assignments":[],
        "manual_placements":[],
        "manual_placement_releases":[],
        "config":{
            "slot_granularity_minutes":5,
            "stability_weight":4,
            "default_soft_weight":100
        },
        "recurrence_context":{}
    }))
    .expect("dynamic compose request")
}

fn partial_command(operation_id: Uuid, expected_revision: u64, note: &str) -> HabitOutcomeCommand {
    HabitOutcomeCommand {
        operation_id,
        expected_revision,
        outcome: HabitOutcomeInput {
            status: HabitOutcomeStatus::Partial,
            progress_basis_points: 5_000,
            quantity: Some(-2),
            unit: Some("pages".to_owned()),
            actual_seconds: Some(900),
            note: Some(note.to_owned()),
            occurred_at: postgres_now(),
        },
    }
}

fn terminal_command(
    operation_id: Uuid,
    expected_revision: u64,
    status: HabitOutcomeStatus,
    progress_basis_points: u16,
) -> HabitOutcomeCommand {
    HabitOutcomeCommand {
        operation_id,
        expected_revision,
        outcome: HabitOutcomeInput {
            status,
            progress_basis_points,
            quantity: Some(-3),
            unit: Some("pages".to_owned()),
            actual_seconds: Some(1_200),
            note: Some(PRIVATE_NOTE.to_owned()),
            occurred_at: postgres_now(),
        },
    }
}

fn item_idempotency(key: &str, marker: u8) -> IdempotencyKey {
    IdempotencyKey {
        key: key.to_owned(),
        fingerprint: [marker; 32],
    }
}

fn habit_key(key: &str, actor_session_id: Option<Uuid>) -> HabitIdempotencyKey {
    HabitIdempotencyKey {
        key: key.to_owned(),
        actor_session_id,
    }
}

fn postgres_now() -> DateTime<Utc> {
    Utc::now().with_nanosecond(0).expect("whole second")
}

fn digest_bytes(value: &str) -> [u8; 32] {
    let encoded = value.strip_prefix("sha256:").expect("digest prefix");
    assert_eq!(encoded.len(), 64);
    let mut output = [0_u8; 32];
    for (index, chunk) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(chunk).expect("hex utf8");
        output[index] = u8::from_str_radix(text, 16).expect("hex byte");
    }
    output
}

async fn wait_for_blocked_queries(pool: &PgPool, blocker_pid: i32, minimum: i64) {
    tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let blocked: i64 = sqlx::query_scalar(
                "WITH RECURSIVE blocked(pid) AS ( \
                   SELECT activity.pid FROM pg_stat_activity AS activity \
                   WHERE $1 = ANY(pg_blocking_pids(activity.pid)) \
                   UNION \
                   SELECT activity.pid FROM pg_stat_activity AS activity \
                   JOIN blocked AS prior ON prior.pid = ANY(pg_blocking_pids(activity.pid)) \
                 ) SELECT COUNT(*) FROM blocked",
            )
            .bind(blocker_pid)
            .fetch_one(pool)
            .await
            .expect("inspect PostgreSQL lock waiters");
            if blocked >= minimum {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publication reached the habit projection lock");
}

async fn seed_scope(pool: &PgPool, label: &str) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) VALUES ($1,$2,$3,'Europe/Paris')",
    )
    .bind(scope.user_id)
    .bind(format!("auth0|{label}-{}", scope.user_id.simple()))
    .bind(label)
    .execute(pool)
    .await
    .expect("seed user");
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) VALUES ($1,$2,$3,$4,'Europe/Paris')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("{label}-{}", scope.workspace_id.simple()))
    .bind(label)
    .execute(pool)
    .await
    .expect("seed workspace");
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1,$2,'owner')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("seed membership");
    scope
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
        let schema = format!("dayweave_habits_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated schema");
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
            .expect("connect isolated pool");
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
            .expect("drop isolated schema");
        self.admin.close().await;
    }
}
