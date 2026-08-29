use std::{str::FromStr, sync::Arc, time::Duration as StdDuration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use dayweave_api::{
    AppState,
    auth::{Authenticator, RuntimeAuthenticator, Scope},
    config::AuthMode,
    credential_auth::{
        CredentialKind, CredentialRepository, DEVICE_CLIENT_CONTRACT_VERSION, DeviceClientKind,
        DeviceEnrollmentSpec, GeneratedCredential,
    },
    http::router,
    items::{IdempotencyKey, ItemKind, ItemService, ItemStatus, NewItem, ReplaceItem, SplitPolicy},
    persistence::{
        DatabaseScope, MIGRATOR, PostgresCredentialRepository, PostgresItemRepository,
        PostgresProposalRepository,
    },
    proposals::{
        InMemoryProposalRepository, NewProposal, Proposal, ProposalKind, ProposalRepository,
        ProposalService, ProposalSource, SystemClock,
    },
    readiness::Readiness,
    scheduling::{
        ComposeScheduleRequest, ConflictQuery, ItemSearchQuery, PlanOperation, PlanOperationKind,
        PlanningSimulationPort, PostgresSchedulingRepository, ProposalSubmissionError,
        ProposalSubmissionPort, ProposalSubmissionSpec, PublishScheduleSpec, ScheduleAccess,
        ScheduleDetail, SchedulePublicationError, ScheduleQuery, ScheduleQueryPort,
        SchedulingPortError, SimulationRequest, compose_canonical_schedule,
        simulation_request_digest,
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

    let preview = compose_canonical_schedule(&items, request.clone())
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

    // A fresh key for identical exact content binds to the current revision
    // without revision churn.
    let fresh_key = Uuid::new_v4();
    let deduplicated = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: fresh_key,
                request_hash,
                input_digest,
                timezone_name: "Europe/Madrid".to_owned(),
                result: preview.clone(),
                published_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert!(!deduplicated.replayed);
    assert_eq!(deduplicated.revision.revision, first_revision);
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
                operations: vec![operation(PlanOperationKind::CreateItem, None, json!({}))],
                assumptions: Vec::new(),
            },
        )
        .await
        .unwrap();

    let next_request = compose_request();
    let next_preview = compose_canonical_schedule(&items, next_request)
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
    let exact_key = Uuid::new_v4();
    let exact_spec = PublishScheduleSpec {
        idempotency_key: exact_key,
        request_hash: [91; 32],
        input_digest: digest_bytes(&next_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        result: next_preview.clone(),
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

    let conflicting_key = Uuid::new_v4();
    let conflict_left = PublishScheduleSpec {
        idempotency_key: conflicting_key,
        request_hash: [92; 32],
        input_digest: digest_bytes(&next_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        result: next_preview.clone(),
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

    let same_content_left = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [94; 32],
        input_digest: digest_bytes(&next_preview.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        result: next_preview.clone(),
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
    let same_content_left = same_content_left.unwrap();
    let same_content_right = same_content_right.unwrap();
    assert!(!same_content_left.replayed && !same_content_right.replayed);
    assert_eq!(
        same_content_left.revision.id,
        same_content_right.revision.id
    );

    let different_left_result = compose_canonical_schedule(&items, compose_request())
        .await
        .unwrap();
    let mut different_right_request = compose_request();
    different_right_request.fixed_blocks[0].title =
        "Explicitly different concurrent publication".to_owned();
    let different_right_result = compose_canonical_schedule(&items, different_right_request)
        .await
        .unwrap();
    let different_left = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [96; 32],
        input_digest: digest_bytes(&different_left_result.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
        result: different_left_result,
        published_at: "2099-01-01T00:00:00Z".parse().unwrap(),
    };
    let different_right = PublishScheduleSpec {
        idempotency_key: Uuid::new_v4(),
        request_hash: [97; 32],
        input_digest: digest_bytes(&different_right_result.input_digest),
        timezone_name: "Europe/Madrid".to_owned(),
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
    let different_left = different_left.unwrap();
    let different_right = different_right.unwrap();
    assert_ne!(different_left.revision.id, different_right.revision.id);

    // Caller clocks can be inverted relative to serialization order. The
    // transaction captures/clamps publication time after its locks, so even a
    // deterministic future-then-past caller sequence remains monotonic.
    let mut future_caller_request = compose_request();
    future_caller_request.fixed_blocks[0].title = "Future caller clock".to_owned();
    let future_caller_result = compose_canonical_schedule(&items, future_caller_request)
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
                result: future_caller_result,
                published_at: "2099-01-01T00:00:00Z".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    let mut past_caller_request = compose_request();
    past_caller_request.fixed_blocks[0].title = "Past caller clock".to_owned();
    let past_caller_result = compose_canonical_schedule(&items, past_caller_request)
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
        8
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
    let stored_privacy_evidence: bool = sqlx::query_scalar(
        "SELECT result_snapshot ? 'privacy_evidence' FROM schedule_simulations \
         WHERE workspace_id = $1 AND user_id = $2 AND consumed_at IS NULL \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert!(stored_privacy_evidence);
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
        Some(simulated.simulation_token.clone()),
    );
    first_submission.proposal.created_at = "2099-01-01T00:00:00.123456789Z".parse().unwrap();
    first_submission.proposal.updated_at = first_submission.proposal.created_at;
    first_submission.proposal.expires_at = "2099-01-02T00:00:00.987654321Z".parse().unwrap();
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
    let after_restart = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let replayed = after_restart
        .submit_proposal(
            &access,
            proposal_submission(
                &access.subject,
                submission_key,
                [81; 32],
                &simulation_request,
                Some(simulated.simulation_token.clone()),
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
                    Some(simulated.simulation_token.clone()),
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

    let concurrent_simulation = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let concurrent_spec = proposal_submission(
        &access.subject,
        "durable-proposal-race",
        [83; 32],
        &simulation_request,
        Some(concurrent_simulation.simulation_token.clone()),
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
                    Some(concurrent_simulation.simulation_token),
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
    let mut before_insert_spec = proposal_submission(
        &access.subject,
        "durable-proposal-before-insert",
        [85; 32],
        &simulation_request,
        Some(before_insert_simulation.simulation_token.clone()),
    );
    let proposal_repository = PostgresProposalRepository::new(test_database.pool.clone(), scope);
    proposal_repository
        .insert(before_insert_spec.proposal.clone())
        .await
        .unwrap();
    assert!(matches!(
        restarted
            .submit_proposal(&access, before_insert_spec.clone())
            .await,
        Err(ProposalSubmissionError::Unavailable)
    ));
    assert_simulation_unconsumed(
        &test_database.pool,
        scope,
        &before_insert_simulation.simulation_token,
    )
    .await;
    before_insert_spec.proposal.id = Uuid::new_v4();
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
        Some(after_insert_simulation.simulation_token.clone()),
    );
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
    let rolled_back_evidence: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM proposals WHERE id = $1), \
           (SELECT COUNT(*) FROM outbox_messages WHERE aggregate_id = $1), \
           (SELECT COUNT(*) FROM audit_operations WHERE entity_id = $1), \
           (SELECT COUNT(*) FROM mcp_proposal_submissions WHERE proposal_id = $1)",
    )
    .bind(after_insert_spec.proposal.id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(rolled_back_evidence, (0, 0, 0, 0));
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
                    Some(subject_simulation.simulation_token.clone()),
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

    let expired_simulation = restarted
        .simulate(&access, simulation_request.clone())
        .await
        .unwrap();
    let mut expired_hash = Sha256::new();
    expired_hash.update(b"dayweave.schedule-simulation-token.v1\0");
    expired_hash.update(expired_simulation.simulation_token.as_bytes());
    sqlx::query(
        "UPDATE schedule_simulations SET created_at = clock_timestamp() - interval '16 minutes', \
         expires_at = clock_timestamp() - interval '2 minutes' \
         WHERE workspace_id = $1 AND user_id = $2 AND token_hash = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(expired_hash.finalize().as_slice())
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
                    Some(expired_simulation.simulation_token),
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
    let race_preview = compose_canonical_schedule(&items, compose_request())
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

    let schedules = PostgresSchedulingRepository::new(test_database.pool.clone(), scope);
    let access = owner_access(scope, "auth0|legacy-upgrade-owner");
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
    let fresh_preview = compose_canonical_schedule(&items, fresh_request)
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
                    operations: vec![operation(PlanOperationKind::CreateItem, None, json!({}))],
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

fn proposal_submission(
    subject: &str,
    idempotency_key: &str,
    request_fingerprint: [u8; 32],
    request: &SimulationRequest,
    simulation_token: Option<String>,
) -> ProposalSubmissionSpec {
    let expected_simulation_digest = simulation_request_digest(request).unwrap();
    let proposal = Proposal::new(
        NewProposal {
            submitted_by: subject.to_owned(),
            source: ProposalSource::ExternalMcp,
            source_reference: Some("synthetic conversation".to_owned()),
            kind: ProposalKind::SchedulePlan,
            title: "Synthetic durable MCP proposal".to_owned(),
            explanation: Some("Exercises transactional submission.".to_owned()),
            payload: json!({
                "schema_version": 1,
                "base_revision": request.base_revision,
                "assumptions": request.assumptions,
                "operations": request.operations,
                "source": {"client": "test", "conversation": "synthetic"},
                "safety": {
                    "proposal_only": true,
                    "requires_app_review": true,
                    "canonical_state_mutated": false
                }
            }),
            expires_at: Utc::now() + chrono::Duration::days(1),
        },
        Utc::now(),
    )
    .unwrap();
    ProposalSubmissionSpec {
        idempotency_key: idempotency_key.to_owned(),
        request_fingerprint,
        expected_simulation_digest,
        simulation_token,
        proposal,
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

    let spec = proposal_submission(
        &access.subject,
        submission_key,
        fingerprint,
        request,
        Some(simulation.simulation_token.clone()),
    );
    let proposal_id = spec.proposal.id;
    assert!(matches!(
        schedules.submit_proposal(access, spec).await,
        Err(ProposalSubmissionError::Simulation(
            SchedulingPortError::NotFound
        ))
    ));
    assert_simulation_unconsumed(pool, scope, &simulation.simulation_token).await;
    let evidence: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM proposals WHERE workspace_id = $1 AND id = $2), \
           (SELECT COUNT(*) FROM outbox_messages WHERE workspace_id = $1 AND aggregate_id = $2), \
           (SELECT COUNT(*) FROM audit_operations WHERE workspace_id = $1 AND entity_id = $2), \
           (SELECT COUNT(*) FROM mcp_proposal_submissions WHERE workspace_id = $1 \
             AND proposal_id = $2)",
    )
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(evidence, (0, 0, 0, 0));
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
        let preview = compose_canonical_schedule(items, request).await.unwrap();
        let result = schedules
            .publish(
                access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [110 + u8::try_from(index).unwrap(); 32],
                    input_digest: digest_bytes(&preview.input_digest),
                    timezone_name: "Europe/Madrid".to_owned(),
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
    let content_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *content)
        .await
        .unwrap();
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
        .await
        .unwrap();
        sqlx::query(
            "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
             WHERE workspace_id = $1 AND id = $2 AND state = 'draft'",
        )
        .bind(scope.workspace_id)
        .bind(draft_id)
        .bind(published_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    });
    wait_for_blocked_queries(pool, content_pid, 1).await;
    content.commit().await.unwrap();
    seal.await.unwrap();

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
