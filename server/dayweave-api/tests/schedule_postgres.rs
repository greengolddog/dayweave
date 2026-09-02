use std::{str::FromStr, sync::Arc, time::Duration as StdDuration};

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use dayweave_api::{
    AppState,
    auth::{Authenticator, RuntimeAuthenticator, Scope},
    config::AuthMode,
    credential_auth::{
        CredentialKind, CredentialRepository, DEVICE_CLIENT_CONTRACT_VERSION, DeviceClientKind,
        DeviceEnrollmentSpec, GeneratedCredential,
    },
    execution::{
        DeferAssessmentRequest, DeferExecution, ExecutionCommand, ExecutionIdempotencyKey,
        ExecutionService, PauseExecution, StartExecution,
    },
    http::router,
    items::{
        IdempotencyKey, Item, ItemKind, ItemService, ItemStatus, NewItem, ReplaceItem, SplitPolicy,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresCredentialRepository, PostgresExecutionRepository,
        PostgresItemRepository,
    },
    proposals::{
        InMemoryProposalRepository, Proposal, ProposalChangeSet, ProposalCommand, ProposalKind,
        ProposalRepository, ProposalService, ProposalSource, SystemClock,
    },
    readiness::Readiness,
    scheduling::{
        ComposeScheduleRequest, ConflictQuery, ItemSearchQuery, ManualPlacementAssignmentInput,
        ManualPlacementInput, ManualPlacementReleaseInput, PlanOperation, PlanOperationKind,
        PlanningSimulationPort, PostgresSchedulingRepository, PreviousAssignmentInput,
        PreviousBlockInput, ProposalSubmissionError, ProposalSubmissionPort,
        ProposalSubmissionSpec, PublishScheduleSpec, ScheduleAccess, ScheduleDetail,
        ScheduleInvalidationConfig, SchedulePublicationError, ScheduleQuery, ScheduleQueryPort,
        SchedulingPortError, SimulationRequest, compose_canonical_schedule,
        simulation_request_digest, simulation_request_hash,
    },
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tower::ServiceExt as _;
use uuid::Uuid;

use tokio::time::timeout;

type StoredSimulationEvidenceRow = (
    bool,
    bool,
    Vec<u8>,
    Vec<u8>,
    Vec<u8>,
    String,
    Option<Vec<u8>>,
);
type SubmissionProofRow = (
    Vec<u8>,
    Vec<u8>,
    i16,
    Vec<u8>,
    String,
    Option<Vec<u8>>,
    Vec<u8>,
    bool,
);
type DeferredBindingRow = (
    Uuid,
    Uuid,
    Uuid,
    i64,
    i64,
    Option<Uuid>,
    i32,
    i64,
    chrono::DateTime<Utc>,
    chrono::DateTime<Utc>,
);

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn publication_queries_and_simulations_are_durable_scoped_and_race_safe() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; schedule PostgreSQL test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    assert_device_contract_scope_coupling(&test_database.pool, scope).await;
    let item_repository = Arc::new(PostgresItemRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let items = Arc::new(ItemService::new(item_repository, Arc::new(SystemClock)));
    let schedules = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let access = owner_access(scope, "auth0|schedule-owner");

    assert_eq!(
        schedules
            .get_schedule(
                &access,
                ScheduleQuery {
                    start: "2026-09-01T00:00:00Z".parse().unwrap(),
                    end: "2026-09-02T00:00:00Z".parse().unwrap(),
                    detail: ScheduleDetail::Full,
                },
            )
            .await,
        Err(SchedulingPortError::NotFound)
    );

    let public_goal_a = Uuid::new_v4();
    let public_goal_b = Uuid::new_v4();
    let public_task = Uuid::new_v4();
    let private_parent = Uuid::new_v4();
    let private_child = Uuid::new_v4();
    for (item, marker) in [
        (goal(public_goal_a, "Goal A", false, None), 1),
        (goal(public_goal_b, "Goal B", false, None), 2),
        (
            task(
                public_task,
                "Public multi-goal task",
                false,
                None,
                json!({"goal_ids": [public_goal_a, public_goal_b]}),
            ),
            3,
        ),
        (goal(private_parent, "Private parent", true, None), 4),
        (
            task(
                private_child,
                "Private inherited child",
                false,
                Some(private_parent),
                json!({}),
            ),
            5,
        ),
    ] {
        items
            .create(item, idempotency(marker))
            .await
            .expect("create canonical item");
    }

    let conflict_canary = Uuid::new_v4();
    let mut impossible = task(
        conflict_canary,
        "SYNTHETIC-PUBLIC-CONFLICT-CANARY",
        false,
        None,
        json!({}),
    );
    impossible.duration_seconds = Some(20 * 60 * 60);
    items
        .create(impossible, idempotency(8))
        .await
        .expect("create deterministic public conflict canary");

    let control_canary = Uuid::new_v4();
    items
        .create(
            task(
                control_canary,
                "SYNTHETIC-REJECTED-TITLE\u{7}CANARY",
                false,
                None,
                json!({"unknown_constraint": true}),
            ),
            idempotency(6),
        )
        .await
        .expect("create rejected-item control canary");

    let (app, device_access_token) =
        credential_publish_app(&test_database.pool, scope, items.clone(), schedules.clone()).await;
    let mut request = compose_request();
    request.fixed_blocks[0].id = public_task;
    let control_rejected = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/preview",
            &serde_json::to_value(request.clone()).unwrap(),
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(control_rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        publication_counts(&test_database.pool, scope).await,
        (0, 0, 0, 0, 0),
        "a rejected-item control canary creates no publication rows"
    );
    items
        .trash(control_canary, 1, idempotency(7))
        .await
        .expect("remove rejected-item control canary from the active graph");

    let preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose preview");
    let expected_digest = preview.input_digest.clone();

    // The public HTTP contract always returns 200 for both the first commit and
    // exact durable replay, distinguished solely by `replayed`.
    let mut unpersistable = request.clone();
    unpersistable.fixed_blocks[0].title = "é".repeat(501);
    let rejected = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/preview",
            &serde_json::to_value(unpersistable).unwrap(),
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        publication_counts(&test_database.pool, scope).await,
        (0, 0, 0, 0, 0),
        "a preview-time persistence rejection creates no publication rows"
    );
    let key = Uuid::new_v4();
    let publish_body = json!({
        "idempotency_key": key,
        "expected_input_digest": expected_digest,
        "schedule": request,
    });
    let first = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &publish_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = body_json(first).await;
    assert_eq!(first["replayed"], false);
    assert_eq!(first["revision"]["revision_number"], 1);
    assert_eq!(
        first["revision"]["horizon_start"]
            .as_str()
            .unwrap()
            .parse::<chrono::DateTime<Utc>>()
            .unwrap(),
        request.horizon_start
    );
    assert_eq!(
        first["revision"]["horizon_end"]
            .as_str()
            .unwrap()
            .parse::<chrono::DateTime<Utc>>()
            .unwrap(),
        request.horizon_end
    );
    let first_revision_id = Uuid::parse_str(first["revision"]["id"].as_str().unwrap()).unwrap();
    let first_revision = first["revision"]["revision"].as_str().unwrap().to_owned();

    let replay = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &publish_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay = body_json(replay).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"]["revision"], first_revision);
    assert_eq!(replay["revision"], first["revision"]);

    let mut conflicting_publish_body = publish_body.clone();
    conflicting_publish_body["expected_input_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    let idempotency_conflict = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &conflicting_publish_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(idempotency_conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(idempotency_conflict).await["error"]["code"],
        "schedule_publication_idempotency_conflict"
    );

    let mut stale_publish_body = publish_body.clone();
    stale_publish_body["idempotency_key"] = json!(Uuid::new_v4());
    stale_publish_body["expected_input_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    let stale_publication = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &stale_publish_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(stale_publication.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(stale_publication).await["error"]["code"],
        "schedule_publication_stale"
    );

    let stored_request_hash: Vec<u8> = sqlx::query_scalar(
        "SELECT request_hash FROM schedule_publication_requests WHERE workspace_id = $1 \
         AND user_id = $2 AND idempotency_key = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(key)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    let request_hash: [u8; 32] = stored_request_hash.try_into().unwrap();
    let stored_publication_correlation: String = sqlx::query_scalar(
        "SELECT metadata->>'idempotency_key' FROM audit_operations WHERE workspace_id = $1 \
         AND entity_id = $2 AND operation_type = 'schedule.published'",
    )
    .bind(scope.workspace_id)
    .bind(first_revision_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(stored_publication_correlation, key.to_string());
    let input_digest = digest_bytes(&preview.input_digest);
    assert!(matches!(
        schedules.publication_receipt(&access, key, &[99; 32]).await,
        Err(SchedulePublicationError::IdempotencyConflict)
    ));

    // Publication changes the private current-revision evidence. A preview
    // captured before that seal cannot be rebound under a fresh key.
    let fresh_key = Uuid::new_v4();
    let stale_previous_publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: fresh_key,
                request_hash,
                input_digest,
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: preview.clone(),
                published_at: Utc::now(),
            },
        )
        .await;
    assert!(matches!(
        stale_previous_publication,
        Err(SchedulePublicationError::StaleComposition)
    ));
    let revision_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert_eq!(revision_count, 1);
    assert_schedule_seal(&test_database.pool, scope, first_revision_id).await;

    let schedule = schedules
        .get_schedule(
            &access,
            ScheduleQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
                detail: ScheduleDetail::Full,
            },
        )
        .await
        .unwrap();
    assert_eq!(schedule.revision, first_revision);
    assert!(schedule.blocks.iter().any(|block| block.kind == "planned"));
    assert!(schedule.redacted_count >= 1);
    assert!(schedule.blocks.iter().any(|block| block.redacted));
    assert!(
        schedule
            .blocks
            .iter()
            .filter(|block| block.redacted)
            .all(|block| block.id.is_none() && block.item_id.is_none() && block.title.is_none())
    );

    let microsecond_start = "2026-09-01T00:00:00.000001Z".parse().unwrap();
    let microsecond_end = "2026-09-02T00:00:00.000001Z".parse().unwrap();
    assert!(
        schedules
            .get_schedule(
                &access,
                ScheduleQuery {
                    start: microsecond_start,
                    end: microsecond_end,
                    detail: ScheduleDetail::Summary,
                },
            )
            .await
            .is_ok()
    );
    let nanosecond_start = "2026-09-01T00:00:00.000000001Z".parse().unwrap();
    let nanosecond_end = "2026-09-02T00:00:00.000000001Z".parse().unwrap();
    assert!(matches!(
        schedules
            .get_schedule(
                &access,
                ScheduleQuery {
                    start: nanosecond_start,
                    end: microsecond_end,
                    detail: ScheduleDetail::Summary,
                },
            )
            .await,
        Err(SchedulingPortError::InvalidQuery(_))
    ));
    for (start, end) in [(Some(nanosecond_start), None), (None, Some(nanosecond_end))] {
        assert!(matches!(
            schedules
                .search_items(
                    &access,
                    ItemSearchQuery {
                        text: None,
                        status: None,
                        kind: None,
                        project_id: None,
                        goal_id: None,
                        start,
                        end,
                        limit: 20,
                    },
                )
                .await,
            Err(SchedulingPortError::InvalidQuery(_))
        ));
    }
    assert!(matches!(
        schedules
            .get_conflicts(
                &access,
                ConflictQuery {
                    start: microsecond_start,
                    end: nanosecond_end,
                },
            )
            .await,
        Err(SchedulingPortError::InvalidQuery(_))
    ));

    let public_block = schedule
        .blocks
        .iter()
        .find(|block| block.item_id.as_deref() == Some(&public_task.to_string()))
        .expect("public task is scheduled")
        .clone();
    let preview_public_block = preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block
                .item_id
                .is_some_and(|item_id| item_id.0 == public_task)
        })
        .expect("preview contains the public task block");
    let preview_start = chrono::DateTime::<Utc>::from_timestamp_micros(
        i64::try_from(preview_public_block.start.unix_timestamp_nanos() / 1_000).unwrap(),
    )
    .unwrap();
    let preview_end = chrono::DateTime::<Utc>::from_timestamp_micros(
        i64::try_from(preview_public_block.end.unix_timestamp_nanos() / 1_000).unwrap(),
    )
    .unwrap();
    assert_eq!(public_block.start, preview_start);
    assert_eq!(public_block.end, preview_end);
    let explanation = schedules
        .explain_placement(&access, public_block.id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(explanation.block_id, public_block.id.unwrap());
    let private_block_id: Uuid = sqlx::query_scalar(
        "SELECT source_block_id FROM schedule_blocks WHERE workspace_id = $1 \
         AND item_id = $2 ORDER BY ordinal LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(private_child)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    let (private_block_start, private_block_end): (
        chrono::DateTime<Utc>,
        chrono::DateTime<Utc>,
    ) = sqlx::query_as(
        "SELECT starts_at, ends_at FROM schedule_blocks WHERE workspace_id = $1 AND source_block_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(private_block_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(
        schedules
            .explain_placement(&access, &private_block_id.to_string())
            .await,
        Err(SchedulingPortError::NotFound)
    );
    let conflicts = schedules
        .get_conflicts(
            &access,
            ConflictQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    let baseline_conflict_redacted = conflicts.redacted_count;
    assert!(conflicts.redacted_count >= 1);
    assert!(
        conflicts
            .conflicts
            .iter()
            .all(|conflict| !conflict.sensitive)
    );
    let conflict_canary_id = conflicts
        .conflicts
        .iter()
        .find(|conflict| {
            conflict
                .related_item_ids
                .contains(&conflict_canary.to_string())
        })
        .map(|conflict| conflict.id.clone())
        .expect("the impossible public item produces a visible conflict canary");

    // Privacy is monotonic between publications: historical sensitivity is
    // never relaxed, while a current item/ancestor change tightens every read
    // immediately without exposing item or block identifiers.
    set_item_sensitivity(&items, conflict_canary, true, 79).await;
    let hidden_canary = schedules
        .get_conflicts(
            &access,
            ConflictQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(hidden_canary.redacted_count > baseline_conflict_redacted);
    assert!(
        hidden_canary
            .conflicts
            .iter()
            .all(|conflict| conflict.id != conflict_canary_id)
    );
    set_item_sensitivity(&items, conflict_canary, false, 80).await;

    let private_item_request = SimulationRequest {
        base_revision: first_revision.clone(),
        operations: vec![operation(
            PlanOperationKind::DeleteItem,
            Some(&private_child.to_string()),
            json!({}),
        )],
        assumptions: Vec::new(),
    };
    let unchanged_private = schedules
        .simulate(&access, private_item_request.clone())
        .await
        .unwrap();
    assert!(unchanged_private.moved_blocks.is_empty());
    assert_eq!(unchanged_private.warnings[0].code, "redacted_item");
    assert!(unchanged_private.warnings[0].related_ids.is_empty());
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &private_item_request,
        &unchanged_private,
        "privacy-evidence-unchanged",
        [207; 32],
    )
    .await;

    set_item_sensitivity(&items, public_task, true, 70).await;
    set_item_sensitivity(&items, private_parent, false, 71).await;
    let historically_private = schedules
        .simulate(&access, private_item_request.clone())
        .await
        .unwrap();
    assert_eq!(historically_private.warnings[0].code, "redacted_item");
    assert!(historically_private.warnings[0].related_ids.is_empty());
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &private_item_request,
        &historically_private,
        "privacy-evidence-historical",
        [208; 32],
    )
    .await;
    let tightened = schedules
        .get_schedule(
            &access,
            ScheduleQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
                detail: ScheduleDetail::Full,
            },
        )
        .await
        .unwrap();
    let tightened_public = tightened
        .blocks
        .iter()
        .find(|block| block.start == public_block.start && block.end == public_block.end)
        .unwrap();
    assert!(tightened_public.redacted);
    assert!(tightened_public.id.is_none() && tightened_public.item_id.is_none());
    assert_eq!(
        schedules
            .explain_placement(&access, explanation.block_id.as_str())
            .await,
        Err(SchedulingPortError::NotFound)
    );
    let tightened_conflicts = schedules
        .get_conflicts(
            &access,
            ConflictQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(tightened_conflicts.redacted_count >= baseline_conflict_redacted);
    assert!(
        tightened_conflicts
            .conflicts
            .iter()
            .all(|conflict| !conflict.related_item_ids.contains(&public_task.to_string()))
    );
    let redacted_move_request = SimulationRequest {
        base_revision: tightened.revision.clone(),
        operations: vec![operation(
            PlanOperationKind::MoveBlock,
            Some(explanation.block_id.as_str()),
            json!({"start": "2026-09-01T13:00:00Z"}),
        )],
        assumptions: Vec::new(),
    };
    let redacted_simulation = schedules
        .simulate(&access, redacted_move_request.clone())
        .await
        .unwrap();
    assert!(redacted_simulation.moved_blocks.is_empty());
    assert_eq!(redacted_simulation.warnings[0].code, "redacted_block");
    assert!(redacted_simulation.warnings[0].related_ids.is_empty());

    set_item_sensitivity(&items, public_task, false, 72).await;
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &redacted_move_request,
        &redacted_simulation,
        "privacy-evidence-block",
        [209; 32],
    )
    .await;
    sqlx::query(
        "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id) VALUES ($1, $2, $3)",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    set_item_sensitivity(&items, public_goal_a, true, 73).await;
    assert!(
        schedule_item_is_redacted(&schedules, &access, public_block.start, public_block.end).await
    );
    set_item_sensitivity(&items, public_goal_a, false, 74).await;
    sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id = $2")
        .bind(scope.workspace_id)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();

    // A sensitive->public flip does not relax historical evidence.
    assert!(
        schedule_item_is_redacted(&schedules, &access, private_block_start, private_block_end,)
            .await
    );

    // Trashed items, orphaned ancestry, and cycles cannot reopen evidence.
    sqlx::query(
        "UPDATE items SET trashed_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(
        schedule_item_is_redacted(&schedules, &access, public_block.start, public_block.end).await
    );
    sqlx::query("UPDATE items SET trashed_at = NULL WHERE workspace_id = $1 AND id = $2")
        .bind(scope.workspace_id)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id) \
         VALUES ($1, $2, $3), ($1, $3, $2)",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(
        schedule_item_is_redacted(&schedules, &access, public_block.start, public_block.end).await
    );
    sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id IN ($2, $3)")
        .bind(scope.workspace_id)
        .bind(public_goal_a)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id) VALUES ($1, $2, $3)",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE items SET trashed_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(
        schedule_item_is_redacted(&schedules, &access, public_block.start, public_block.end).await
    );
    sqlx::query("UPDATE items SET trashed_at = NULL WHERE workspace_id = $1 AND id = $2")
        .bind(scope.workspace_id)
        .bind(public_goal_a)
        .execute(&test_database.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id = $2")
        .bind(scope.workspace_id)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();

    // Fixed-block identifiers are caller supplied and may collide with item
    // identifiers. A later privacy tightening must still treat the target as
    // an item, reject both consume and proposal submission, and roll back all
    // capability/proposal evidence.
    let collision_request = SimulationRequest {
        base_revision: first_revision.clone(),
        operations: vec![operation(
            PlanOperationKind::DeleteItem,
            Some(&public_task.to_string()),
            json!({}),
        )],
        assumptions: Vec::new(),
    };
    let direct_collision = schedules
        .simulate(&access, collision_request.clone())
        .await
        .unwrap();
    set_item_sensitivity(&items, public_task, true, 75).await;
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &collision_request,
        &direct_collision,
        "privacy-collision-direct",
        [201; 32],
    )
    .await;
    set_item_sensitivity(&items, public_task, false, 76).await;

    sqlx::query(
        "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id) VALUES ($1, $2, $3)",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    let ancestor_collision = schedules
        .simulate(&access, collision_request.clone())
        .await
        .unwrap();
    set_item_sensitivity(&items, public_goal_a, true, 77).await;
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &collision_request,
        &ancestor_collision,
        "privacy-collision-ancestor",
        [202; 32],
    )
    .await;
    set_item_sensitivity(&items, public_goal_a, false, 78).await;
    sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id = $2")
        .bind(scope.workspace_id)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();

    let cycle_collision = schedules
        .simulate(&access, collision_request.clone())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id) \
         VALUES ($1, $2, $3), ($1, $3, $2)",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &collision_request,
        &cycle_collision,
        "privacy-collision-cycle",
        [203; 32],
    )
    .await;
    sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id IN ($2, $3)")
        .bind(scope.workspace_id)
        .bind(public_goal_a)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO item_hierarchy (workspace_id, parent_item_id, child_item_id) VALUES ($1, $2, $3)",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .bind(public_task)
    .execute(&test_database.pool)
    .await
    .unwrap();
    let orphan_collision = schedules
        .simulate(&access, collision_request.clone())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE items SET trashed_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(public_goal_a)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_private_simulation_rejected(
        &test_database.pool,
        scope,
        &schedules,
        &access,
        &collision_request,
        &orphan_collision,
        "privacy-collision-orphan",
        [204; 32],
    )
    .await;
    sqlx::query("UPDATE items SET trashed_at = NULL WHERE workspace_id = $1 AND id = $2")
        .bind(scope.workspace_id)
        .bind(public_goal_a)
        .execute(&test_database.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM item_hierarchy WHERE workspace_id = $1 AND child_item_id = $2")
        .bind(scope.workspace_id)
        .bind(public_task)
        .execute(&test_database.pool)
        .await
        .unwrap();

    let goal_b = public_goal_b.to_string();
    let search = schedules
        .search_items(
            &access,
            ItemSearchQuery {
                text: None,
                status: None,
                kind: None,
                project_id: None,
                goal_id: Some(goal_b.clone()),
                start: None,
                end: None,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert_eq!(search.redacted_count, 0);
    assert!(
        search
            .items
            .iter()
            .any(|item| item.id == public_task.to_string()
                && item.goal_id.as_deref() == Some(&goal_b))
    );
    assert!(
        search
            .items
            .iter()
            .all(|item| item.id != private_child.to_string())
    );

    for denied in [
        ScheduleAccess {
            subject: "legacy-token".to_owned(),
            include_sensitive: false,
            workspace_id: None,
            user_id: None,
        },
        ScheduleAccess {
            subject: "cross-scope".to_owned(),
            include_sensitive: false,
            workspace_id: Some(Uuid::new_v4()),
            user_id: Some(scope.user_id),
        },
    ] {
        assert_eq!(
            schedules
                .search_items(
                    &denied,
                    ItemSearchQuery {
                        text: None,
                        status: None,
                        kind: None,
                        project_id: None,
                        goal_id: None,
                        start: None,
                        end: None,
                        limit: 1,
                    },
                )
                .await,
            Err(SchedulingPortError::NotFound)
        );
        assert!(matches!(
            schedules
                .publication_receipt(&denied, key, &request_hash)
                .await,
            Err(SchedulePublicationError::AccessDenied)
        ));
    }

    let current_public = items.get(public_task).await.unwrap();
    items
        .replace(
            public_task,
            current_public.revision,
            ReplaceItem {
                is_sensitive: current_public.is_sensitive,
                kind: current_public.kind,
                status: current_public.status,
                title: "Public task changed after lost response".to_owned(),
                notes: current_public.notes,
                timezone_name: current_public.timezone_name,
                duration_seconds: current_public.duration_seconds,
                deadline_at: current_public.deadline_at,
                earliest_start_at: current_public.earliest_start_at,
                recurrence: current_public.recurrence,
                flexible_constraints: current_public.flexible_constraints,
                split_policy: current_public.split_policy,
                importance: current_public.importance,
                urgency: current_public.urgency,
                parent_id: current_public.parent_id,
                sibling_order: current_public.sibling_order,
            },
            idempotency(10),
        )
        .await
        .unwrap();

    // Lost response recovery happens before recomposition: an old exact key
    // still returns its old receipt after later item/current-schedule changes.
    let old = schedules
        .publication_receipt(&access, key, &request_hash)
        .await
        .unwrap()
        .unwrap();
    assert!(old.replayed);
    assert_eq!(old.revision.revision, first_revision);
    let before_stale_counts = publication_counts(&test_database.pool, scope).await;
    let stale = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [31; 32],
                input_digest,
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: preview,
                published_at: Utc::now(),
            },
        )
        .await;
    assert!(matches!(
        stale,
        Err(SchedulePublicationError::StaleComposition)
    ));
    assert_eq!(
        publication_counts(&test_database.pool, scope).await,
        before_stale_counts
    );

    let pre_update_schedule = schedules
        .get_schedule(
            &access,
            ScheduleQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
                detail: ScheduleDetail::Summary,
            },
        )
        .await
        .unwrap();
    let stale_simulation = schedules
        .simulate(
            &access,
            SimulationRequest {
                base_revision: pre_update_schedule.revision,
                operations: vec![operation(
                    PlanOperationKind::CreateItem,
                    None,
                    create_task_parameters("Stale simulation proposal"),
                )],
                assumptions: Vec::new(),
            },
        )
        .await
        .unwrap();

    let next_request = compose_request();
    let next_preview = compose_canonical_schedule(&items, &schedules, next_request)
        .await
        .unwrap();
    let legacy_draft_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, horizon_start, \
         horizon_end, timezone_name, solver_version, input_digest, created_by_user_id, created_at) \
         VALUES ($1, $2, 7, 'draft', $3, $4, 'Europe/Madrid', 'legacy-draft', $5, $6, $7)",
    )
    .bind(legacy_draft_id)
    .bind(scope.workspace_id)
    .bind(
        "2026-09-01T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap(),
    )
    .bind(
        "2026-09-02T00:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap(),
    )
    .bind(vec![42_u8; 32])
    .bind(scope.user_id)
    .bind(Utc::now())
    .execute(&test_database.pool)
    .await
    .unwrap();
    let next = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [41; 32],
                input_digest: digest_bytes(&next_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: next_preview.clone(),
                published_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert_eq!(next.revision.revision_number, 8);
    let audited_parent_revision: Option<i64> = sqlx::query_scalar(
        "SELECT base_revision FROM audit_operations WHERE workspace_id = $1 \
         AND entity_type = 'schedule_revision' AND entity_id = $2 \
         AND operation_type = 'schedule.published'",
    )
    .bind(scope.workspace_id)
    .bind(next.revision.id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(audited_parent_revision, Some(1));
    assert!(matches!(
        schedules
            .consume_simulation(
                &access,
                &stale_simulation.simulation_token,
                &stale_simulation.request_digest,
            )
            .await,
        Err(SchedulingPortError::RevisionConflict { current_revision })
            if current_revision == next.revision.revision
    ));

    let concurrency_repository = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let exact_preview = compose_canonical_schedule(&items, &schedules, compose_request())
        .await
        .unwrap();
    let exact_key = Uuid::new_v4();
    let exact_spec = PublishScheduleSpec {
        idempotency_key: exact_key,
        request_hash: [91; 32],
        input_digest: digest_bytes(&exact_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: exact_preview,
        published_at: Utc::now(),
    };
    let (exact_left, exact_right) = publish_concurrently_after_lock_barrier(
        &test_database.pool,
        scope,
        concurrency_repository.clone(),
        &access,
        exact_spec.clone(),
        exact_spec,
    )
    .await;
    let exact_left = exact_left.unwrap();
    let exact_right = exact_right.unwrap();
    assert_eq!(
        usize::from(exact_left.replayed) + usize::from(exact_right.replayed),
        1
    );
    assert_eq!(exact_left.revision.id, exact_right.revision.id);

    let conflict_preview = compose_canonical_schedule(&items, &schedules, compose_request())
        .await
        .unwrap();
    let conflicting_key = Uuid::new_v4();
    let conflict_left = PublishScheduleSpec {
        idempotency_key: conflicting_key,
        request_hash: [92; 32],
        input_digest: digest_bytes(&conflict_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: conflict_preview,
        published_at: Utc::now(),
    };
    let mut conflict_right = conflict_left.clone();
    conflict_right.request_hash = [93; 32];
    let (conflict_left, conflict_right) = publish_concurrently_after_lock_barrier(
        &test_database.pool,
        scope,
        concurrency_repository.clone(),
        &access,
        conflict_left,
        conflict_right,
    )
    .await;
    assert_eq!(
        usize::from(conflict_left.is_ok()) + usize::from(conflict_right.is_ok()),
        1
    );
    assert!(matches!(
        (conflict_left, conflict_right),
        (Ok(_), Err(SchedulePublicationError::IdempotencyConflict))
            | (Err(SchedulePublicationError::IdempotencyConflict), Ok(_))
    ));

    let same_content_preview = compose_canonical_schedule(&items, &schedules, compose_request())
        .await
        .unwrap();
    let same_content_left = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [94; 32],
        input_digest: digest_bytes(&same_content_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: same_content_preview,
        published_at: Utc::now(),
    };
    let mut same_content_right = same_content_left.clone();
    same_content_right.idempotency_key = Uuid::new_v4();
    same_content_right.request_hash = [95; 32];
    let (same_content_left, same_content_right) = publish_concurrently_after_lock_barrier(
        &test_database.pool,
        scope,
        concurrency_repository.clone(),
        &access,
        same_content_left,
        same_content_right,
    )
    .await;
    assert_eq!(
        usize::from(same_content_left.is_ok()) + usize::from(same_content_right.is_ok()),
        1
    );
    assert!(matches!(
        (same_content_left, same_content_right),
        (Ok(_), Err(SchedulePublicationError::StaleComposition))
            | (Err(SchedulePublicationError::StaleComposition), Ok(_))
    ));

    let different_left_result = compose_canonical_schedule(&items, &schedules, compose_request())
        .await
        .unwrap();
    let mut different_right_request = compose_request();
    different_right_request.fixed_blocks[0].title =
        "Explicitly different concurrent publication".to_owned();
    let different_right_result =
        compose_canonical_schedule(&items, &schedules, different_right_request)
            .await
            .unwrap();
    let different_left = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [96; 32],
        input_digest: digest_bytes(&different_left_result.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: different_left_result,
        published_at: "2099-01-01T00:00:00Z".parse().unwrap(),
    };
    let different_right = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [97; 32],
        input_digest: digest_bytes(&different_right_result.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: different_right_result,
        published_at: "2000-01-01T00:00:00Z".parse().unwrap(),
    };
    let (different_left, different_right) = publish_concurrently_after_lock_barrier(
        &test_database.pool,
        scope,
        concurrency_repository.clone(),
        &access,
        different_left,
        different_right,
    )
    .await;
    assert_eq!(
        usize::from(different_left.is_ok()) + usize::from(different_right.is_ok()),
        1
    );
    assert!(matches!(
        (different_left, different_right),
        (Ok(_), Err(SchedulePublicationError::StaleComposition))
            | (Err(SchedulePublicationError::StaleComposition), Ok(_))
    ));

    // Caller clocks can be inverted relative to serialization order. The
    // transaction captures/clamps publication time after its locks, so even a
    // deterministic future-then-past caller sequence remains monotonic.
    let mut future_caller_request = compose_request();
    future_caller_request.fixed_blocks[0].title = "Future caller clock".to_owned();
    let future_caller_result =
        compose_canonical_schedule(&items, &schedules, future_caller_request)
            .await
            .unwrap();
    let future_caller = concurrency_repository
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [205; 32],
                input_digest: digest_bytes(&future_caller_result.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: future_caller_result,
                published_at: "2099-01-01T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    let mut past_caller_request = compose_request();
    past_caller_request.fixed_blocks[0].title = "Past caller clock".to_owned();
    let past_caller_result = compose_canonical_schedule(&items, &schedules, past_caller_request)
        .await
        .unwrap();
    let past_caller = concurrency_repository
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [206; 32],
                input_digest: digest_bytes(&past_caller_result.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: past_caller_result,
                published_at: "2000-01-01T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(past_caller.revision.published_at >= future_caller.revision.published_at);

    let latest_schedule = schedules
        .get_schedule(
            &access,
            ScheduleQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
                detail: ScheduleDetail::Full,
            },
        )
        .await
        .unwrap();
    let movable = latest_schedule
        .blocks
        .iter()
        .find(|block| !block.redacted && block.kind == "planned")
        .and_then(|block| block.id.clone())
        .unwrap();
    let immutable = latest_schedule
        .blocks
        .iter()
        .find(|block| !block.redacted && block.kind != "planned")
        .and_then(|block| block.id.clone())
        .unwrap();
    let operations = vec![
        operation(
            PlanOperationKind::MoveBlock,
            Some(&movable),
            json!({"start": "2026-09-01T13:00:00Z"}),
        ),
        operation(
            PlanOperationKind::MoveBlock,
            Some(&immutable),
            json!({"start": "2026-09-01T14:00:00Z"}),
        ),
        operation(
            PlanOperationKind::DeleteItem,
            Some(&public_task.to_string()),
            json!({}),
        ),
        operation(PlanOperationKind::CreateItem, None, json!({})),
        operation(
            PlanOperationKind::UpdateItem,
            Some(&public_task.to_string()),
            json!({}),
        ),
        operation(
            PlanOperationKind::CompleteItem,
            Some(&public_task.to_string()),
            json!({}),
        ),
        operation(
            PlanOperationKind::UpdateConstraint,
            Some(&public_task.to_string()),
            json!({}),
        ),
        operation(PlanOperationKind::CreateEvent, None, json!({})),
        operation(
            PlanOperationKind::GoalBreakdown,
            Some(&public_goal_a.to_string()),
            json!({}),
        ),
        operation(PlanOperationKind::ReplaceSchedule, None, json!({})),
    ];
    let simulation_request = SimulationRequest {
        base_revision: latest_schedule.revision.clone(),
        operations,
        assumptions: vec!["Synthetic test assumption".to_owned()],
    };
    let simulated = schedules
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    assert!(simulated.moved_blocks.is_empty());
    assert_eq!(
        simulated
            .warnings
            .iter()
            .filter(|warning| warning.code == "not_modeled")
            .count(),
        4
    );
    assert!(
        simulated
            .warnings
            .iter()
            .any(|warning| warning.code == "confirmation_required")
    );
    assert!(
        simulated
            .warnings
            .iter()
            .any(|warning| warning.code == "not_movable")
    );
    assert!(
        serde_json::to_value(&simulated)
            .unwrap()
            .get("privacy_evidence")
            .is_none(),
        "typed privacy evidence is server-internal and never appears in the MCP result"
    );
    assert!(!simulated.application_ready);
    assert!(simulated.change_set_schema.is_none());
    let public_simulation = serde_json::to_value(&simulated).unwrap();
    assert!(public_simulation.get("proposal_evidence").is_none());
    let (
        stored_privacy_evidence,
        stored_proposal_evidence,
        stored_request_hash,
        stored_request_digest,
        stored_evidence_hash,
        compilation_outcome,
        compiled_payload_hash,
    ): StoredSimulationEvidenceRow = sqlx::query_as(
        "SELECT result_snapshot ? 'privacy_evidence', result_snapshot ? 'proposal_evidence', \
           request_hash, request_digest, evidence_hash, compilation_outcome, compiled_payload_hash \
         FROM schedule_simulations \
         WHERE workspace_id = $1 AND user_id = $2 AND consumed_at IS NULL \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert!(stored_privacy_evidence);
    assert!(stored_proposal_evidence);
    let expected_request_hash = simulation_request_hash(&simulation_request).unwrap();
    assert_eq!(stored_request_hash, expected_request_hash);
    assert_eq!(stored_request_digest, expected_request_hash[..16]);
    assert_eq!(stored_evidence_hash.len(), 32);
    assert_ne!(stored_evidence_hash, stored_request_hash);
    assert_eq!(compilation_outcome, "manual_review");
    assert!(compiled_payload_hash.is_none());
    assert!(
        sqlx::query(
            "UPDATE schedule_simulations SET evidence_hash = $3 \
             WHERE workspace_id = $1 AND user_id = $2 AND consumed_at IS NULL",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(vec![0_u8; 32])
        .execute(&test_database.pool)
        .await
        .is_err(),
        "the hidden request and compilation commitment is immutable"
    );
    let stored_token_leaks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_simulations WHERE result_snapshot::text LIKE $1",
    )
    .bind(format!("%{}%", simulated.simulation_token))
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(stored_token_leaks, 0);
    let stored_token_hash: Vec<u8> = sqlx::query_scalar(
        "SELECT token_hash FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 \
         AND consumed_at IS NULL ORDER BY created_at DESC LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    let raw_hash: [u8; 32] = Sha256::digest(simulated.simulation_token.as_bytes()).into();
    let mut expected_token_hash = Sha256::new();
    expected_token_hash.update(b"dayweave.schedule-simulation-token.v1\0");
    expected_token_hash.update(simulated.simulation_token.as_bytes());
    assert_ne!(stored_token_hash.as_slice(), raw_hash.as_slice());
    assert_eq!(stored_token_hash, expected_token_hash.finalize().as_slice());

    let restarted = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let submission_key = "durable-proposal-001";
    let mut first_submission = proposal_submission(
        &access.subject,
        submission_key,
        [81; 32],
        &simulation_request,
        simulated.simulation_token.clone(),
    );
    first_submission.expires_at = "2099-01-02T00:00:00.987654321Z".parse().unwrap();
    let submitted = restarted
        .submit_proposal(&access, first_submission)
        .await
        .unwrap();
    assert!(!submitted.duplicate);
    assert!(
        submitted
            .proposal
            .created_at
            .timestamp_subsec_nanos()
            .is_multiple_of(1_000)
    );
    assert!(
        submitted
            .proposal
            .expires_at
            .timestamp_subsec_nanos()
            .is_multiple_of(1_000)
    );
    assert_eq!(submitted.proposal.kind, ProposalKind::SchedulePlan);
    assert_eq!(
        submitted.proposal.payload["safety"]["application_ready"],
        false
    );
    assert_eq!(
        submitted.proposal.payload["safety"]["manual_review_reasons"],
        json!(["mixed_operation_kinds"])
    );
    assert!(ProposalChangeSet::from_payload(&submitted.proposal.payload).is_err());
    assert_submission_proof(
        &test_database.pool,
        scope,
        submitted.proposal.id,
        &simulation_request,
        "manual_review",
    )
    .await;
    let after_restart = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let replayed = after_restart
        .submit_proposal(
            &access,
            proposal_submission(
                &access.subject,
                submission_key,
                [81; 32],
                &simulation_request,
                simulated.simulation_token.clone(),
            ),
        )
        .await
        .unwrap();
    assert!(replayed.duplicate);
    assert_eq!(replayed.proposal.id, submitted.proposal.id);
    assert_eq!(
        serde_json::to_value(&replayed.proposal).unwrap(),
        serde_json::to_value(&submitted.proposal).unwrap()
    );
    assert!(matches!(
        after_restart
            .submit_proposal(
                &access,
                proposal_submission(
                    &access.subject,
                    submission_key,
                    [82; 32],
                    &simulation_request,
                    simulated.simulation_token.clone(),
                ),
            )
            .await,
        Err(ProposalSubmissionError::IdempotencyConflict)
    ));
    assert_eq!(
        restarted
            .consume_simulation(
                &access,
                &simulated.simulation_token,
                &simulated.request_digest,
            )
            .await,
        Err(SchedulingPortError::NotFound)
    );
    let receipt_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mcp_proposal_submissions WHERE workspace_id = $1 AND proposal_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(submitted.proposal.id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(receipt_count, 1);
    let leaked_submission_capabilities: i64 = sqlx::query_scalar(
        "SELECT \
           (SELECT COUNT(*) FROM proposals WHERE id = $1 \
             AND (payload::text LIKE $2 OR payload::text LIKE $3)) + \
           (SELECT COUNT(*) FROM outbox_messages WHERE aggregate_id = $1 \
             AND (payload::text LIKE $2 OR payload::text LIKE $3)) + \
           (SELECT COUNT(*) FROM audit_operations WHERE entity_id = $1 \
             AND (COALESCE(metadata, '{}'::jsonb)::text LIKE $2 \
               OR COALESCE(metadata, '{}'::jsonb)::text LIKE $3)) + \
           (SELECT COUNT(*) FROM mcp_proposal_submissions WHERE proposal_id = $1 \
             AND (mcp_proposal_submissions::text LIKE $2 \
               OR mcp_proposal_submissions::text LIKE $3))",
    )
    .bind(submitted.proposal.id)
    .bind(format!("%{submission_key}%"))
    .bind(format!("%{}%", simulated.simulation_token))
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(leaked_submission_capabilities, 0);
    assert!(
        sqlx::query(
            "UPDATE mcp_proposal_submissions SET request_fingerprint = $2 WHERE proposal_id = $1",
        )
        .bind(submitted.proposal.id)
        .bind(vec![9_u8; 32])
        .execute(&test_database.pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM mcp_proposal_submissions WHERE proposal_id = $1")
            .bind(submitted.proposal.id)
            .execute(&test_database.pool)
            .await
            .is_err()
    );
    assert!(
        !submitted
            .proposal
            .payload
            .to_string()
            .contains(submission_key)
    );

    assert_actionable_proposal_bridge(
        &test_database.pool,
        scope,
        &items,
        &restarted,
        &access,
        &latest_schedule.revision,
        public_task,
    )
    .await;

    let concurrent_simulation = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let concurrent_spec = proposal_submission(
        &access.subject,
        "durable-proposal-race",
        [83; 32],
        &simulation_request,
        concurrent_simulation.simulation_token.clone(),
    );
    let left_repository = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let right_repository = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let mut proposal_blocker = test_database.pool.begin().await.unwrap();
    let proposal_blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *proposal_blocker)
        .await
        .unwrap();
    sqlx::query(
        "SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut *proposal_blocker)
    .await
    .unwrap();
    let left_access = access.clone();
    let right_access = access.clone();
    let left_spec = concurrent_spec.clone();
    let right_spec = concurrent_spec.clone();
    let left_submission = tokio::spawn(async move {
        left_repository
            .submit_proposal(&left_access, left_spec)
            .await
    });
    let right_submission = tokio::spawn(async move {
        right_repository
            .submit_proposal(&right_access, right_spec)
            .await
    });
    wait_for_blocked_queries(&test_database.pool, proposal_blocker_pid, 2).await;
    proposal_blocker.commit().await.unwrap();
    let left_submission = left_submission.await.unwrap();
    let right_submission = right_submission.await.unwrap();
    let left_submission = left_submission.unwrap();
    let right_submission = right_submission.unwrap();
    assert_eq!(
        usize::from(left_submission.duplicate) + usize::from(right_submission.duplicate),
        1
    );
    assert_eq!(left_submission.proposal.id, right_submission.proposal.id);
    assert!(matches!(
        restarted
            .submit_proposal(
                &access,
                proposal_submission(
                    &access.subject,
                    "durable-proposal-token-reuse",
                    [84; 32],
                    &simulation_request,
                    concurrent_simulation.simulation_token,
                ),
            )
            .await,
        Err(ProposalSubmissionError::Simulation(
            SchedulingPortError::NotFound
        ))
    ));

    // A failure after token locking but before proposal insertion rolls back
    // consumed_at; a valid retry with the same key can still commit.
    let before_insert_simulation = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let before_insert_spec = proposal_submission(
        &access.subject,
        "durable-proposal-before-insert",
        [85; 32],
        &simulation_request,
        before_insert_simulation.simulation_token.clone(),
    );
    sqlx::raw_sql(
        "CREATE FUNCTION fail_test_mcp_proposal_insert() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'synthetic proposal insert failure'; END $$; \
         CREATE TRIGGER fail_test_mcp_proposal_insert BEFORE INSERT ON proposals \
         FOR EACH ROW EXECUTE FUNCTION fail_test_mcp_proposal_insert();",
    )
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(matches!(
        restarted
            .submit_proposal(&access, before_insert_spec.clone())
            .await,
        Err(ProposalSubmissionError::Unavailable)
    ));
    sqlx::raw_sql(
        "DROP TRIGGER fail_test_mcp_proposal_insert ON proposals; \
         DROP FUNCTION fail_test_mcp_proposal_insert();",
    )
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_simulation_unconsumed(
        &test_database.pool,
        scope,
        &before_insert_simulation.simulation_token,
    )
    .await;
    assert!(
        restarted
            .submit_proposal(&access, before_insert_spec)
            .await
            .is_ok()
    );

    // A failure after proposal/outbox/audit insertion but before receipt
    // completion rolls the entire transaction back without an in-progress
    // tombstone. Removing the injected trigger makes the exact retry succeed.
    let after_insert_simulation = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let after_insert_spec = proposal_submission(
        &access.subject,
        "durable-proposal-after-insert",
        [86; 32],
        &simulation_request,
        after_insert_simulation.simulation_token.clone(),
    );
    let before_failed_submission = proposal_artifact_counts(&test_database.pool, scope).await;
    sqlx::raw_sql(
        "CREATE FUNCTION fail_test_mcp_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'synthetic receipt failure'; END $$; \
         CREATE TRIGGER fail_test_mcp_receipt BEFORE INSERT ON mcp_proposal_submissions \
         FOR EACH ROW EXECUTE FUNCTION fail_test_mcp_receipt();",
    )
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(matches!(
        restarted
            .submit_proposal(&access, after_insert_spec.clone())
            .await,
        Err(ProposalSubmissionError::Unavailable)
    ));
    sqlx::raw_sql(
        "DROP TRIGGER fail_test_mcp_receipt ON mcp_proposal_submissions; \
         DROP FUNCTION fail_test_mcp_receipt();",
    )
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_simulation_unconsumed(
        &test_database.pool,
        scope,
        &after_insert_simulation.simulation_token,
    )
    .await;
    assert_eq!(
        proposal_artifact_counts(&test_database.pool, scope).await,
        before_failed_submission,
        "receipt failure rolls back the generated proposal, outbox, audit, and receipt rows"
    );
    assert!(
        restarted
            .submit_proposal(&access, after_insert_spec)
            .await
            .is_ok()
    );

    let subject_simulation = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let wrong_subject_access = owner_access(scope, "auth0|different-owner-subject");
    assert!(matches!(
        restarted
            .submit_proposal(
                &wrong_subject_access,
                proposal_submission(
                    &wrong_subject_access.subject,
                    "durable-proposal-wrong-subject",
                    [87; 32],
                    &simulation_request,
                    subject_simulation.simulation_token.clone(),
                ),
            )
            .await,
        Err(ProposalSubmissionError::Simulation(
            SchedulingPortError::NotFound
        ))
    ));
    assert_simulation_unconsumed(
        &test_database.pool,
        scope,
        &subject_simulation.simulation_token,
    )
    .await;

    let expired_token = format!("sim_{}", "A".repeat(43));
    let mut expired_hash = Sha256::new();
    expired_hash.update(b"dayweave.schedule-simulation-token.v1\0");
    expired_hash.update(expired_token.as_bytes());
    let expired_token_hash: [u8; 32] = expired_hash.finalize().into();
    let mut expired_subject_hash = Sha256::new();
    expired_subject_hash.update(b"dayweave.schedule-simulation-subject.v1\0");
    expired_subject_hash.update(access.subject.as_bytes());
    let expired_subject_hash: [u8; 32] = expired_subject_hash.finalize().into();
    let expired_request_hash = simulation_request_hash(&simulation_request).unwrap();
    let expired_request_digest = simulation_request_digest(&simulation_request).unwrap();
    let expired_base_revision_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    let expired_simulation_id = Uuid::new_v4();
    let expired_created_at: chrono::DateTime<Utc> = "2000-01-01T00:00:00Z".parse().unwrap();
    let expired_expires_at: chrono::DateTime<Utc> = "2000-01-01T00:14:00Z".parse().unwrap();
    let expired_snapshot = json!({
        "request_digest": expired_request_digest,
        "base_revision": simulation_request.base_revision,
        "application_ready": false,
        "change_set_schema": null,
        "moved_blocks": [],
        "unscheduled_item_ids": [],
        "violations": [],
        "warnings": [],
        "privacy_evidence": {
            "schema_version": 1,
            "item_ids": [],
            "block_ids": [],
            "sensitive_at_simulation": false
        },
        "proposal_evidence": {
            "schema_version": 1,
            "proposal_kind": null,
            "change_set": null,
            "manual_review_reasons": ["expired_fixture"]
        }
    });
    let expired_commitment = json!({
        "workspace_id": scope.workspace_id,
        "user_id": scope.user_id,
        "simulation_id": expired_simulation_id,
        "subject_hash": URL_SAFE_NO_PAD.encode(expired_subject_hash),
        "request_hash": URL_SAFE_NO_PAD.encode(expired_request_hash),
        "base_revision_id": expired_base_revision_id,
        "base_revision_label": simulation_request.base_revision,
        "created_at": expired_created_at,
        "expires_at": expired_expires_at,
        "snapshot": expired_snapshot
    });
    let mut expired_evidence_hash = Sha256::new();
    expired_evidence_hash.update(b"dayweave.mcp-simulation-evidence.v1\0");
    expired_evidence_hash.update(serde_json::to_vec(&expired_commitment).unwrap());
    let expired_evidence_hash: [u8; 32] = expired_evidence_hash.finalize().into();
    sqlx::query(
        "INSERT INTO schedule_simulations (id, workspace_id, user_id, token_hash, subject_hash, \
           request_digest, base_revision_id, base_revision_label, result_snapshot, created_at, \
           expires_at, evidence_schema, request_hash, evidence_hash, compilation_outcome, \
           compiled_payload_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
           1, $12, $13, 'manual_review', NULL)",
    )
    .bind(expired_simulation_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(expired_token_hash.as_slice())
    .bind(expired_subject_hash.as_slice())
    .bind(&expired_request_hash[..16])
    .bind(expired_base_revision_id)
    .bind(&simulation_request.base_revision)
    .bind(expired_snapshot)
    .bind(expired_created_at)
    .bind(expired_expires_at)
    .bind(expired_request_hash.as_slice())
    .bind(expired_evidence_hash.as_slice())
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(matches!(
        restarted
            .submit_proposal(
                &access,
                proposal_submission(
                    &access.subject,
                    "durable-proposal-expired",
                    [88; 32],
                    &simulation_request,
                    expired_token,
                ),
            )
            .await,
        Err(ProposalSubmissionError::Simulation(
            SchedulingPortError::NotFound
        ))
    ));

    let raced = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let left = restarted.clone();
    let right = restarted.clone();
    let left_access = access.clone();
    let right_access = access.clone();
    let left_token = raced.simulation_token.clone();
    let right_token = raced.simulation_token.clone();
    let left_digest = raced.request_digest.clone();
    let right_digest = raced.request_digest.clone();
    let mut consume_blocker = test_database.pool.begin().await.unwrap();
    let consume_blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *consume_blocker)
        .await
        .unwrap();
    sqlx::query(
        "SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&mut *consume_blocker)
    .await
    .unwrap();
    let left = tokio::spawn(async move {
        left.consume_simulation(&left_access, &left_token, &left_digest)
            .await
    });
    let right = tokio::spawn(async move {
        right
            .consume_simulation(&right_access, &right_token, &right_digest)
            .await
    });
    wait_for_blocked_queries(&test_database.pool, consume_blocker_pid, 2).await;
    consume_blocker.commit().await.unwrap();
    let left = left.await.unwrap();
    let right = right.await.unwrap();
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert!(matches!(
        (left, right),
        (Ok(_), Err(SchedulingPortError::NotFound)) | (Err(SchedulingPortError::NotFound), Ok(_))
    ));

    // Item mutation holding the shared advisory lock wins; publication wakes,
    // rechecks the exact active revision map, and refuses stale composition.
    let race_preview = compose_canonical_schedule(&items, &schedules, compose_request())
        .await
        .unwrap();
    let mut mutation = test_database.pool.begin().await.unwrap();
    let mutation_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *mutation)
        .await
        .unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .execute(&mut *mutation)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE items SET revision = revision + 1, updated_at = clock_timestamp() \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(public_task)
    .execute(&mut *mutation)
    .await
    .unwrap();
    let publisher = restarted.clone();
    let race_access = access.clone();
    let race_digest = digest_bytes(&race_preview.input_digest);
    let publish = tokio::spawn(async move {
        publisher
            .publish(
                &race_access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [51; 32],
                    input_digest: race_digest,
                    timezone_name: "Europe/Madrid".to_owned(),
                    manual_placement_approvals: Vec::new(),
                    result: race_preview,
                    published_at: Utc::now(),
                },
            )
            .await
    });
    wait_for_blocked_queries(&test_database.pool, mutation_pid, 1).await;
    mutation.commit().await.unwrap();
    assert!(matches!(
        publish.await.unwrap(),
        Err(SchedulePublicationError::StaleComposition)
    ));

    assert_publication_failure_rollbacks(&test_database.pool, scope, &items, &restarted, &access)
        .await;
    assert_content_insert_seal_race(&test_database.pool, scope).await;

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn manual_placement_approval_and_carry_forward_are_durable() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; manual placement test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        Arc::new(SystemClock),
    ));
    let schedules = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let access = owner_access(scope, "auth0|manual-placement-owner");
    let item_id = Uuid::new_v4();
    let created = items
        .create(
            task(
                item_id,
                "SYNTHETIC-SENSITIVE-MANUAL-PG-TARGET",
                true,
                None,
                json!({}),
            ),
            idempotency(221),
        )
        .await
        .expect("create manual placement target");

    let mut baseline_request = compose_request();
    baseline_request.fixed_blocks.clear();
    let baseline_preview = compose_canonical_schedule(&items, &schedules, baseline_request.clone())
        .await
        .expect("baseline preview");
    let baseline_block = baseline_preview
        .plan
        .blocks
        .iter()
        .find(|block| block.item_id.is_some_and(|id| id.0 == item_id))
        .expect("baseline source block")
        .clone();
    let baseline_publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [221; 32],
                input_digest: digest_bytes(&baseline_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: baseline_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("publish baseline");

    let placement_id = Uuid::new_v4();
    let fixed_id = Uuid::new_v4();
    let mut manual_request = baseline_request.clone();
    let mut conflict = compose_request().fixed_blocks.remove(0);
    conflict.id = fixed_id;
    conflict.is_sensitive = true;
    conflict.title = "SYNTHETIC-SENSITIVE-MANUAL-PG-CONFLICT".to_owned();
    conflict.start = "2026-09-01T09:30:00Z".parse().unwrap();
    conflict.end = "2026-09-01T10:30:00Z".parse().unwrap();
    manual_request.fixed_blocks = vec![conflict];
    manual_request.manual_placements = vec![ManualPlacementInput {
        id: placement_id,
        source_schedule_revision_id: Some(baseline_publication.revision.id),
        assignments: vec![ManualPlacementAssignmentInput {
            item_id,
            item_revision: created.item.revision,
            occurrence_id: None,
            blocks: vec![PreviousBlockInput {
                start: "2026-09-01T09:00:00Z".parse().unwrap(),
                end: "2026-09-01T10:00:00Z".parse().unwrap(),
                session_index: baseline_block.session_index,
            }],
        }],
    }];
    let preview = compose_canonical_schedule(&items, &schedules, manual_request.clone())
        .await
        .expect("manual placement preview");
    let [assessment] = preview.manual_placement_assessments.as_slice() else {
        panic!("one manual placement assessment");
    };
    assert!(assessment.approval_required);
    assert!(
        assessment
            .violations
            .iter()
            .any(|violation| violation.conflicting_block_ids == vec![fixed_id])
    );
    assert!(
        !serde_json::to_string(assessment)
            .unwrap()
            .contains("SYNTHETIC-SENSITIVE")
    );

    let (app, device_access_token) =
        credential_publish_app(&test_database.pool, scope, items.clone(), schedules.clone()).await;
    let missing = schedule_publish_body(Uuid::new_v4(), &preview.input_digest, &manual_request);
    assert_stale_schedule_publication(&app, &device_access_token, &missing).await;
    let mut wrong = schedule_publish_body(Uuid::new_v4(), &preview.input_digest, &manual_request);
    wrong["manual_placement_approvals"] = json!([{
        "placement_id": placement_id,
        "approval_digest": format!("sha256:{}", "0".repeat(64)),
    }]);
    assert_stale_schedule_publication(&app, &device_access_token, &wrong).await;

    let mut exact = schedule_publish_body(Uuid::new_v4(), &preview.input_digest, &manual_request);
    exact["manual_placement_approvals"] = json!([{
        "placement_id": placement_id,
        "approval_digest": assessment.approval_digest,
    }]);
    let published = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &exact,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::OK);
    let published = body_json(published).await;
    let revision_id = Uuid::parse_str(published["revision"]["id"].as_str().unwrap()).unwrap();
    let state: Value = sqlx::query_scalar(
        "SELECT result_snapshot -> 'manual_placement_state' \
         FROM schedule_revision_details WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(state[0]["placement"]["id"], placement_id.to_string());
    assert_eq!(state[0]["authorization"], "explicit_approval");
    assert!(!state.to_string().contains("SYNTHETIC-SENSITIVE"));
    let block_evidence: Value = sqlx::query_scalar(
        "SELECT constraint_snapshot -> 'manual_placement' FROM schedule_blocks \
         WHERE workspace_id = $1 AND schedule_revision_id = $2 AND item_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(item_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(block_evidence["placement_id"], placement_id.to_string());
    assert_eq!(block_evidence["authorization"], "explicit_approval");

    let mut carried_request = manual_request.clone();
    carried_request.manual_placements.clear();
    let carried_preview = compose_canonical_schedule(&items, &schedules, carried_request.clone())
        .await
        .expect("retained manual placement preview");
    assert!(!carried_preview.manual_placement_assessments[0].approval_required);
    let carried_body = schedule_publish_body(
        Uuid::new_v4(),
        &carried_preview.input_digest,
        &carried_request,
    );
    let carried_publish = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &carried_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(carried_publish.status(), StatusCode::OK);
    let carried_publish = body_json(carried_publish).await;
    let carried_revision =
        Uuid::parse_str(carried_publish["revision"]["id"].as_str().unwrap()).unwrap();
    let carried_authorization: String = sqlx::query_scalar(
        "SELECT result_snapshot -> 'manual_placement_state' -> 0 ->> 'authorization' \
         FROM schedule_revision_details WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(carried_revision)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(carried_authorization, "carried_forward");

    let mut changed_obstacle_request = carried_request.clone();
    changed_obstacle_request.fixed_blocks[0].start = "2026-09-01T09:15:00Z".parse().unwrap();
    changed_obstacle_request.fixed_blocks[0].end = "2026-09-01T10:15:00Z".parse().unwrap();
    let changed = compose_canonical_schedule(&items, &schedules, changed_obstacle_request)
        .await
        .expect("changed obstacle re-assessment");
    assert!(changed.manual_placement_assessments[0].approval_required);

    // A fresh client can recover the exact complete group without possessing
    // the private publication snapshot or the original local placement journal.
    let catalog_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/schedule/manual-placements")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {device_access_token}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(catalog_response.status(), StatusCode::OK);
    let catalog = body_json(catalog_response).await;
    assert_eq!(
        catalog["current_schedule_revision_id"],
        carried_revision.to_string()
    );
    assert_eq!(catalog["placements"].as_array().unwrap().len(), 1);
    assert_eq!(
        catalog["placements"][0]["placement_id"],
        placement_id.to_string()
    );
    assert_eq!(
        catalog["placements"][0]["assignments"][0]["item_id"],
        item_id.to_string()
    );
    assert_eq!(
        catalog["placements"][0]["assignments"][0]["published_item_revision"],
        created.item.revision
    );
    assert_eq!(
        catalog["placements"][0]["assignments"][0]["blocks"][0]["session_index"],
        baseline_block.session_index
    );
    assert!(!catalog.to_string().contains("SYNTHETIC-SENSITIVE"));

    let discovered_placement_id =
        Uuid::parse_str(catalog["placements"][0]["placement_id"].as_str().unwrap()).unwrap();
    let discovered_revision =
        Uuid::parse_str(catalog["current_schedule_revision_id"].as_str().unwrap()).unwrap();
    let mut stale_release_request = carried_request.clone();
    stale_release_request.manual_placement_releases = vec![ManualPlacementReleaseInput {
        id: Uuid::new_v4(),
        placement_id: discovered_placement_id,
        source_schedule_revision_id: discovered_revision,
    }];
    let stale_release_preview =
        compose_canonical_schedule(&items, &schedules, stale_release_request.clone())
            .await
            .expect("release preview before intervening publication");

    let mut intervening_request = carried_request.clone();
    intervening_request.config.stability_weight = intervening_request
        .config
        .stability_weight
        .saturating_add(1);
    let intervening_preview =
        compose_canonical_schedule(&items, &schedules, intervening_request.clone())
            .await
            .expect("intervening retained-placement preview");
    let intervening = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [223; 32],
                input_digest: digest_bytes(&intervening_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: intervening_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("intervening publication");
    assert_ne!(intervening.revision.id, discovered_revision);
    let revisions_before_stale: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    let stale_release_body = schedule_publish_body(
        Uuid::new_v4(),
        &stale_release_preview.input_digest,
        &stale_release_request,
    );
    let stale_release = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &stale_release_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(stale_release.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(stale_release).await["error"]["code"],
        "schedule_publication_stale"
    );
    let revisions_after_stale: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert_eq!(revisions_after_stale, revisions_before_stale);

    let refreshed_catalog = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/schedule/manual-placements")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {device_access_token}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refreshed_catalog.status(), StatusCode::OK);
    let refreshed_catalog = body_json(refreshed_catalog).await;
    let refreshed_revision = Uuid::parse_str(
        refreshed_catalog["current_schedule_revision_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(refreshed_revision, intervening.revision.id);

    let pre_edit_carried_request = intervening_request.clone();
    let pre_edit_carried_preview =
        compose_canonical_schedule(&items, &schedules, pre_edit_carried_request.clone())
            .await
            .expect("fresh retained-placement preview before item edit");
    assert!(
        pre_edit_carried_preview
            .manual_placement_assessments
            .iter()
            .all(|assessment| !assessment.approval_required)
    );
    let pre_edit_carried_body = schedule_publish_body(
        Uuid::new_v4(),
        &pre_edit_carried_preview.input_digest,
        &pre_edit_carried_request,
    );

    let current = items.get(item_id).await.expect("current placement item");
    let changed_item = items
        .replace(
            item_id,
            current.revision,
            ReplaceItem {
                is_sensitive: current.is_sensitive,
                kind: current.kind,
                status: current.status,
                title: current.title,
                notes: current.notes,
                timezone_name: current.timezone_name,
                duration_seconds: Some(5_400),
                deadline_at: current.deadline_at,
                earliest_start_at: current.earliest_start_at,
                recurrence: current.recurrence,
                flexible_constraints: current.flexible_constraints,
                split_policy: current.split_policy,
                importance: current.importance,
                urgency: current.urgency,
                parent_id: current.parent_id,
                sibling_order: current.sibling_order,
            },
            idempotency(222),
        )
        .await
        .expect("change retained item duration");
    assert_eq!(changed_item.item.duration_seconds, Some(5_400));

    let revisions_before_changed_item_publish: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert_stale_schedule_publication(&app, &device_access_token, &pre_edit_carried_body).await;
    let revisions_after_changed_item_publish: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert_eq!(
        revisions_after_changed_item_publish, revisions_before_changed_item_publish,
        "stale carried placement must not write a schedule revision"
    );
    assert!(
        compose_canonical_schedule(&items, &schedules, carried_request.clone())
            .await
            .is_err(),
        "an incompatible retained shape must not be silently normalized"
    );

    let release_id = Uuid::new_v4();
    let mut release_request = carried_request;
    release_request.manual_placement_releases = vec![ManualPlacementReleaseInput {
        id: release_id,
        placement_id: discovered_placement_id,
        source_schedule_revision_id: refreshed_revision,
    }];
    let release_preview = compose_canonical_schedule(&items, &schedules, release_request.clone())
        .await
        .expect("discovered pure release preview");
    assert!(release_preview.manual_placement_assessments.is_empty());
    assert!(
        release_preview
            .plan
            .blocks
            .iter()
            .filter(|block| block.item_id.is_some_and(|id| id.0 == item_id))
            .all(|block| block.kind != dayweave_core::ScheduleBlockKind::Pinned)
    );
    let release_body = schedule_publish_body(
        Uuid::new_v4(),
        &release_preview.input_digest,
        &release_request,
    );
    let release_publish = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &release_body,
            &device_access_token,
        ))
        .await
        .unwrap();
    assert_eq!(release_publish.status(), StatusCode::OK);
    let release_publish = body_json(release_publish).await;
    let release_revision =
        Uuid::parse_str(release_publish["revision"]["id"].as_str().unwrap()).unwrap();
    let release_snapshot: Value = sqlx::query_scalar(
        "SELECT result_snapshot FROM schedule_revision_details \
         WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(release_revision)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(
        release_snapshot["manual_placement_releases"][0]["id"],
        release_id.to_string()
    );
    assert_eq!(
        release_snapshot["manual_placement_releases"][0]["placement_id"],
        placement_id.to_string()
    );
    assert_eq!(release_snapshot["manual_placement_state"], json!([]));
    let audit_release_id: String = sqlx::query_scalar(
        "SELECT metadata -> 'manual_placement_releases' -> 0 ->> 'id' \
         FROM audit_operations WHERE workspace_id = $1 AND entity_id = $2 \
           AND operation_type = 'schedule.published'",
    )
    .bind(scope.workspace_id)
    .bind(release_revision)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(audit_release_id, release_id.to_string());

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle proves pre-defer reuse, binding, replay, and supersession together.
async fn deferred_publication_requires_an_exact_pinned_binding_and_preserves_receipts() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; schedule PostgreSQL test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let item_repository = Arc::new(PostgresItemRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let items = Arc::new(ItemService::new(item_repository, Arc::new(SystemClock)));
    let schedules = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        items.clone(),
        Arc::new(SystemClock),
    );
    let access = owner_access(scope, "auth0|deferred-publication-owner");
    let item_id = Uuid::new_v4();
    let fixture_day = future_fixture_day_start();
    let move_start = fixture_day + chrono::Duration::hours(11);
    let move_end = move_start + chrono::Duration::minutes(40);
    let mut source_item = task(
        item_id,
        "Keep the exact deferred promise",
        false,
        None,
        json!({}),
    );
    source_item.deadline_at = Some(fixture_day + chrono::Duration::hours(17));
    items.create(source_item, idempotency(141)).await.unwrap();
    let item = items.get(item_id).await.unwrap();

    let exact_request = deferred_compose_request(item_id, item.revision, move_start, move_end);
    let pre_defer_preview = compose_canonical_schedule(&items, &schedules, exact_request.clone())
        .await
        .unwrap();
    let pre_defer_blocks = pre_defer_preview
        .plan
        .blocks
        .iter()
        .filter(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .collect::<Vec<_>>();
    let [pre_defer_block] = pre_defer_blocks.as_slice() else {
        panic!("the pre-defer fixture must emit exactly one semantic block");
    };
    assert_eq!(
        serde_json::to_value(pre_defer_block.kind).unwrap(),
        json!("planned")
    );
    assert_eq!(pre_defer_block.session_index, 0);
    let original_block_id = pre_defer_block.id;

    // The same content is a normal revision before any defer exists.
    let pre_defer_spec = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [141; 32],
        input_digest: digest_bytes(&pre_defer_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: pre_defer_preview.clone(),
        published_at: Utc::now(),
    };
    let pre_defer = schedules
        .publish(&access, pre_defer_spec.clone())
        .await
        .unwrap();
    assert_eq!(pre_defer.revision.revision_number, 1);
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, pre_defer.revision.id).await,
        0
    );

    let deferred_session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id: deferred_session_id,
                item_id,
                item_revision: item.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(original_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("schedule-v20-source-start", 141),
        )
        .await
        .expect("start the exact published source block");
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id: deferred_session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess the exact replacement".to_owned()),
            }),
            execution_idempotency("schedule-v21-source-pause", 142),
        )
        .await
        .expect("pause before assessing the replacement");
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id: deferred_session_id,
            move_start,
            actual_seconds: Some(20 * 60),
        })
        .await
        .expect("assess the exact forty-minute remainder");
    assert_eq!(assessment.move_end, move_end);
    let approval = assessment
        .approval_required
        .then(|| assessment.assessment_digest.clone());
    execution
        .command(
            2,
            ExecutionCommand::Defer(DeferExecution {
                session_id: deferred_session_id,
                move_start: assessment.move_start,
                move_end: assessment.move_end,
                actual_seconds: Some(assessment.actual_seconds),
                assessment_digest: Some(assessment.assessment_digest),
                approved_assessment_digest: approval,
            }),
            execution_idempotency("schedule-v21-source-defer", 143),
        )
        .await
        .expect("defer with an exact forty-minute remainder");

    let exact_preview = compose_canonical_schedule(&items, &schedules, exact_request.clone())
        .await
        .expect("compose the authoritative fresh-index replacement");
    let exact_blocks = exact_preview
        .plan
        .blocks
        .iter()
        .filter(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .collect::<Vec<_>>();
    let [exact_block] = exact_blocks.as_slice() else {
        panic!("the replacement fixture must emit exactly one semantic block");
    };
    assert_eq!(
        serde_json::to_value(exact_block.kind).unwrap(),
        json!("pinned")
    );
    assert_eq!(exact_block.session_index, 1);
    assert_eq!(
        exact_block.start.unix_timestamp_nanos(),
        i128::from(move_start.timestamp_micros()) * 1_000
    );
    assert_eq!(
        exact_block.end.unix_timestamp_nanos(),
        i128::from(move_end.timestamp_micros()) * 1_000
    );
    let source_block_id = exact_block.id;

    // An old exact receipt wins before fresh defer guards, even though that
    // historical revision predates the binding protocol.
    let replay = schedules
        .publish(&access, pre_defer_spec.clone())
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.revision.id, pre_defer.revision.id);

    let omission = pre_defer_preview.clone();
    let before_stale = publication_attestation_counts(&test_database.pool, scope).await;
    assert!(matches!(
        schedules
            .publish(
                &access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [142; 32],
                    input_digest: digest_bytes(&omission.input_digest),
                    timezone_name: "Europe/Madrid".to_owned(),
                    manual_placement_approvals: Vec::new(),
                    result: omission.clone(),
                    published_at: Utc::now(),
                },
            )
            .await,
        Err(SchedulePublicationError::StaleComposition)
    ));
    assert_eq!(
        publication_attestation_counts(&test_database.pool, scope).await,
        before_stale
    );

    // A binding write failure must roll back the draft, its blocks/details,
    // request receipt, audit row, and attempted current-head transition. The
    // same key then remains usable once storage recovers.
    let post_defer_spec = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [143; 32],
        input_digest: digest_bytes(&exact_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        manual_placement_approvals: Vec::new(),
        result: exact_preview.clone(),
        published_at: Utc::now(),
    };
    test_database
        .pool
        .execute(
            "CREATE FUNCTION fail_test_deferred_binding() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'synthetic deferred binding failure'; END $$; \
             CREATE TRIGGER fail_test_deferred_binding BEFORE INSERT \
             ON schedule_defer_replacement_placements FOR EACH ROW \
             EXECUTE FUNCTION fail_test_deferred_binding();",
        )
        .await
        .unwrap();
    let before_binding_failure = publication_attestation_counts(&test_database.pool, scope).await;
    assert!(matches!(
        schedules.publish(&access, post_defer_spec.clone()).await,
        Err(SchedulePublicationError::Unavailable)
    ));
    assert_eq!(
        publication_attestation_counts(&test_database.pool, scope).await,
        before_binding_failure
    );
    let current_after_failure: Uuid = sqlx::query_scalar(
        "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(current_after_failure, pre_defer.revision.id);
    test_database
        .pool
        .execute(
            "DROP TRIGGER fail_test_deferred_binding ON schedule_defer_replacement_placements; \
             DROP FUNCTION fail_test_deferred_binding();",
        )
        .await
        .unwrap();

    // Although its content hash equals revision 1, the first successful
    // post-defer seal must create revision 2 to carry immutable evidence.
    let post_defer = schedules
        .publish(&access, post_defer_spec.clone())
        .await
        .unwrap();
    assert_eq!(post_defer.revision.revision_number, 2);
    assert_ne!(post_defer.revision.id, pre_defer.revision.id);
    let binding: DeferredBindingRow = sqlx::query_as(
        "SELECT source_deferred_session_id, source_block_id, item_id, item_revision, \
             execution_epoch, occurrence_id, replacement_session_index, \
             remaining_duration_seconds, move_start, move_end \
             FROM schedule_defer_replacement_placements \
             WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(post_defer.revision.id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(
        binding,
        (
            deferred_session_id,
            source_block_id,
            item_id,
            i64::try_from(item.revision).unwrap(),
            1,
            None,
            1,
            40 * 60,
            move_start,
            move_end,
        )
    );

    // Exact retries remain durable even after the claim-aware revision seals.
    let same_content = schedules
        .publish(&access, post_defer_spec.clone())
        .await
        .unwrap();
    assert!(same_content.replayed);
    assert_eq!(same_content.revision.id, post_defer.revision.id);
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, post_defer.revision.id).await,
        1
    );

    // A disjoint horizon has no obligation and may supersede the bound
    // revision without deleting its historical attestation.
    let mut disjoint_request = compose_request();
    disjoint_request.fixed_blocks.clear();
    disjoint_request.horizon_end = "2026-09-01T10:00:00Z".parse().unwrap();
    disjoint_request.availability[0].end = "2026-09-01T09:00:00Z".parse().unwrap();
    let disjoint = compose_canonical_schedule(&items, &schedules, disjoint_request)
        .await
        .unwrap();
    let disjoint_publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [145; 32],
                input_digest: digest_bytes(&disjoint.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: disjoint,
                published_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert_eq!(disjoint_publication.revision.revision_number, 3);
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, post_defer.revision.id).await,
        1
    );

    // A later overlapping legacy payload still cannot replace the current
    // disjoint head while silently erasing the deferred promise.
    let before_legacy_omission = publication_attestation_counts(&test_database.pool, scope).await;
    assert!(matches!(
        schedules
            .publish(
                &access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [148; 32],
                    input_digest: digest_bytes(&omission.input_digest),
                    timezone_name: "Europe/Madrid".to_owned(),
                    manual_placement_approvals: Vec::new(),
                    result: omission,
                    published_at: Utc::now(),
                },
            )
            .await,
        Err(SchedulePublicationError::StaleComposition)
    ));
    assert_eq!(
        publication_attestation_counts(&test_database.pool, scope).await,
        before_legacy_omission
    );
    let current_after_legacy_omission: Uuid = sqlx::query_scalar(
        "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(
        current_after_legacy_omission,
        disjoint_publication.revision.id
    );

    let historical_replay = schedules
        .publish(&access, pre_defer_spec.clone())
        .await
        .unwrap();
    assert!(historical_replay.replayed);
    assert_eq!(historical_replay.revision.id, pre_defer.revision.id);

    // Recompose from the disjoint head, seal the claim again, and consume it
    // through the normal execution repository. This joins Defer, scheduler
    // publication, exact Start origin, and one-shot consumption end to end.
    let restart_preview = compose_canonical_schedule(&items, &schedules, exact_request)
        .await
        .expect("recompose the still-live replacement after a disjoint head");
    let restart_block = restart_preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .expect("recomposed replacement block");
    assert_eq!(restart_block.session_index, 1);
    let restart_block_id = restart_block.id;
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [149; 32],
                input_digest: digest_bytes(&restart_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: restart_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("publish the recomposed replacement");
    let replacement_session_id = Uuid::new_v4();
    execution
        .command(
            3,
            ExecutionCommand::Start(StartExecution {
                session_id: replacement_session_id,
                item_id,
                item_revision: item.revision,
                occurrence_id: None,
                session_index: 1,
                planned_block_id: Some(restart_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("schedule-v21-replacement-start", 149),
        )
        .await
        .expect("consume the exact current replacement placement");
    let consumed_source: Uuid = sqlx::query_scalar(
        "SELECT source_deferred_session_id FROM execution_defer_replacement_consumptions \
         WHERE workspace_id = $1 AND replacement_execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(replacement_session_id)
    .fetch_one(&test_database.pool)
    .await
    .expect("load one-shot replacement consumption");
    assert_eq!(consumed_source, deferred_session_id);
    let active_replacement_preview = compose_canonical_schedule(
        &items,
        &schedules,
        deferred_compose_request(item_id, item.revision, move_start, move_end),
    )
    .await
    .expect("compose the consumed replacement as an in-flight reservation");
    let active_replacement = active_replacement_preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .expect("active replacement block");
    assert_eq!(active_replacement.session_index, 1);
    assert_eq!(
        serde_json::to_value(active_replacement.kind).unwrap(),
        json!("pinned")
    );
    let active_replacement_publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [150; 32],
                input_digest: digest_bytes(&active_replacement_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: active_replacement_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("republish the exact active replacement reservation");
    assert_eq!(
        deferred_binding_count(
            &test_database.pool,
            scope,
            active_replacement_publication.revision.id
        )
        .await,
        0
    );

    // Scope claims alone cannot replay an owner's durable receipt after a role
    // downgrade or removal. Both the HTTP preflight adapter and publication
    // transaction retain the database ownership fence.
    sqlx::query(
        "UPDATE workspace_members SET role = 'viewer' \
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(matches!(
        schedules
            .publication_receipt(
                &access,
                pre_defer_spec.idempotency_key,
                &pre_defer_spec.request_hash,
            )
            .await,
        Err(SchedulePublicationError::Unavailable)
    ));
    assert!(matches!(
        schedules.publish(&access, pre_defer_spec.clone()).await,
        Err(SchedulePublicationError::Unavailable)
    ));
    sqlx::query(
        "UPDATE workspace_members SET role = 'owner' \
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&test_database.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE workspace_members SET removed_at = clock_timestamp() \
         WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert!(matches!(
        schedules
            .publication_receipt(
                &access,
                pre_defer_spec.idempotency_key,
                &pre_defer_spec.request_hash,
            )
            .await,
        Err(SchedulePublicationError::Unavailable)
    ));
    assert!(matches!(
        schedules.publish(&access, pre_defer_spec).await,
        Err(SchedulePublicationError::Unavailable)
    ));

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The two orderings establish precedence and the execution-lock race.
async fn active_execution_precedes_a_newer_defer_and_publication_waits_for_execution_state() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; schedule PostgreSQL test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let item_repository = Arc::new(PostgresItemRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let items = Arc::new(ItemService::new(item_repository, Arc::new(SystemClock)));
    let schedules = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let access = owner_access(scope, "auth0|execution-precedence-owner");
    let item_id = Uuid::new_v4();
    let fixture_day = future_fixture_day_start();
    let move_start = fixture_day + chrono::Duration::hours(11);
    let move_end = move_start + chrono::Duration::minutes(40);
    let mut source_item = task(
        item_id,
        "Execution precedence fixture",
        false,
        None,
        json!({}),
    );
    source_item.deadline_at = Some(fixture_day + chrono::Duration::hours(17));
    items.create(source_item, idempotency(146)).await.unwrap();
    let item = items.get(item_id).await.unwrap();
    let execution = Arc::new(ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        items.clone(),
        Arc::new(SystemClock),
    ));
    let mut request = compose_request_for_day(fixture_day);
    request.fixed_blocks.clear();
    let initial_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose the initial executable schedule");
    let initial_block = initial_preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .expect("initial item block");
    assert_eq!(initial_block.session_index, 0);
    let initial_block_id = initial_block.id;
    schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [146; 32],
                input_digest: digest_bytes(&initial_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: initial_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("publish the source schedule");
    let active_session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id: active_session_id,
                item_id,
                item_revision: item.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: Some(initial_block_id),
                device_id: Uuid::new_v4(),
            }),
            execution_idempotency("schedule-v20-race-start", 146),
        )
        .await
        .expect("start the exact current block");
    let active_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose with the exact in-flight reservation");
    let active_publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [147; 32],
                input_digest: digest_bytes(&active_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: active_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("republish the exact active reservation");
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, active_publication.revision.id).await,
        0
    );
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id: active_session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Assess the exact replacement before the lock race".to_owned()),
            }),
            execution_idempotency("schedule-v21-race-pause", 147),
        )
        .await
        .expect("pause the source before assessing its replacement");
    let assessment = execution
        .assess_defer(DeferAssessmentRequest {
            expected_revision: 2,
            session_id: active_session_id,
            move_start,
            actual_seconds: Some(20 * 60),
        })
        .await
        .expect("assess the exact replacement before the lock race");
    assert_eq!(assessment.move_end, move_end);
    let race_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("refresh the paused evidence before the defer race");

    let before_race = publication_attestation_counts(&test_database.pool, scope).await;
    let mut blocker = test_database.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT workspace_id FROM execution_state WHERE workspace_id = $1 FOR UPDATE")
        .bind(scope.workspace_id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let deferred_execution = execution.clone();
    let approval = assessment
        .approval_required
        .then(|| assessment.assessment_digest.clone());
    let defer = tokio::spawn(async move {
        deferred_execution
            .command(
                2,
                ExecutionCommand::Defer(DeferExecution {
                    session_id: active_session_id,
                    move_start: assessment.move_start,
                    move_end: assessment.move_end,
                    actual_seconds: Some(assessment.actual_seconds),
                    assessment_digest: Some(assessment.assessment_digest),
                    approved_assessment_digest: approval,
                }),
                execution_idempotency("schedule-v21-race-defer", 148),
            )
            .await
    });
    wait_for_blocked_queries(&test_database.pool, blocker_pid, 1).await;
    let canonical_lock_available: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock( \
         hashtextextended('dayweave.items.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    assert!(
        canonical_lock_available,
        "defer must wait before canonical item space"
    );
    let publisher = schedules.clone();
    let race_access = access.clone();
    let publish = tokio::spawn(async move {
        publisher
            .publish(
                &race_access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [149; 32],
                    input_digest: digest_bytes(&race_preview.input_digest),
                    timezone_name: "Europe/Madrid".to_owned(),
                    manual_placement_approvals: Vec::new(),
                    result: race_preview,
                    published_at: Utc::now(),
                },
            )
            .await
    });
    wait_for_blocked_queries(&test_database.pool, blocker_pid, 2).await;
    blocker.commit().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), defer)
        .await
        .expect("assessed defer must finish without deadlocking publication")
        .unwrap()
        .expect("the earlier waiter applies its exact assessed defer");
    let result = tokio::time::timeout(StdDuration::from_secs(10), publish)
        .await
        .expect("publication/defer ordering must not deadlock")
        .unwrap();
    assert!(matches!(
        result,
        Err(SchedulePublicationError::StaleComposition)
    ));
    assert_eq!(
        publication_attestation_counts(&test_database.pool, scope).await,
        before_race
    );

    let claim_preview = compose_canonical_schedule(&items, &schedules, request)
        .await
        .expect("recompose the fresh replacement claim");
    let replacement = claim_preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .expect("replacement block");
    assert_eq!(replacement.session_index, 1);
    assert_eq!(
        serde_json::to_value(replacement.kind).unwrap(),
        json!("pinned")
    );
    let publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [150; 32],
                input_digest: digest_bytes(&claim_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: claim_preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("publish the exact replacement claim");
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, publication.revision.id).await,
        1
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercises all three dynamic claim-retirement paths in one isolated workspace.
async fn non_executable_claims_retire_without_blocking_publication() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; claim liveness test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 21) {
        test_database
            .pool
            .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-authorization migration applies");
    }
    let scope = seed_scope(&test_database.pool).await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        Arc::new(SystemClock),
    ));
    let nonleaf_item = Uuid::new_v4();
    let terminal_item = Uuid::new_v4();
    let trashed_item = Uuid::new_v4();
    for (item_id, title, marker) in [
        (nonleaf_item, "Claim becomes non-leaf", 201),
        (terminal_item, "Claim becomes completed", 202),
        (trashed_item, "Claim becomes trashed", 203),
    ] {
        items
            .create(
                task(item_id, title, false, None, json!({})),
                idempotency(marker),
            )
            .await
            .expect("create claim liveness item");
    }
    let terminal_at: chrono::DateTime<Utc> = "2026-08-31T06:00:00Z".parse().unwrap();
    insert_live_legacy_claim(
        &test_database.pool,
        scope,
        nonleaf_item,
        terminal_at,
        "2026-09-01T11:00:00Z".parse().unwrap(),
    )
    .await;
    insert_live_legacy_claim(
        &test_database.pool,
        scope,
        terminal_item,
        terminal_at + chrono::Duration::seconds(1),
        "2026-09-01T12:00:00Z".parse().unwrap(),
    )
    .await;
    insert_live_legacy_claim(
        &test_database.pool,
        scope,
        trashed_item,
        terminal_at + chrono::Duration::seconds(2),
        "2026-09-01T13:00:00Z".parse().unwrap(),
    )
    .await;
    let authorization_migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 21)
        .expect("execution defer authorization migration is embedded");
    test_database
        .pool
        .execute(AssertSqlSafe(
            authorization_migration.sql.as_str().to_owned(),
        ))
        .await
        .expect("execution defer authorization migration applies");
    sqlx::query(
        "UPDATE execution_state SET revision = 1, active_session_id = NULL, updated_at = $2 \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(terminal_at + chrono::Duration::seconds(3))
    .execute(&test_database.pool)
    .await
    .expect("advance claim liveness execution clock");

    let child_id = Uuid::new_v4();
    items
        .create(
            task(
                child_id,
                "Executable child control",
                false,
                Some(nonleaf_item),
                json!({}),
            ),
            idempotency(204),
        )
        .await
        .expect("turn the claimed item into a non-leaf");
    sqlx::query(
        "UPDATE items SET status = 'completed', revision = revision + 1, \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(terminal_item)
    .execute(&test_database.pool)
    .await
    .expect("make a claimed item terminal");
    items
        .trash(trashed_item, 1, idempotency(205))
        .await
        .expect("trash a claimed item");

    let schedules = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let access = owner_access(scope, "auth0|claim-liveness-owner");
    let mut request = compose_request();
    request.fixed_blocks.clear();
    let preview = compose_canonical_schedule(&items, &schedules, request)
        .await
        .expect("retired claims do not remain planning reservations");
    assert!(preview.plan.blocks.iter().any(|block| {
        block
            .item_id
            .is_some_and(|candidate| candidate.0 == child_id)
    }));
    assert!(preview.plan.blocks.iter().all(|block| {
        block.item_id.is_none_or(|candidate| {
            ![nonleaf_item, terminal_item, trashed_item].contains(&candidate.0)
        })
    }));
    let publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [205; 32],
                input_digest: digest_bytes(&preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("retired claims do not block the revision seal");
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, publication.revision.id).await,
        0
    );

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)] // Rehearses the passive-claim migration through a real compose and seal.
async fn migrated_passive_replacement_index_is_never_reallocated() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 20) {
        test_database
            .pool
            .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-ledger migration applies");
    }
    let scope = seed_scope(&test_database.pool).await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        Arc::new(SystemClock),
    ));
    let item_id = Uuid::new_v4();
    items
        .create(
            task(
                item_id,
                "Never reuse a passive migrated index",
                false,
                None,
                json!({}),
            ),
            idempotency(209),
        )
        .await
        .expect("create migration fixture item");
    let source_session_id = Uuid::new_v4();
    let started_at: chrono::DateTime<Utc> = "2026-08-31T06:00:00Z".parse().unwrap();
    let deferred_at: chrono::DateTime<Utc> = "2026-08-31T06:20:00Z".parse().unwrap();
    let completed_at: chrono::DateTime<Utc> = "2026-08-31T06:30:00Z".parse().unwrap();
    let move_start: chrono::DateTime<Utc> = "2026-09-01T11:00:00Z".parse().unwrap();
    let move_end: chrono::DateTime<Utc> = "2026-09-01T11:30:00Z".parse().unwrap();
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, observed_running_since, \
         paused_at, pause_until, pause_reason, move_start, move_end, ended_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, NULL, 9, NULL, $4, 'deferred', 1, 1200, 1200, $5, NULL, \
         NULL, NULL, NULL, NULL, $6, $7, $8, $5, $8)",
    )
    .bind(source_session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .bind(started_at)
    .bind(move_start)
    .bind(move_end)
    .bind(deferred_at)
    .execute(&test_database.pool)
    .await
    .expect("insert historical defer");
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, observed_running_since, \
         paused_at, pause_until, pause_reason, move_start, move_end, ended_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, NULL, 9, NULL, $4, 'completed', 1, 1800, 1800, $5, NULL, \
         NULL, NULL, NULL, NULL, NULL, NULL, $6, $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .bind(started_at)
    .bind(completed_at)
    .execute(&test_database.pool)
    .await
    .expect("insert the later completed semantic head");
    sqlx::query(
        "INSERT INTO execution_state (workspace_id, revision, active_session_id, updated_at) \
         VALUES ($1, 2, NULL, $2)",
    )
    .bind(scope.workspace_id)
    .bind(completed_at)
    .execute(&test_database.pool)
    .await
    .expect("seed the legacy execution clock");

    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 20)
        .expect("execution ledger migration is embedded");
    test_database
        .pool
        .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect("execution ledger migration applies");
    let (replacement_index, actionable): (i32, bool) = sqlx::query_as(
        "SELECT replacement_session_index, actionable \
         FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(source_session_id)
    .fetch_one(&test_database.pool)
    .await
    .expect("load migrated passive claim");
    assert_eq!(replacement_index, 10);
    assert!(!actionable);

    let schedules = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let access = owner_access(scope, "auth0|passive-index-owner");
    let mut request = compose_request();
    request.fixed_blocks.clear();
    let preview = compose_canonical_schedule(&items, &schedules, request)
        .await
        .expect("compose above every passive physical index");
    let block = preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block
                .item_id
                .is_some_and(|candidate| candidate.0 == item_id)
        })
        .expect("migrated item remains schedulable");
    assert_eq!(block.session_index, 11);
    let publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [209; 32],
                input_digest: digest_bytes(&preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: preview,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("publish without colliding with passive physical history");
    assert_eq!(
        deferred_binding_count(&test_database.pool, scope, publication.revision.id).await,
        0
    );

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)] // The migration cutover is one ordered, auditable rehearsal.
async fn legacy_schedule_upgrade_is_sealed_and_requires_one_fresh_publication() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
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
    ] {
        test_database
            .pool
            .execute(migration)
            .await
            .expect("legacy migration applies");
    }
    let scope = seed_scope(&test_database.pool).await;
    let item_repository = Arc::new(PostgresItemRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let items = Arc::new(ItemService::new(item_repository, Arc::new(SystemClock)));
    let item_id = Uuid::new_v4();
    items
        .create(
            task(item_id, "Fresh publication item", false, None, json!({})),
            idempotency(210),
        )
        .await
        .unwrap();

    let legacy_published = Uuid::new_v4();
    let legacy_published_block = Uuid::new_v4();
    let legacy_draft = Uuid::new_v4();
    let legacy_draft_block = Uuid::new_v4();
    let horizon_start: chrono::DateTime<Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
    let horizon_end: chrono::DateTime<Utc> = "2026-09-02T00:00:00Z".parse().unwrap();
    let legacy_published_at: chrono::DateTime<Utc> = "2026-08-31T20:00:00Z".parse().unwrap();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, horizon_start, \
         horizon_end, timezone_name, solver_version, input_digest, created_by_user_id, created_at, \
         published_at) VALUES ($1, $2, 1, 'published', $3, $4, 'Europe/Madrid', \
         'legacy-solver', $5, $6, $7, $7), ($8, $2, 2, 'draft', $3, $4, \
         'Europe/Madrid', 'legacy-solver', $9, $6, $7, NULL)",
    )
    .bind(legacy_published)
    .bind(scope.workspace_id)
    .bind(horizon_start)
    .bind(horizon_end)
    .bind(vec![1_u8; 32])
    .bind(scope.user_id)
    .bind(legacy_published_at)
    .bind(legacy_draft)
    .bind(vec![2_u8; 32])
    .execute(&test_database.pool)
    .await
    .unwrap();
    for (block_id, revision_id, title, sensitive) in [
        (
            legacy_published_block,
            legacy_published,
            "SYNTHETIC-LEGACY-PRIVATE-CANARY",
            true,
        ),
        (
            legacy_draft_block,
            legacy_draft,
            "Editable legacy draft",
            false,
        ),
    ] {
        sqlx::query(
            "INSERT INTO schedule_blocks (id, workspace_id, schedule_revision_id, block_kind, \
             title_snapshot, starts_at, ends_at, timezone_name, ordinal, is_fixed, is_sensitive) \
             VALUES ($1, $2, $3, 'item', $4, $5, $6, 'Europe/Madrid', 0, false, $7)",
        )
        .bind(block_id)
        .bind(scope.workspace_id)
        .bind(revision_id)
        .bind(title)
        .bind(
            "2026-09-01T09:00:00Z"
                .parse::<chrono::DateTime<Utc>>()
                .unwrap(),
        )
        .bind(
            "2026-09-01T10:00:00Z"
                .parse::<chrono::DateTime<Utc>>()
                .unwrap(),
        )
        .bind(sensitive)
        .execute(&test_database.pool)
        .await
        .unwrap();
    }

    test_database
        .pool
        .execute(include_str!("../migrations/0012_schedule_publication.sql"))
        .await
        .expect("schedule publication migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0013_schedule_seal_and_mcp_submission.sql"
        ))
        .await
        .expect("schedule seal migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0014_google_calendar_projection.sql"
        ))
        .await
        .expect("Calendar projection fence migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0015_transactional_proposal_applications.sql"
        ))
        .await
        .expect("transactional proposal application migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0016_mcp_simulation_evidence.sql"
        ))
        .await
        .expect("MCP simulation evidence migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0017_google_refresh_generations.sql"
        ))
        .await
        .expect("Google refresh generation migration applies");
    test_database
        .pool
        .execute(include_str!("../migrations/0018_execution_defer.sql"))
        .await
        .expect("execution defer migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0019_schedule_deferred_placements.sql"
        ))
        .await
        .expect("deferred placement migration applies");
    test_database
        .pool
        .execute(include_str!(
            "../migrations/0020_execution_progress_ledger.sql"
        ))
        .await
        .expect("execution progress ledger migration applies");

    let schedules = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let access = owner_access(scope, "auth0|legacy-upgrade-owner");
    assert!(matches!(
        schedules.current_native_schedule(&access).await,
        Err(SchedulingPortError::RepublishRequired)
    ));
    let legacy = schedules
        .get_schedule(
            &access,
            ScheduleQuery {
                start: horizon_start,
                end: horizon_end,
                detail: ScheduleDetail::Full,
            },
        )
        .await
        .unwrap();
    assert_eq!(legacy.revision, format!("1:{legacy_published}"));
    assert_eq!(legacy.redacted_count, 1);
    assert!(
        legacy.blocks[0].id.is_none()
            && legacy.blocks[0].item_id.is_none()
            && legacy.blocks[0].title.is_none()
    );
    assert_eq!(
        schedules
            .get_conflicts(
                &access,
                ConflictQuery {
                    start: horizon_start,
                    end: horizon_end,
                },
            )
            .await,
        Err(SchedulingPortError::RepublishRequired)
    );
    assert_eq!(
        schedules
            .simulate(
                &access,
                SimulationRequest {
                    base_revision: legacy.revision,
                    operations: vec![operation(PlanOperationKind::CreateItem, None, json!({}))],
                    assumptions: Vec::new(),
                },
            )
            .await,
        Err(SchedulingPortError::RepublishRequired)
    );
    assert!(
        sqlx::query("UPDATE schedule_blocks SET title_snapshot = 'forbidden edit' WHERE id = $1",)
            .bind(legacy_published_block)
            .execute(&test_database.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
             result_snapshot) VALUES ($1, $2, $3, '{}'::jsonb)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(legacy_published)
        .execute(&test_database.pool)
        .await
        .is_err()
    );
    sqlx::query("UPDATE schedule_blocks SET title_snapshot = 'edited draft' WHERE id = $1")
        .bind(legacy_draft_block)
        .execute(&test_database.pool)
        .await
        .expect("legacy draft content remains editable");
    sqlx::query("UPDATE schedule_revisions SET state = 'discarded' WHERE id = $1")
        .bind(legacy_draft)
        .execute(&test_database.pool)
        .await
        .expect("legacy draft remains discardable");

    let fresh_request = compose_request();
    let fresh_preview = compose_canonical_schedule(&items, &schedules, fresh_request)
        .await
        .unwrap();
    let fresh = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [211; 32],
                input_digest: digest_bytes(&fresh_preview.input_digest),
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: fresh_preview,
                published_at: "2000-01-01T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(fresh.revision.revision_number, 3);
    assert!(
        schedules
            .get_conflicts(
                &access,
                ConflictQuery {
                    start: horizon_start,
                    end: horizon_end,
                },
            )
            .await
            .is_ok()
    );
    assert!(
        schedules
            .simulate(
                &access,
                SimulationRequest {
                    base_revision: fresh.revision.revision,
                    operations: vec![operation(
                        PlanOperationKind::CreateItem,
                        None,
                        create_task_parameters("Fresh upgrade proposal"),
                    )],
                    assumptions: Vec::new(),
                },
            )
            .await
            .is_ok()
    );
    let legacy_state: String =
        sqlx::query_scalar("SELECT state FROM schedule_revisions WHERE id = $1")
            .bind(legacy_published)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert_eq!(legacy_state, "superseded");

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn calendar_projection_fences_preview_publication_and_exact_replay() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; schedule PostgreSQL test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let item_repository = Arc::new(PostgresItemRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let items = Arc::new(ItemService::new(item_repository, Arc::new(SystemClock)));
    items
        .create(
            task(
                Uuid::new_v4(),
                "Synthetic projection fence task",
                false,
                None,
                json!({}),
            ),
            idempotency(201),
        )
        .await
        .expect("create projection fence task");
    let schedules = Arc::new(PostgresSchedulingRepository::new(
        test_database.pool.clone(),
        scope,
    ));
    let (app, access_token) =
        credential_publish_app(&test_database.pool, scope, items.clone(), schedules.clone()).await;
    let request = compose_request();
    let account_id = seed_google_calendar_account(&test_database.pool, scope).await;
    let remote_calendar_canary = "synthetic-remote-calendar-content-must-not-leak";
    let first_collection = insert_blocking_calendar(
        &test_database.pool,
        scope,
        account_id,
        remote_calendar_canary,
        true,
    )
    .await;

    assert_projection_preview_unavailable(&app, &access_token, &request).await;

    set_failed_calendar_projection(&test_database.pool, first_collection).await;
    assert_projection_preview_unavailable(&app, &access_token, &request).await;

    let full_window_start = "2026-08-31T00:00:00Z".parse().unwrap();
    let full_window_end = "2026-09-03T00:00:00Z".parse().unwrap();
    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        1,
        "2026-09-01T01:00:00Z".parse().unwrap(),
        full_window_end,
        Utc::now(),
    )
    .await;
    assert_projection_preview_unavailable(&app, &access_token, &request).await;

    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        2,
        full_window_start,
        full_window_end,
        Utc::now() - chrono::Duration::minutes(31),
    )
    .await;
    assert_projection_preview_unavailable(&app, &access_token, &request).await;

    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        3,
        full_window_start,
        full_window_end,
        Utc::now() + chrono::Duration::minutes(1),
    )
    .await;
    assert_projection_preview_unavailable(&app, &access_token, &request).await;

    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        4,
        full_window_start,
        full_window_end,
        Utc::now(),
    )
    .await;
    let transaction_preview = compose_canonical_schedule(&items, &schedules, request.clone())
        .await
        .expect("compose with a fresh Calendar projection");
    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        4,
        full_window_start,
        full_window_end,
        Utc::now() + chrono::Duration::minutes(1),
    )
    .await;
    let transaction_result = schedules
        .publish(
            &ScheduleAccess {
                subject: "calendar-projection-fence-test".to_owned(),
                include_sensitive: false,
                workspace_id: Some(scope.workspace_id),
                user_id: Some(scope.user_id),
            },
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [202; 32],
                input_digest: digest_bytes(&transaction_preview.input_digest),
                timezone_name: request.timezone_name.clone(),
                manual_placement_approvals: Vec::new(),
                result: transaction_preview,
                published_at: Utc::now(),
            },
        )
        .await;
    assert!(matches!(
        transaction_result,
        Err(SchedulePublicationError::StaleComposition)
    ));

    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        5,
        full_window_start,
        full_window_end,
        Utc::now(),
    )
    .await;
    let generation_preview = public_schedule_preview(&app, &access_token, &request).await;
    assert!(
        generation_preview
            .get("calendar_projection_stamps")
            .is_none()
    );
    assert!(
        !generation_preview
            .to_string()
            .contains(remote_calendar_canary)
    );
    let generation_publish = schedule_publish_body(
        Uuid::new_v4(),
        generation_preview["input_digest"].as_str().unwrap(),
        &request,
    );
    sqlx::query(
        "UPDATE google_sync_collections SET planning_generation = planning_generation + 1, \
         planning_window_refreshed_at = clock_timestamp() WHERE id = $1",
    )
    .bind(first_collection)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_stale_schedule_publication(&app, &access_token, &generation_publish).await;

    let configuration_preview = public_schedule_preview(&app, &access_token, &request).await;
    let configuration_publish = schedule_publish_body(
        Uuid::new_v4(),
        configuration_preview["input_digest"].as_str().unwrap(),
        &request,
    );
    sqlx::query(
        "UPDATE google_sync_collections SET visible = NOT visible, revision = revision + 1 \
         WHERE id = $1",
    )
    .bind(first_collection)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_stale_schedule_publication(&app, &access_token, &configuration_publish).await;

    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        7,
        full_window_start,
        full_window_end,
        Utc::now(),
    )
    .await;
    let selection_preview = public_schedule_preview(&app, &access_token, &request).await;
    let selection_publish = schedule_publish_body(
        Uuid::new_v4(),
        selection_preview["input_digest"].as_str().unwrap(),
        &request,
    );
    let newly_selected_remote_canary = "synthetic-newly-selected-calendar-content";
    let second_collection = insert_blocking_calendar(
        &test_database.pool,
        scope,
        account_id,
        newly_selected_remote_canary,
        false,
    )
    .await;
    sqlx::query(
        "UPDATE google_sync_collections SET selected = true, revision = revision + 1 WHERE id = $1",
    )
    .bind(second_collection)
    .execute(&test_database.pool)
    .await
    .unwrap();
    assert_stale_schedule_publication(&app, &access_token, &selection_publish).await;
    assert_eq!(
        publication_counts(&test_database.pool, scope).await,
        (0, 0, 0, 0, 0),
        "projection invalidation must not create partial publication evidence"
    );

    set_complete_calendar_projection(
        &test_database.pool,
        first_collection,
        8,
        full_window_start,
        full_window_end,
        Utc::now(),
    )
    .await;
    set_complete_calendar_projection(
        &test_database.pool,
        second_collection,
        1,
        full_window_start,
        full_window_end,
        Utc::now(),
    )
    .await;
    let publish_preview = public_schedule_preview(&app, &access_token, &request).await;
    assert!(publish_preview.get("calendar_projection_stamps").is_none());
    let publish_key = Uuid::new_v4();
    let publish_body = schedule_publish_body(
        publish_key,
        publish_preview["input_digest"].as_str().unwrap(),
        &request,
    );
    let published = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &publish_body,
            &access_token,
        ))
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::OK);
    let published = body_json(published).await;
    assert_eq!(published["replayed"], false);
    let published_id = Uuid::parse_str(published["revision"]["id"].as_str().unwrap()).unwrap();

    let snapshot: Value = sqlx::query_scalar(
        "SELECT result_snapshot FROM schedule_revision_details \
         WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(published_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert!(
        snapshot["compose"]
            .get("calendar_projection_stamps")
            .is_none()
    );
    let stamps = snapshot["evidence"]["calendar_projection_stamps"]
        .as_array()
        .expect("durable projection stamps");
    assert_eq!(stamps.len(), 2);
    let stamped_collection_ids: std::collections::BTreeSet<_> = stamps
        .iter()
        .map(|stamp| Uuid::parse_str(stamp["collection_id"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        stamped_collection_ids,
        std::collections::BTreeSet::from([first_collection, second_collection])
    );
    for stamp in stamps {
        let keys: std::collections::BTreeSet<_> = stamp
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "collection_id",
                "collection_revision",
                "generation",
                "refreshed_at",
                "window_end",
                "window_start",
            ])
        );
    }
    let encoded_snapshot = snapshot.to_string();
    assert!(!encoded_snapshot.contains(remote_calendar_canary));
    assert!(!encoded_snapshot.contains(newly_selected_remote_canary));
    assert!(!encoded_snapshot.contains(&account_id.to_string()));

    sqlx::query("UPDATE google_sync_collections SET revision = revision + 1 WHERE id = $1")
        .bind(first_collection)
        .execute(&test_database.pool)
        .await
        .unwrap();
    let replay = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &publish_body,
            &access_token,
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay = body_json(replay).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"], published["revision"]);

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn native_schedule_replication_is_exact_scoped_and_cross_process_durable() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; native schedule replication test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        Arc::new(SystemClock),
    ));
    items
        .create(
            task(
                Uuid::new_v4(),
                "Native replica task",
                false,
                None,
                json!({}),
            ),
            idempotency(241),
        )
        .await
        .expect("create native replica task");
    let stream_config = ScheduleInvalidationConfig::new(
        StdDuration::from_millis(50),
        StdDuration::from_millis(250),
        StdDuration::from_secs(3),
        8,
    )
    .expect("bounded native stream config");
    let schedules = Arc::new(
        PostgresSchedulingRepository::new(test_database.pool.clone(), scope)
            .with_invalidation_config(stream_config),
    );
    let (app, access_token) =
        credential_publish_app(&test_database.pool, scope, items.clone(), schedules.clone()).await;

    let missing = app
        .clone()
        .oneshot(authenticated_get("/v1/schedule/current", &access_token))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(missing.headers()[header::PRAGMA], "no-cache");
    assert_eq!(
        body_json(missing).await,
        json!({
            "error": {
                "code": "not_found",
                "message": "Published schedule was not found"
            }
        })
    );

    let unacceptable = app
        .clone()
        .oneshot(authenticated_get("/v1/schedule/stream", &access_token))
        .await
        .unwrap();
    assert_eq!(unacceptable.status(), StatusCode::NOT_ACCEPTABLE);
    let malformed = app
        .clone()
        .oneshot(schedule_stream_request(&access_token, Some("01")))
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let ahead = app
        .clone()
        .oneshot(schedule_stream_request(&access_token, Some("1")))
        .await
        .unwrap();
    assert_eq!(ahead.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(ahead).await,
        json!({
            "error": {
                "code": "conflict",
                "message": "schedule stream cursor is ahead of authoritative state",
                "details": {"cursor_revision": 1, "head_revision": 0}
            }
        })
    );
    let empty_stream = app
        .clone()
        .oneshot(schedule_stream_request(&access_token, Some("0")))
        .await
        .unwrap();
    assert_eq!(empty_stream.status(), StatusCode::OK);
    assert_eq!(
        empty_stream.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    drop(empty_stream);

    let request = compose_request();
    let preview = public_schedule_preview(&app, &access_token, &request).await;
    let publish_body = schedule_publish_body(
        Uuid::new_v4(),
        preview["input_digest"].as_str().unwrap(),
        &request,
    );
    let published = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &publish_body,
            &access_token,
        ))
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::OK);
    assert_eq!(body_json(published).await["revision"]["revision_number"], 1);

    let current = app
        .clone()
        .oneshot(authenticated_get("/v1/schedule/current", &access_token))
        .await
        .unwrap();
    assert_eq!(current.status(), StatusCode::OK);
    assert_eq!(
        current.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(current.headers()[header::PRAGMA], "no-cache");
    let etag = current.headers()[header::ETAG].to_str().unwrap().to_owned();
    let current = body_json(current).await;
    assert_eq!(
        current
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["revision", "schedule"])
    );
    assert_eq!(current["revision"]["revision_number"], 1);
    assert_eq!(current["schedule"], preview);
    assert_eq!(
        etag,
        format!(r#""{}""#, current["revision"]["revision"].as_str().unwrap())
    );
    let encoded_current = current.to_string();
    assert!(!encoded_current.contains("planning_evidence"));
    assert!(!encoded_current.contains("manual_placement_state"));
    assert!(!encoded_current.contains("calendar_projection_stamps"));

    let mut catch_up = app
        .clone()
        .oneshot(schedule_stream_request(&access_token, Some("0")))
        .await
        .unwrap();
    assert_schedule_revision_frame(
        &next_schedule_stream_chunk(&mut catch_up, StdDuration::from_secs(1))
            .await
            .expect("catch-up revision event"),
        1,
    );

    let mut replay_stream = app
        .clone()
        .oneshot(schedule_stream_request(&access_token, Some("1")))
        .await
        .unwrap();
    let replay = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &publish_body,
            &access_token,
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(body_json(replay).await["replayed"], true);
    assert!(
        timeout(
            StdDuration::from_millis(120),
            replay_stream.body_mut().frame()
        )
        .await
        .is_err(),
        "an idempotent replay must not generate a false invalidation"
    );

    let mut second_request = request.clone();
    second_request.fixed_blocks[0].title = "Changed public fixed block".to_owned();
    let second_preview = public_schedule_preview(&app, &access_token, &second_request).await;
    let second_body = schedule_publish_body(
        Uuid::new_v4(),
        second_preview["input_digest"].as_str().unwrap(),
        &second_request,
    );
    let mut live = app
        .clone()
        .oneshot(schedule_stream_request(&access_token, Some("1")))
        .await
        .unwrap();
    let second = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &second_body,
            &access_token,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(body_json(second).await["revision"]["revision_number"], 2);
    assert_schedule_revision_frame(
        &next_schedule_stream_chunk(&mut live, StdDuration::from_secs(1))
            .await
            .expect("live revision event"),
        2,
    );

    let polling_schedules = Arc::new(
        PostgresSchedulingRepository::new(test_database.pool.clone(), scope)
            .with_invalidation_config(stream_config),
    );
    let (polling_app, polling_token) =
        credential_publish_app(&test_database.pool, scope, items.clone(), polling_schedules).await;
    let mut polling_stream = polling_app
        .clone()
        .oneshot(schedule_stream_request(&polling_token, Some("2")))
        .await
        .unwrap();
    let mut third_request = second_request;
    third_request.fixed_blocks[0].title = "Cross-process publication".to_owned();
    let third_preview = public_schedule_preview(&app, &access_token, &third_request).await;
    let third_body = schedule_publish_body(
        Uuid::new_v4(),
        third_preview["input_digest"].as_str().unwrap(),
        &third_request,
    );
    let third = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/publish",
            &third_body,
            &access_token,
        ))
        .await
        .unwrap();
    assert_eq!(third.status(), StatusCode::OK);
    assert_eq!(body_json(third).await["revision"]["revision_number"], 3);
    assert_schedule_revision_frame(
        &next_schedule_stream_chunk(&mut polling_stream, StdDuration::from_secs(1))
            .await
            .expect("durable polling revision event"),
        3,
    );

    let foreign_scope = seed_scope(&test_database.pool).await;
    let (foreign_app, foreign_token) =
        credential_publish_app(&test_database.pool, foreign_scope, items, schedules).await;
    let foreign_current = foreign_app
        .clone()
        .oneshot(authenticated_get("/v1/schedule/current", &foreign_token))
        .await
        .unwrap();
    assert_eq!(foreign_current.status(), StatusCode::NOT_FOUND);
    let foreign_stream = foreign_app
        .clone()
        .oneshot(schedule_stream_request(&foreign_token, Some("0")))
        .await
        .unwrap();
    assert_eq!(foreign_stream.status(), StatusCode::FORBIDDEN);

    test_database.destroy().await;
}

async fn seed_google_calendar_account(pool: &PgPool, scope: DatabaseScope) -> Uuid {
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
         display_label, encrypted_credentials, credential_key_version, granted_scopes, status, \
         sync_enabled) VALUES ($1, $2, $3, 'google', $4, \
         'Synthetic projection account', $5, 1, \
         ARRAY['https://www.googleapis.com/auth/calendar.readonly']::text[], 'active', true)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("synthetic-calendar-account-{account_id}"))
    .bind(vec![0x53_u8; 32])
    .execute(pool)
    .await
    .unwrap();
    account_id
}

async fn insert_blocking_calendar(
    pool: &PgPool,
    scope: DatabaseScope,
    account_id: Uuid,
    remote_collection_id: &str,
    selected: bool,
) -> Uuid {
    let collection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
         collection_kind, remote_collection_id, display_name, provider_access_role, \
         provider_selected, selected, visible, sync_role, discovered_at, configured_at, \
         created_at, updated_at) VALUES ($1, $2, $3, $4, 'calendar', $5, \
         'Synthetic blocking calendar', 'owner', $6, $6, true, 'blocking', \
         clock_timestamp(), clock_timestamp(), clock_timestamp(), clock_timestamp())",
    )
    .bind(collection_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .bind(remote_collection_id)
    .bind(selected)
    .execute(pool)
    .await
    .unwrap();
    collection_id
}

async fn set_failed_calendar_projection(pool: &PgPool, collection_id: Uuid) {
    sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'failed', \
         planning_collection_revision = NULL, planning_window_start = NULL, \
         planning_window_end = NULL, planning_window_refreshed_at = NULL, \
         planning_last_error_code = 'synthetic_projection_failure' WHERE id = $1",
    )
    .bind(collection_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_complete_calendar_projection(
    pool: &PgPool,
    collection_id: Uuid,
    generation: i64,
    window_start: chrono::DateTime<Utc>,
    window_end: chrono::DateTime<Utc>,
    refreshed_at: chrono::DateTime<Utc>,
) {
    sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'complete', \
         planning_generation = $2, planning_collection_revision = revision, \
         planning_window_start = $3, planning_window_end = $4, \
         planning_window_refreshed_at = $5, planning_last_error_code = NULL WHERE id = $1",
    )
    .bind(collection_id)
    .bind(generation)
    .bind(window_start)
    .bind(window_end)
    .bind(refreshed_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn assert_projection_preview_unavailable(
    app: &Router,
    access_token: &str,
    request: &ComposeScheduleRequest,
) {
    let response = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/preview",
            &serde_json::to_value(request).unwrap(),
            access_token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
}

async fn public_schedule_preview(
    app: &Router,
    access_token: &str,
    request: &ComposeScheduleRequest,
) -> Value {
    let response = app
        .clone()
        .oneshot(json_request(
            "/v1/schedule/preview",
            &serde_json::to_value(request).unwrap(),
            access_token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

fn schedule_publish_body(
    idempotency_key: Uuid,
    expected_input_digest: &str,
    request: &ComposeScheduleRequest,
) -> Value {
    json!({
        "idempotency_key": idempotency_key,
        "expected_input_digest": expected_input_digest,
        "schedule": request,
    })
}

async fn assert_stale_schedule_publication(app: &Router, access_token: &str, body: &Value) {
    let response = app
        .clone()
        .oneshot(json_request("/v1/schedule/publish", body, access_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "schedule_publication_stale");
}

fn compose_request() -> ComposeScheduleRequest {
    serde_json::from_value(json!({
        "as_of": "2026-09-01T06:00:00Z",
        "horizon_start": "2026-09-01T00:00:00Z",
        "horizon_end": "2026-09-02T00:00:00Z",
        "timezone_name": "Europe/Madrid",
        "availability": [{
            "start": "2026-09-01T07:00:00Z",
            "end": "2026-09-01T18:00:00Z",
            "contexts": [],
            "location": null,
            "energy": "deep"
        }],
        "fixed_blocks": [{
            "id": Uuid::new_v4(),
            "is_sensitive": false,
            "title": "Public fixed block",
            "start": "2026-09-01T09:00:00Z",
            "end": "2026-09-01T10:00:00Z",
            "source": "manual"
        }, {
            "id": Uuid::new_v4(),
            "is_sensitive": true,
            "title": "Private overlapping fixed block",
            "start": "2026-09-01T09:30:00Z",
            "end": "2026-09-01T10:30:00Z",
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
    .unwrap()
}

/// Keeps execution/defer integration fixtures ahead of the wall clock without weakening the
/// production requirement that a defer target outlive its short-lived assessment capability.
fn future_fixture_day_start() -> chrono::DateTime<Utc> {
    (Utc::now() + chrono::Duration::days(2))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("future fixture day is representable")
        .and_utc()
}

fn compose_request_for_day(day_start: chrono::DateTime<Utc>) -> ComposeScheduleRequest {
    let mut request = compose_request();
    request.as_of = day_start + chrono::Duration::hours(6);
    request.horizon_start = day_start;
    request.horizon_end = day_start + chrono::Duration::days(1);
    let [availability] = request.availability.as_mut_slice() else {
        panic!("the schedule fixture must contain exactly one availability window");
    };
    availability.start = day_start + chrono::Duration::hours(7);
    availability.end = day_start + chrono::Duration::hours(18);
    request
}

fn deferred_compose_request(
    item_id: Uuid,
    item_revision: u64,
    move_start: chrono::DateTime<Utc>,
    move_end: chrono::DateTime<Utc>,
) -> ComposeScheduleRequest {
    let fixture_day = move_start
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("defer fixture day is representable")
        .and_utc();
    let mut request = compose_request_for_day(fixture_day);
    request.fixed_blocks.clear();
    request.previous_assignments = vec![PreviousAssignmentInput {
        item_id,
        item_revision,
        occurrence_id: None,
        blocks: vec![PreviousBlockInput {
            start: move_start,
            end: move_end,
            session_index: 0,
        }],
        pinned: true,
    }];
    request
}

async fn insert_live_legacy_claim(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    terminal_at: chrono::DateTime<Utc>,
    move_start: chrono::DateTime<Utc>,
) -> Uuid {
    let source_session_id = Uuid::new_v4();
    let move_end = move_start + chrono::Duration::minutes(30);
    let started_at = terminal_at - chrono::Duration::seconds(5);
    let mut transaction = pool.begin().await.expect("begin raw legacy claim");
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         execution_epoch, occurrence_id, session_index, planned_block_id, source_device_id, state, \
         revision, accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, 1, 1, NULL, 0, NULL, $4, \
         'deferred', 1, 0, 0, $5, NULL, NULL, NULL, NULL, NULL, $6, $7, $8, $5, $8)",
    )
    .bind(source_session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .bind(started_at)
    .bind(move_start)
    .bind(move_end)
    .bind(terminal_at)
    .execute(&mut *transaction)
    .await
    .expect("insert raw deferred source");
    sqlx::query(
        "INSERT INTO execution_defer_replacement_claims (workspace_id, \
         source_deferred_session_id, item_id, source_item_revision, execution_epoch, \
         occurrence_id, source_session_index, replacement_session_index, \
         planned_duration_seconds, planned_duration_source, actionable, consumed_before_seconds, \
         consumed_by_source_seconds, remaining_duration_seconds, move_start, move_end, created_at) \
         VALUES ($1, $2, $3, 1, 1, NULL, 0, 1, 1800, 'legacy_move_window', true, 0, 0, \
         1800, $4, $5, $6)",
    )
    .bind(scope.workspace_id)
    .bind(source_session_id)
    .bind(item_id)
    .bind(move_start)
    .bind(move_end)
    .bind(terminal_at)
    .execute(&mut *transaction)
    .await
    .expect("insert exact live legacy claim");
    transaction
        .commit()
        .await
        .expect("commit deferred source and claim together");
    source_session_id
}

fn goal(id: Uuid, title: &str, sensitive: bool, parent_id: Option<Uuid>) -> NewItem {
    NewItem {
        id,
        is_sensitive: sensitive,
        kind: ItemKind::Goal,
        status: ItemStatus::Planned,
        title: title.to_owned(),
        notes: None,
        timezone_name: "Europe/Madrid".to_owned(),
        duration_seconds: None,
        deadline_at: None,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: json!({}),
        split_policy: SplitPolicy::Indivisible,
        importance: 50,
        urgency: 50,
        parent_id,
        sibling_order: 0,
    }
}

fn task(
    id: Uuid,
    title: &str,
    sensitive: bool,
    parent_id: Option<Uuid>,
    flexible_constraints: Value,
) -> NewItem {
    NewItem {
        id,
        is_sensitive: sensitive,
        kind: ItemKind::Task,
        status: ItemStatus::Planned,
        title: title.to_owned(),
        notes: None,
        timezone_name: "Europe/Madrid".to_owned(),
        duration_seconds: Some(3_600),
        deadline_at: Some("2026-09-01T17:00:00Z".parse().unwrap()),
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints,
        split_policy: SplitPolicy::Indivisible,
        importance: 80,
        urgency: 60,
        parent_id,
        sibling_order: 0,
    }
}

fn idempotency(marker: u8) -> IdempotencyKey {
    IdempotencyKey {
        key: format!("schedule-postgres-{marker:03}"),
        fingerprint: [marker; 32],
    }
}

fn execution_idempotency(key: &str, marker: u8) -> ExecutionIdempotencyKey {
    ExecutionIdempotencyKey {
        key: key.to_owned(),
        fingerprint: [marker; 32],
    }
}

fn operation(kind: PlanOperationKind, target_id: Option<&str>, parameters: Value) -> PlanOperation {
    let Value::Object(parameters) = parameters else {
        panic!("test operation parameters must be an object");
    };
    PlanOperation {
        kind,
        target_id: target_id.map(str::to_owned),
        parameters: parameters.into_iter().collect(),
    }
}

fn create_task_parameters(title: &str) -> Value {
    let mut parameters =
        serde_json::to_value(task(Uuid::new_v4(), title, false, None, json!({}))).unwrap();
    parameters.as_object_mut().unwrap().remove("id");
    parameters
}

fn proposal_submission(
    _subject: &str,
    idempotency_key: &str,
    request_fingerprint: [u8; 32],
    request: &SimulationRequest,
    simulation_token: String,
) -> ProposalSubmissionSpec {
    ProposalSubmissionSpec {
        idempotency_key: idempotency_key.to_owned(),
        request_fingerprint,
        simulation_token,
        request: request.clone(),
        title: "Synthetic durable MCP proposal".to_owned(),
        explanation: "Exercises transactional submission.".to_owned(),
        source_conversation_label: "synthetic conversation".to_owned(),
        source_client_label: Some("schedule-postgres-test".to_owned()),
        source_request_id: Uuid::new_v4().to_string(),
        expires_at: Utc::now() + chrono::Duration::days(1),
    }
}

fn owner_access(scope: DatabaseScope, subject: &str) -> ScheduleAccess {
    ScheduleAccess {
        subject: subject.to_owned(),
        include_sensitive: false,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    }
}

async fn set_item_sensitivity(items: &ItemService, item_id: Uuid, is_sensitive: bool, marker: u8) {
    let current = items.get(item_id).await.unwrap();
    items
        .replace(
            item_id,
            current.revision,
            ReplaceItem {
                is_sensitive,
                kind: current.kind,
                status: current.status,
                title: current.title,
                notes: current.notes,
                timezone_name: current.timezone_name,
                duration_seconds: current.duration_seconds,
                deadline_at: current.deadline_at,
                earliest_start_at: current.earliest_start_at,
                recurrence: current.recurrence,
                flexible_constraints: current.flexible_constraints,
                split_policy: current.split_policy,
                importance: current.importance,
                urgency: current.urgency,
                parent_id: current.parent_id,
                sibling_order: current.sibling_order,
            },
            idempotency(marker),
        )
        .await
        .unwrap();
}

async fn schedule_item_is_redacted(
    schedules: &PostgresSchedulingRepository,
    access: &ScheduleAccess,
    known_start: chrono::DateTime<Utc>,
    known_end: chrono::DateTime<Utc>,
) -> bool {
    schedules
        .get_schedule(
            access,
            ScheduleQuery {
                start: "2026-09-01T00:00:00Z".parse().unwrap(),
                end: "2026-09-02T00:00:00Z".parse().unwrap(),
                detail: ScheduleDetail::Full,
            },
        )
        .await
        .unwrap()
        .blocks
        .iter()
        .any(|block| {
            block.start == known_start
                && block.end == known_end
                && block.redacted
                && block.id.is_none()
        })
}

async fn assert_simulation_unconsumed(pool: &PgPool, scope: DatabaseScope, token: &str) {
    let mut hash = Sha256::new();
    hash.update(b"dayweave.schedule-simulation-token.v1\0");
    hash.update(token.as_bytes());
    let consumed_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT consumed_at FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 \
         AND token_hash = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(hash.finalize().as_slice())
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(consumed_at.is_none());
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
            .unwrap();
            if blocked >= minimum {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("competing PostgreSQL query reached the intended lock");
}

fn postgres_error_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
}

async fn publish_concurrently_after_lock_barrier(
    pool: &PgPool,
    scope: DatabaseScope,
    repository: Arc<PostgresSchedulingRepository>,
    access: &ScheduleAccess,
    left: PublishScheduleSpec,
    right: PublishScheduleSpec,
) -> (
    Result<dayweave_api::scheduling::SchedulePublication, SchedulePublicationError>,
    Result<dayweave_api::scheduling::SchedulePublication, SchedulePublicationError>,
) {
    let mut blocker = pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text, 0))",
    )
    .bind(scope.workspace_id)
    .execute(&mut *blocker)
    .await
    .unwrap();

    let left_repository = repository.clone();
    let right_repository = repository;
    let left_access = access.clone();
    let right_access = access.clone();
    let left = tokio::spawn(async move { left_repository.publish(&left_access, left).await });
    let right = tokio::spawn(async move { right_repository.publish(&right_access, right).await });
    wait_for_blocked_queries(pool, blocker_pid, 2).await;
    blocker.commit().await.unwrap();
    (left.await.unwrap(), right.await.unwrap())
}

#[allow(clippy::too_many_arguments)] // Every argument binds one rollback assertion to its capability.
async fn assert_private_simulation_rejected(
    pool: &PgPool,
    scope: DatabaseScope,
    schedules: &PostgresSchedulingRepository,
    access: &ScheduleAccess,
    request: &SimulationRequest,
    simulation: &dayweave_api::scheduling::SimulationResult,
    submission_key: &str,
    fingerprint: [u8; 32],
) {
    assert_eq!(
        schedules
            .consume_simulation(
                access,
                &simulation.simulation_token,
                &simulation.request_digest,
            )
            .await,
        Err(SchedulingPortError::NotFound)
    );
    assert_simulation_unconsumed(pool, scope, &simulation.simulation_token).await;

    let before = proposal_artifact_counts(pool, scope).await;
    let spec = proposal_submission(
        &access.subject,
        submission_key,
        fingerprint,
        request,
        simulation.simulation_token.clone(),
    );
    assert!(matches!(
        schedules.submit_proposal(access, spec).await,
        Err(ProposalSubmissionError::Simulation(
            SchedulingPortError::NotFound
        ))
    ));
    assert_simulation_unconsumed(pool, scope, &simulation.simulation_token).await;
    assert_eq!(proposal_artifact_counts(pool, scope).await, before);
}

#[allow(clippy::too_many_arguments)]
async fn submit_actionable_operation(
    pool: &PgPool,
    scope: DatabaseScope,
    schedules: &PostgresSchedulingRepository,
    access: &ScheduleAccess,
    base_revision: &str,
    operation: PlanOperation,
    idempotency_key: &str,
    fingerprint: [u8; 32],
    expected_kind: ProposalKind,
) -> Proposal {
    let request = SimulationRequest {
        base_revision: base_revision.to_owned(),
        operations: vec![operation],
        assumptions: vec!["Typed bridge integration proof".to_owned()],
    };
    let simulation = schedules.simulate(access, request.clone()).await.unwrap();
    assert!(simulation.application_ready);
    assert_eq!(
        simulation.change_set_schema.as_deref(),
        Some("dayweave.proposal-change-set/1")
    );
    let public_result = serde_json::to_value(&simulation).unwrap();
    assert!(public_result.get("proposal_evidence").is_none());

    let submission = schedules
        .submit_proposal(
            access,
            proposal_submission(
                &access.subject,
                idempotency_key,
                fingerprint,
                &request,
                simulation.simulation_token,
            ),
        )
        .await
        .unwrap();
    assert!(!submission.duplicate);
    assert_eq!(submission.proposal.source, ProposalSource::ExternalMcp);
    assert_eq!(submission.proposal.kind, expected_kind);
    ProposalChangeSet::from_payload(&submission.proposal.payload)
        .expect("application-ready simulation materializes the strict typed payload");
    assert_submission_proof(pool, scope, submission.proposal.id, &request, "actionable").await;
    submission.proposal
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn assert_actionable_proposal_bridge(
    pool: &PgPool,
    scope: DatabaseScope,
    items: &ItemService,
    schedules: &PostgresSchedulingRepository,
    access: &ScheduleAccess,
    base_revision: &str,
    target_item_id: Uuid,
) {
    let generated_item_marker = Uuid::new_v4();
    let mut create_parameters = serde_json::to_value(task(
        generated_item_marker,
        "AI-created typed task",
        false,
        None,
        json!({"preferred_period": "afternoon"}),
    ))
    .unwrap();
    create_parameters.as_object_mut().unwrap().remove("id");
    let created = submit_actionable_operation(
        pool,
        scope,
        schedules,
        access,
        base_revision,
        operation(PlanOperationKind::CreateItem, None, create_parameters),
        "typed-bridge-create",
        [101; 32],
        ProposalKind::CreateItem,
    )
    .await;
    let create_change_set = ProposalChangeSet::from_payload(&created.payload).unwrap();
    match create_change_set.commands.as_slice() {
        [ProposalCommand::CreateItem { command_id, item }] => {
            assert_ne!(*command_id, Uuid::nil());
            assert_ne!(item.id, Uuid::nil());
            assert_ne!(item.id, generated_item_marker);
            assert_eq!(item.title, "AI-created typed task");
            assert_eq!(item.kind, ItemKind::Task);
        }
        commands => panic!("unexpected create change set: {commands:?}"),
    }

    let current = items.get(target_item_id).await.unwrap();
    let target = target_item_id.to_string();
    let completed = submit_actionable_operation(
        pool,
        scope,
        schedules,
        access,
        base_revision,
        operation(PlanOperationKind::CompleteItem, Some(&target), json!({})),
        "typed-bridge-complete",
        [102; 32],
        ProposalKind::UpdateItem,
    )
    .await;
    let complete_change_set = ProposalChangeSet::from_payload(&completed.payload).unwrap();
    match complete_change_set.commands.as_slice() {
        [
            ProposalCommand::ReplaceItem {
                item_id,
                expected_revision,
                item,
                ..
            },
        ] => {
            assert_eq!(*item_id, target_item_id);
            assert_eq!(*expected_revision, current.revision);
            assert_eq!(item.status, ItemStatus::Completed);
            assert_eq!(item.title, current.title);
        }
        commands => panic!("unexpected complete change set: {commands:?}"),
    }

    let deleted = submit_actionable_operation(
        pool,
        scope,
        schedules,
        access,
        base_revision,
        operation(PlanOperationKind::DeleteItem, Some(&target), json!({})),
        "typed-bridge-delete",
        [103; 32],
        ProposalKind::UpdateItem,
    )
    .await;
    let delete_change_set = ProposalChangeSet::from_payload(&deleted.payload).unwrap();
    match delete_change_set.commands.as_slice() {
        [
            ProposalCommand::TrashItem {
                item_id,
                expected_revision,
                ..
            },
        ] => {
            assert_eq!(*item_id, target_item_id);
            assert_eq!(*expected_revision, current.revision);
        }
        commands => panic!("unexpected delete change set: {commands:?}"),
    }

    let constrained = submit_actionable_operation(
        pool,
        scope,
        schedules,
        access,
        base_revision,
        operation(
            PlanOperationKind::UpdateConstraint,
            Some(&target),
            json!({
                "duration_seconds": 5_400,
                "flexible_constraints": {"preferred_period": "morning"}
            }),
        ),
        "typed-bridge-constraint",
        [104; 32],
        ProposalKind::ConstraintChange,
    )
    .await;
    let constraint_change_set = ProposalChangeSet::from_payload(&constrained.payload).unwrap();
    match constraint_change_set.commands.as_slice() {
        [
            ProposalCommand::ReplaceItem {
                item_id,
                expected_revision,
                item,
                ..
            },
        ] => {
            assert_eq!(*item_id, target_item_id);
            assert_eq!(*expected_revision, current.revision);
            assert_eq!(item.duration_seconds, Some(5_400));
            assert_eq!(
                item.flexible_constraints,
                json!({"preferred_period": "morning"})
            );
            assert_eq!(item.status, current.status);
        }
        commands => panic!("unexpected constraint change set: {commands:?}"),
    }

    let mapped_request = SimulationRequest {
        base_revision: base_revision.to_owned(),
        operations: vec![operation(
            PlanOperationKind::UpdateConstraint,
            Some(&target),
            json!({"duration_seconds": 6_000}),
        )],
        assumptions: vec!["Provider mapping may change before submission".to_owned()],
    };
    let before_mapping = schedules
        .simulate(access, mapped_request.clone())
        .await
        .unwrap();
    assert!(before_mapping.application_ready);
    let mapping_id = mark_provider_mapped_dayweave(pool, scope, &current).await;
    assert!(matches!(
        schedules
            .submit_proposal(
                access,
                proposal_submission(
                    &access.subject,
                    "typed-bridge-provider-race",
                    [105; 32],
                    &mapped_request,
                    before_mapping.simulation_token.clone(),
                ),
            )
            .await,
        Err(ProposalSubmissionError::Simulation(
            SchedulingPortError::InvalidQuery(_)
        ))
    ));
    assert_simulation_unconsumed(pool, scope, &before_mapping.simulation_token).await;

    let mapped_target = schedules
        .simulate(access, mapped_request.clone())
        .await
        .unwrap();
    assert!(!mapped_target.application_ready);
    assert!(mapped_target.change_set_schema.is_none());

    let mut child_parameters = serde_json::to_value(task(
        Uuid::new_v4(),
        "Child of mapped item",
        false,
        Some(target_item_id),
        json!({}),
    ))
    .unwrap();
    child_parameters.as_object_mut().unwrap().remove("id");
    let mapped_parent = schedules
        .simulate(
            access,
            SimulationRequest {
                base_revision: base_revision.to_owned(),
                operations: vec![operation(
                    PlanOperationKind::CreateItem,
                    None,
                    child_parameters,
                )],
                assumptions: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(!mapped_parent.application_ready);
    assert!(mapped_parent.change_set_schema.is_none());

    sqlx::query(
        "UPDATE provider_sync_mappings SET tombstoned_at = clock_timestamp(), \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(mapping_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn mark_provider_mapped_dayweave(pool: &PgPool, scope: DatabaseScope, item: &Item) -> Uuid {
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
         display_label, encrypted_credentials, credential_key_version, status, sync_enabled, \
         is_default) VALUES ($1,$2,$3,'google',$4,'Synthetic bridge provider',$5,1,'active',true,false)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("synthetic-bridge-provider-{account_id}"))
    .bind(vec![0xA5_u8; 64])
    .execute(pool)
    .await
    .unwrap();
    let mapping_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, entity_kind, \
         local_entity_id, remote_resource_id, local_revision, sync_state, ownership, created_at, \
         updated_at) VALUES ($1,$2,$3,'item',$4,$5,$6,'synced','dayweave',$7,$7)",
    )
    .bind(mapping_id)
    .bind(scope.workspace_id)
    .bind(account_id)
    .bind(item.id)
    .bind(format!("synthetic-bridge-remote-{}", item.id))
    .bind(i64::try_from(item.revision).unwrap())
    .bind(Utc::now())
    .execute(pool)
    .await
    .unwrap();
    mapping_id
}

async fn assert_submission_proof(
    pool: &PgPool,
    scope: DatabaseScope,
    proposal_id: Uuid,
    request: &SimulationRequest,
    expected_outcome: &str,
) {
    let (
        request_digest,
        request_hash,
        evidence_schema,
        evidence_hash,
        outcome,
        compiled_payload_hash,
        proposal_payload_hash,
        hidden_evidence_present,
    ): SubmissionProofRow = sqlx::query_as(
        "SELECT submission.simulation_request_digest, submission.simulation_request_hash, \
           submission.simulation_evidence_schema, submission.simulation_evidence_hash, \
           submission.compilation_outcome, submission.compiled_payload_hash, \
           submission.proposal_payload_hash, simulation.result_snapshot ? 'proposal_evidence' \
         FROM mcp_proposal_submissions AS submission \
         JOIN schedule_simulations AS simulation \
           ON simulation.workspace_id = submission.workspace_id \
          AND simulation.user_id = submission.user_id \
          AND simulation.id = submission.simulation_id \
         WHERE submission.workspace_id = $1 AND submission.proposal_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let expected_hash = simulation_request_hash(request).unwrap();
    assert_eq!(request_hash, expected_hash);
    assert_eq!(request_digest, expected_hash[..16]);
    assert_eq!(evidence_schema, 1);
    assert_eq!(evidence_hash.len(), 32);
    assert_eq!(outcome, expected_outcome);
    assert_eq!(proposal_payload_hash.len(), 32);
    assert!(hidden_evidence_present);
    if expected_outcome == "actionable" {
        assert_eq!(
            compiled_payload_hash.as_deref(),
            Some(proposal_payload_hash.as_slice())
        );
    } else {
        assert!(compiled_payload_hash.is_none());
    }
}

async fn proposal_artifact_counts(pool: &PgPool, scope: DatabaseScope) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM proposals WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM outbox_messages WHERE workspace_id = $1 \
             AND aggregate_type = 'proposal'), \
           (SELECT COUNT(*) FROM audit_operations WHERE workspace_id = $1 \
             AND entity_type = 'proposal'), \
           (SELECT COUNT(*) FROM mcp_proposal_submissions WHERE workspace_id = $1)",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn publication_counts(pool: &PgPool, scope: DatabaseScope) -> (i64, i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_blocks WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_revision_details WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_publication_requests WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM audit_operations WHERE workspace_id = $1 \
             AND operation_type = 'schedule.published')",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn publication_attestation_counts(
    pool: &PgPool,
    scope: DatabaseScope,
) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM schedule_revisions WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_blocks WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_revision_details WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_defer_replacement_placements WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM schedule_publication_requests WHERE workspace_id = $1), \
           (SELECT COUNT(*) FROM audit_operations WHERE workspace_id = $1 \
             AND operation_type = 'schedule.published')",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn deferred_binding_count(pool: &PgPool, scope: DatabaseScope, revision_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_defer_replacement_placements \
         WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn assert_publication_failure_rollbacks(
    pool: &PgPool,
    scope: DatabaseScope,
    items: &ItemService,
    schedules: &PostgresSchedulingRepository,
    access: &ScheduleAccess,
) {
    let current_before: Uuid = sqlx::query_scalar(
        "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    for (index, table) in [
        "schedule_blocks",
        "schedule_revision_details",
        "schedule_publication_requests",
        "audit_operations",
    ]
    .into_iter()
    .enumerate()
    {
        let trigger_sql = format!(
            "CREATE FUNCTION fail_test_publication() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'synthetic publication failure'; END $$; \
             CREATE TRIGGER fail_test_publication BEFORE INSERT ON {table} \
             FOR EACH ROW EXECUTE FUNCTION fail_test_publication();"
        );
        pool.execute(AssertSqlSafe(trigger_sql)).await.unwrap();
        let before = publication_counts(pool, scope).await;
        let mut request = compose_request();
        request.fixed_blocks[0].title = format!("Failure injection target {index}");
        request.fixed_blocks[0].start += chrono::Duration::minutes(i64::try_from(index).unwrap());
        request.fixed_blocks[0].end += chrono::Duration::minutes(i64::try_from(index).unwrap());
        let preview = compose_canonical_schedule(items, schedules, request)
            .await
            .unwrap();
        let result = schedules
            .publish(
                access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [110 + u8::try_from(index).unwrap(); 32],
                    input_digest: digest_bytes(&preview.input_digest),
                    timezone_name: "Europe/Madrid".to_owned(),
                    manual_placement_approvals: Vec::new(),
                    result: preview,
                    published_at: Utc::now(),
                },
            )
            .await;
        assert!(matches!(result, Err(SchedulePublicationError::Unavailable)));
        assert_eq!(publication_counts(pool, scope).await, before);
        pool.execute(AssertSqlSafe(format!(
            "DROP TRIGGER fail_test_publication ON {table}; DROP FUNCTION fail_test_publication();"
        )))
        .await
        .unwrap();
        let current_after: Uuid = sqlx::query_scalar(
            "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
        )
        .bind(scope.workspace_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(current_after, current_before);
    }
}

#[allow(clippy::too_many_lines)] // Keeps both sides and outcomes of the database race in one regression.
async fn assert_content_insert_seal_race(pool: &PgPool, scope: DatabaseScope) {
    let current_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM schedule_revisions WHERE workspace_id = $1 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let next_revision: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_number), 0) + 1 FROM schedule_revisions WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let draft_id = Uuid::new_v4();
    let start = Utc::now();
    let end = start + chrono::Duration::hours(1);
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, parent_revision_id, \
         state, horizon_start, horizon_end, timezone_name, input_digest, created_by_user_id) \
         VALUES ($1, $2, $3, $4, 'draft', $5, $6, 'Europe/Madrid', $7, $8)",
    )
    .bind(draft_id)
    .bind(scope.workspace_id)
    .bind(next_revision)
    .bind(current_id)
    .bind(start)
    .bind(end)
    .bind(vec![121_u8; 32])
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
         result_snapshot) VALUES ($1, $2, $3, '{}'::jsonb)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(draft_id)
    .execute(pool)
    .await
    .unwrap();

    let source_block_id = Uuid::new_v4();
    let mut content = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, schedule_revision_id, \
         block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal) \
         VALUES ($1, $2, $3, $4, 'planned', 'race-before-seal', $5, $6, 'Europe/Madrid', 0)",
    )
    .bind(Uuid::new_v4())
    .bind(source_block_id)
    .bind(scope.workspace_id)
    .bind(draft_id)
    .bind(start)
    .bind(end)
    .execute(&mut *content)
    .await
    .unwrap();

    let seal_pool = pool.clone();
    let seal = tokio::spawn(async move {
        let mut transaction = seal_pool.begin().await.unwrap();
        let published_at = Utc::now();
        sqlx::query(
            "UPDATE schedule_revisions SET state = 'superseded', superseded_at = $3 \
             WHERE workspace_id = $1 AND id = $2 AND state = 'published'",
        )
        .bind(scope.workspace_id)
        .bind(current_id)
        .bind(published_at)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
             WHERE workspace_id = $1 AND id = $2 AND state = 'draft'",
        )
        .bind(scope.workspace_id)
        .bind(draft_id)
        .bind(published_at)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    });
    let seal_error = seal
        .await
        .expect("raw seal task completes")
        .expect_err("raw seal fails fast behind the execution-state lock");
    assert_eq!(postgres_error_code(&seal_error).as_deref(), Some("55P03"));
    content.commit().await.unwrap();

    let mut retry = pool.begin().await.unwrap();
    let published_at = Utc::now();
    sqlx::query(
        "UPDATE schedule_revisions SET state = 'superseded', superseded_at = $3 \
         WHERE workspace_id = $1 AND id = $2 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .bind(current_id)
    .bind(published_at)
    .execute(&mut *retry)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
         WHERE workspace_id = $1 AND id = $2 AND state = 'draft'",
    )
    .bind(scope.workspace_id)
    .bind(draft_id)
    .bind(published_at)
    .execute(&mut *retry)
    .await
    .unwrap();
    retry.commit().await.unwrap();

    let included: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_blocks WHERE workspace_id = $1 \
         AND schedule_revision_id = $2 AND source_block_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(draft_id)
    .bind(source_block_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(included, 1);
    assert!(
        sqlx::query(
            "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, schedule_revision_id, \
             block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal) \
             VALUES ($1, $2, $3, $4, 'planned', 'race-after-seal', $5, $6, 'Europe/Madrid', 1)",
        )
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .bind(scope.workspace_id)
        .bind(draft_id)
        .bind(start)
        .bind(end)
        .execute(pool)
        .await
        .is_err()
    );
}

fn digest_bytes(value: &str) -> [u8; 32] {
    let hex = value.strip_prefix("sha256:").unwrap().as_bytes();
    let mut output = [0_u8; 32];
    for (index, pair) in hex.chunks_exact(2).enumerate() {
        output[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    output
}

fn nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => panic!("non-hex digest"),
    }
}

async fn credential_publish_app(
    pool: &PgPool,
    scope: DatabaseScope,
    items: Arc<ItemService>,
    schedules: Arc<PostgresSchedulingRepository>,
) -> (Router, String) {
    let repository = Arc::new(PostgresCredentialRepository::new(pool.clone(), scope));
    let now = Utc::now();
    let enrollment = GeneratedCredential::generate(CredentialKind::Enrollment)
        .expect("generate enrollment credential");
    repository
        .create_device_enrollment(
            DeviceEnrollmentSpec {
                id: Uuid::new_v4(),
                client_instance_id: Uuid::new_v4(),
                client_kind: DeviceClientKind::Macos,
                device_label: "Schedule publication integration".to_owned(),
                scopes: vec![
                    Scope::ScheduleRead,
                    Scope::ScheduleSimulate,
                    Scope::SchedulePublish,
                ],
                client_contract_version: DEVICE_CLIENT_CONTRACT_VERSION,
                client_version: "schedule-publication-integration-1".to_owned(),
                client_capabilities: vec!["schedule-publication-journal-v1".to_owned()],
                created_at: now,
            },
            &enrollment.parsed().expect("parse enrollment credential"),
        )
        .await
        .expect("create v2 device enrollment");
    let access = GeneratedCredential::generate(CredentialKind::DeviceAccess)
        .expect("generate device access credential");
    let refresh = GeneratedCredential::generate(CredentialKind::DeviceRefresh)
        .expect("generate device refresh credential");
    repository
        .consume_device_enrollment(
            &enrollment.parsed().expect("parse enrollment credential"),
            Uuid::new_v4(),
            &access.parsed().expect("parse access credential"),
            &refresh.parsed().expect("parse refresh credential"),
            now,
        )
        .await
        .expect("consume v2 device enrollment");
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
        AppState::new(proposals, authenticator, readiness)
            .with_items(items)
            .with_postgres_scheduling(schedules, Arc::new(Vec::new()))
            .with_credential_auth(
                credential_repository,
                Arc::new(RuntimeAuthenticator::new(
                    None,
                    Arc::new(PostgresCredentialRepository::new(pool.clone(), scope)),
                    Arc::new(SystemClock),
                )),
                AuthMode::CredentialOnly,
            ),
    );
    (app, access_token)
}

fn authenticated_get(uri: &str, access_token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Body::empty())
        .unwrap()
}

fn schedule_stream_request(access_token: &str, cursor: Option<&str>) -> Request<Body> {
    let mut request = authenticated_get("/v1/schedule/stream", access_token);
    request.headers_mut().insert(
        header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );
    if let Some(cursor) = cursor {
        request.headers_mut().insert(
            "last-event-id",
            HeaderValue::from_str(cursor).expect("valid test cursor header"),
        );
    }
    request
}

async fn next_schedule_stream_chunk(
    response: &mut axum::response::Response,
    wait: StdDuration,
) -> Option<String> {
    let frame = timeout(wait, response.body_mut().frame())
        .await
        .expect("schedule stream produced or ended before timeout")?;
    let frame = frame.expect("valid schedule stream frame");
    let data = frame.into_data().expect("schedule stream data frame");
    Some(String::from_utf8(data.to_vec()).expect("UTF-8 schedule stream frame"))
}

fn assert_schedule_revision_frame(frame: &str, revision: u64) {
    assert_eq!(
        frame,
        format!(
            "id: {revision}\nevent: schedule-invalidation\ndata: {{\"revision\":{revision}}}\n\n"
        )
    );
    for forbidden in ["block", "item", "title", "sensitive"] {
        assert!(
            !frame.contains(forbidden),
            "schedule SSE leaked {forbidden}"
        );
    }
}

fn json_request(uri: &str, body: &Value, access_token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn seed_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'Schedule owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(format!("auth0|schedule-owner-{}", scope.user_id.simple()))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'Schedule workspace', 'Europe/Madrid')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("schedule-{}", scope.workspace_id.simple()))
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

#[allow(clippy::too_many_lines)] // Audits every sealed-content mutation and allowed draft transition together.
async fn assert_schedule_seal(pool: &PgPool, scope: DatabaseScope, published_id: Uuid) {
    let existing_block: Uuid = sqlx::query_scalar(
        "SELECT id FROM schedule_blocks WHERE workspace_id = $1 AND schedule_revision_id = $2 \
         ORDER BY ordinal LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(published_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let inserted_block = sqlx::query(
        "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, schedule_revision_id, \
         item_id, block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal, \
         is_fixed, is_sensitive, constraint_snapshot) SELECT $3, $4, workspace_id, \
         schedule_revision_id, item_id, block_kind, title_snapshot, starts_at, ends_at, \
         timezone_name, ordinal + 100, is_fixed, is_sensitive, constraint_snapshot \
         FROM schedule_blocks WHERE workspace_id = $1 AND schedule_revision_id = $2 LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(published_id)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(pool)
    .await;
    assert!(inserted_block.is_err());
    assert!(
        sqlx::query("UPDATE schedule_blocks SET ordinal = ordinal + 1 WHERE id = $1")
            .bind(existing_block)
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM schedule_blocks WHERE id = $1")
            .bind(existing_block)
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
             result_snapshot) SELECT workspace_id, user_id, schedule_revision_id, result_snapshot \
             FROM schedule_revision_details WHERE workspace_id = $1 AND schedule_revision_id = $2",
        )
        .bind(scope.workspace_id)
        .bind(published_id)
        .execute(pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE schedule_revision_details SET result_snapshot = '{}'::jsonb \
             WHERE workspace_id = $1 AND schedule_revision_id = $2",
        )
        .bind(scope.workspace_id)
        .bind(published_id)
        .execute(pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "DELETE FROM schedule_revision_details WHERE workspace_id = $1 \
             AND schedule_revision_id = $2",
        )
        .bind(scope.workspace_id)
        .bind(published_id)
        .execute(pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE schedule_publication_requests SET request_hash = $3 \
             WHERE workspace_id = $1 AND schedule_revision_id = $2",
        )
        .bind(scope.workspace_id)
        .bind(published_id)
        .bind(vec![77_u8; 32])
        .execute(pool)
        .await
        .is_err()
    );

    let trigger_scope = seed_scope(pool).await;
    let forbidden = sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, horizon_start, \
         horizon_end, timezone_name, input_digest, created_by_user_id, published_at) \
         VALUES ($1, $2, 1, 'published', $3, $4, 'Europe/Madrid', $5, $6, $3)",
    )
    .bind(Uuid::new_v4())
    .bind(trigger_scope.workspace_id)
    .bind(Utc::now())
    .bind(Utc::now() + chrono::Duration::hours(1))
    .bind(vec![1_u8; 32])
    .bind(trigger_scope.user_id)
    .execute(pool)
    .await
    .expect_err("direct published insertion must hit the draft-only trigger");
    assert!(
        forbidden
            .to_string()
            .contains("schedule revisions must be inserted as drafts")
    );

    let draft_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, horizon_start, \
         horizon_end, timezone_name, input_digest, created_by_user_id) \
         VALUES ($1, $2, 2, 'draft', $3, $4, 'Europe/Madrid', $5, $6)",
    )
    .bind(draft_id)
    .bind(scope.workspace_id)
    .bind(Utc::now())
    .bind(Utc::now() + chrono::Duration::hours(1))
    .bind(vec![2_u8; 32])
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
    let draft_block = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, schedule_revision_id, \
         block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal) \
         VALUES ($1, $2, $3, $4, 'planned', 'draft', $5, $6, 'Europe/Madrid', 0)",
    )
    .bind(draft_block)
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(draft_id)
    .bind(Utc::now())
    .bind(Utc::now() + chrono::Duration::minutes(30))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE schedule_blocks SET title_snapshot = 'edited draft' WHERE id = $1")
        .bind(draft_block)
        .execute(pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE schedule_blocks SET schedule_revision_id = $2 WHERE id = $1")
            .bind(draft_block)
            .bind(published_id)
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE schedule_blocks SET schedule_revision_id = $2 WHERE id = $1")
            .bind(existing_block)
            .bind(draft_id)
            .execute(pool)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM schedule_blocks WHERE id = $1")
        .bind(draft_block)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
         result_snapshot) VALUES ($1, $2, $3, '{}'::jsonb)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(draft_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE schedule_revision_details SET result_snapshot = '{\"draft\":true}'::jsonb \
         WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(draft_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM schedule_revision_details WHERE workspace_id = $1 AND schedule_revision_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(draft_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE schedule_revisions SET state = 'discarded' WHERE id = $1")
        .bind(draft_id)
        .execute(pool)
        .await
        .unwrap();

    let detail_less = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, horizon_start, \
         horizon_end, timezone_name, input_digest, created_by_user_id) \
         VALUES ($1, $2, 3, 'draft', $3, $4, 'Europe/Madrid', $5, $6)",
    )
    .bind(detail_less)
    .bind(scope.workspace_id)
    .bind(Utc::now())
    .bind(Utc::now() + chrono::Duration::hours(1))
    .bind(vec![3_u8; 32])
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
    assert!(
        sqlx::query(
            "UPDATE schedule_revisions SET state = 'published', published_at = clock_timestamp() \
             WHERE id = $1",
        )
        .bind(detail_less)
        .execute(pool)
        .await
        .is_err()
    );
    sqlx::query("UPDATE schedule_revisions SET state = 'discarded' WHERE id = $1")
        .bind(detail_less)
        .execute(pool)
        .await
        .unwrap();
}

#[allow(clippy::too_many_lines)] // Direct SQL adversaries cover every live and terminal audience/version branch.
async fn assert_device_contract_scope_coupling(pool: &PgPool, scope: DatabaseScope) {
    let now = Utc::now();
    let v1_enrollment = sqlx::query(
        "INSERT INTO device_enrollments (id, workspace_id, user_id, client_instance_id, \
         client_kind, device_label, token_hash, scopes, created_at, expires_at, \
         client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'macos', 'Forbidden v1 publisher', $5, \
         ARRAY['schedule_publish']::text[], $6, $7, 1, 'legacy-device')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(Uuid::new_v4())
    .bind(vec![11_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::minutes(5))
    .execute(pool)
    .await;
    assert!(v1_enrollment.is_err());

    let consumed_v1_enrollment = sqlx::query(
        "INSERT INTO device_enrollments (id, workspace_id, user_id, client_instance_id, \
         client_kind, device_label, token_hash, scopes, created_at, expires_at, consumed_at, \
         client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'android', 'Consumed v1 publisher', $5, \
         ARRAY['schedule_publish']::text[], $6, $7, $6, 1, 'legacy-device')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(Uuid::new_v4())
    .bind(vec![15_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::minutes(5))
    .execute(pool)
    .await;
    assert!(consumed_v1_enrollment.is_err());

    let v1_session = sqlx::query(
        "INSERT INTO sessions (id, workspace_id, user_id, token_hash, client_kind, device_label, \
         metadata, created_at, last_seen_at, expires_at, auth_version, client_instance_id, \
         refresh_token_hash, scopes, refresh_idle_expires_at, absolute_expires_at, \
         credential_issued_at, client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'macos', 'Forbidden v1 session', '{}'::jsonb, $5, $5, $6, \
         1, $7, $8, ARRAY['schedule_publish']::text[], $9, $10, $5, 1, 'legacy-device')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![12_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::minutes(15))
    .bind(Uuid::new_v4())
    .bind(vec![13_u8; 32])
    .bind(now + chrono::Duration::days(30))
    .bind(now + chrono::Duration::days(180))
    .execute(pool)
    .await;
    assert!(v1_session.is_err());

    let revoked_v1_session = sqlx::query(
        "INSERT INTO sessions (id, workspace_id, user_id, token_hash, client_kind, device_label, \
         metadata, created_at, last_seen_at, expires_at, revoked_at, auth_version, \
         client_instance_id, refresh_token_hash, scopes, refresh_idle_expires_at, \
         absolute_expires_at, credential_issued_at, client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'macos', 'Revoked v1 publisher', '{}'::jsonb, $5, $5, $6, $5, \
         1, $7, $8, ARRAY['schedule_publish']::text[], $9, $10, $5, 1, 'legacy-device')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![16_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::minutes(15))
    .bind(Uuid::new_v4())
    .bind(vec![17_u8; 32])
    .bind(now + chrono::Duration::days(30))
    .bind(now + chrono::Duration::days(180))
    .execute(pool)
    .await;
    assert!(revoked_v1_session.is_err());

    // Existing contract-v1 device credentials retain their exact historical
    // scope set. They remain readable, but the database cannot widen them to
    // the v2-only publication scope.
    let compatible_v1_session = sqlx::query(
        "INSERT INTO sessions (id, workspace_id, user_id, token_hash, client_kind, device_label, \
         metadata, created_at, last_seen_at, expires_at, auth_version, client_instance_id, \
         refresh_token_hash, scopes, refresh_idle_expires_at, absolute_expires_at, \
         credential_issued_at, client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'android', 'Compatible v1 reader', '{}'::jsonb, $5, $5, $6, \
         1, $7, $8, ARRAY['schedule_read']::text[], $9, $10, $5, 1, 'existing-device')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![21_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::minutes(15))
    .bind(Uuid::new_v4())
    .bind(vec![22_u8; 32])
    .bind(now + chrono::Duration::days(30))
    .bind(now + chrono::Duration::days(180))
    .execute(pool)
    .await;
    assert!(compatible_v1_session.is_ok());
    let widened_v1_session = sqlx::query(
        "UPDATE sessions SET scopes = ARRAY['schedule_read', 'schedule_publish']::text[] \
         WHERE workspace_id = $1 AND token_hash = $2",
    )
    .bind(scope.workspace_id)
    .bind(vec![21_u8; 32])
    .execute(pool)
    .await;
    assert!(widened_v1_session.is_err());

    // Native MCP remains contract v1 and its audience constraint remains
    // unchanged; schedule_publish is never part of its scope vocabulary.
    let mcp_v1 = sqlx::query(
        "INSERT INTO mcp_clients (id, workspace_id, created_by_user_id, client_identifier, \
         display_name, scopes, allowed_origins, status, credential_hash, revision, created_at, \
         updated_at, expires_at, auth_version, client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'Existing MCP v1', ARRAY['schedule_read']::text[], \
         ARRAY[]::text[], 'active', $5, 1, $6, $6, $7, 1, 1, 'existing-mcp')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("existing-mcp-{}", Uuid::new_v4()))
    .bind(vec![14_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::days(30))
    .execute(pool)
    .await;
    assert!(mcp_v1.is_ok());

    let revoked_mcp_with_publish = sqlx::query(
        "INSERT INTO mcp_clients (id, workspace_id, created_by_user_id, client_identifier, \
         display_name, scopes, allowed_origins, status, credential_hash, revision, created_at, \
         updated_at, expires_at, revoked_at, auth_version, client_contract_version, client_version) \
         VALUES ($1, $2, $3, $4, 'Forbidden MCP publisher', ARRAY['schedule_publish']::text[], \
         ARRAY[]::text[], 'revoked', $5, 1, $6, $6, $7, $6, 1, 1, 'existing-mcp')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("forbidden-mcp-{}", Uuid::new_v4()))
    .bind(vec![18_u8; 32])
    .bind(now)
    .bind(now + chrono::Duration::days(30))
    .execute(pool)
    .await;
    assert!(revoked_mcp_with_publish.is_err());
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
        let schema = format!("dayweave_schedule_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated test schema");
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
