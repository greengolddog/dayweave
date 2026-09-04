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
        HabitOccurrence, HabitOutcomeCommand, HabitOutcomeInput, HabitOutcomeStatus,
        HabitPauseResumeCommand, HabitPauseStartCommand, HabitRepository, HabitRepositoryError,
        HabitService,
    },
    http::router,
    items::{IdempotencyKey, ItemKind, ItemService, ItemStatus, NewItem, SplitPolicy},
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

    let operation_id = Uuid::new_v4();
    let partial = service
        .put_outcome(
            habit_id,
            evidence_id,
            partial_command(operation_id, 0, PRIVATE_NOTE),
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
            partial_command(operation_id, 0, PRIVATE_NOTE),
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
            partial_command(operation_id, 0, PRIVATE_NOTE),
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
        "INSERT INTO habit_changes (workspace_id, change_kind, entity_id, entity_revision, \
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
