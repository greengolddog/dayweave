use std::{
    str::FromStr,
    sync::{Arc, OnceLock},
    time::Duration,
};

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    items::{
        DeltaChange, IdempotencyKey, ItemInvalidationConfig, ItemKind, ItemQuery, ItemRepository,
        ItemRepositoryError, ItemService, ItemServiceError, ItemStatus, NewItem, ReplaceItem,
        SplitPolicy,
    },
    persistence::{DatabaseScope, MIGRATOR, PostgresItemRepository},
    proposals::{InMemoryProposalRepository, ProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use tower::ServiceExt as _;
use uuid::Uuid;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn postgres_items_are_atomic_isolated_hierarchical_and_delta_synced() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL item test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");

    let scope = seed_scope(pool, "item-owner-one", "item-workspace-one").await;
    let repository = Arc::new(PostgresItemRepository::new(pool.clone(), scope));
    let service = ItemService::new(repository.clone(), Arc::new(SystemClock));
    let other_scope = seed_scope(pool, "item-owner-two", "item-workspace-two").await;
    let other_repository = Arc::new(PostgresItemRepository::new(pool.clone(), other_scope));
    let other_service = ItemService::new(other_repository.clone(), Arc::new(SystemClock));

    let root_id = Uuid::new_v4();
    let later_sibling_id = Uuid::new_v4();
    let earlier_sibling_id = Uuid::new_v4();
    let root = service
        .create(
            new_item(root_id, "Root goal", ItemKind::Goal, None, 0),
            idempotency("postgres-root-001", 1),
        )
        .await
        .unwrap();
    assert_eq!(root.item.revision, 1);
    let replay = service
        .create(
            new_item(root_id, "Root goal", ItemKind::Goal, None, 0),
            idempotency("postgres-root-001", 1),
        )
        .await
        .unwrap();
    assert!(replay.replayed);

    service
        .create(
            new_item(
                later_sibling_id,
                "Second sibling",
                ItemKind::Routine,
                Some(root_id),
                2,
            ),
            idempotency("postgres-child-a-001", 2),
        )
        .await
        .unwrap();
    let child_b = service
        .create(
            new_item(
                earlier_sibling_id,
                "First sibling",
                ItemKind::Task,
                Some(root_id),
                1,
            ),
            idempotency("postgres-child-b-001", 3),
        )
        .await
        .unwrap()
        .item;

    assert!(!service.get(root_id).await.unwrap().is_executable);
    let siblings = service
        .list(ItemQuery {
            parent_id: Some(root_id),
            include_deleted: false,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        siblings.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![earlier_sibling_id, later_sibling_id]
    );

    let cycle = service
        .replace(
            root_id,
            3,
            replacement(&root.item, Some(earlier_sibling_id), ItemStatus::Planned),
            idempotency("postgres-cycle-001", 4),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        cycle,
        ItemServiceError::Repository(ItemRepositoryError::HierarchyCycle)
    ));
    assert_eq!(service.get(root_id).await.unwrap().parent_id, None);

    let stale = service
        .replace(
            earlier_sibling_id,
            99,
            replacement(&child_b, Some(root_id), ItemStatus::Planned),
            idempotency("postgres-stale-001", 5),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        ItemServiceError::Repository(ItemRepositoryError::RevisionConflict {
            expected: 99,
            actual: 1
        })
    ));

    let missing_parent_key = "postgres-missing-parent-001";
    let missing_parent = service
        .create(
            new_item(
                Uuid::new_v4(),
                "Must roll back",
                ItemKind::Task,
                Some(Uuid::new_v4()),
                0,
            ),
            idempotency(missing_parent_key, 6),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        missing_parent,
        ItemServiceError::Repository(ItemRepositoryError::ParentNotFound(_))
    ));
    let missing_key_hash: [u8; 32] = Sha256::digest(missing_parent_key.as_bytes()).into();
    let leaked_reservation: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1 AND key_hash = $2",
    )
    .bind(scope.workspace_id)
    .bind(missing_key_hash.as_slice())
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(leaked_reservation, 0);

    let deleted = service
        .trash(earlier_sibling_id, 1, idempotency("postgres-delete-001", 7))
        .await
        .unwrap();
    assert_eq!(deleted.item.revision, 2);
    assert!(deleted.item.deleted_at.is_some());
    let delta = service.delta(None, 20).await.unwrap();
    assert_eq!(delta.changes.len(), 7);
    assert!(
        delta
            .changes
            .iter()
            .any(|change| matches!(change, DeltaChange::Tombstone { .. }))
    );
    let tail = service.delta(Some(&delta.next_cursor), 20).await.unwrap();
    assert!(tail.changes.is_empty());

    let restored = service
        .restore(
            earlier_sibling_id,
            2,
            idempotency("postgres-restore-001", 8),
        )
        .await
        .unwrap();
    assert_eq!(restored.item.revision, 3);
    let restore_delta = service.delta(Some(&delta.next_cursor), 20).await.unwrap();
    assert_eq!(restore_delta.changes.len(), 2);
    assert!(
        restore_delta
            .changes
            .iter()
            .all(|change| matches!(change, DeltaChange::Upsert { .. }))
    );

    assert!(matches!(
        other_service.get(root_id).await.unwrap_err(),
        ItemServiceError::Repository(ItemRepositoryError::NotFound(id)) if id == root_id
    ));
    assert!(
        other_service
            .delta(None, 20)
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    assert!(matches!(
        other_service
            .delta(Some(&delta.next_cursor), 20)
            .await
            .unwrap_err(),
        ItemServiceError::InvalidCursor
    ));

    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_operations WHERE workspace_id = $1 AND entity_type = 'item'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND aggregate_type = 'item'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let change_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM item_changes WHERE workspace_id = $1")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(audit_count, 9);
    assert_eq!(outbox_count, audit_count);
    assert_eq!(change_count, audit_count);

    test_database.destroy().await;
}

#[tokio::test]
async fn postgres_item_stream_probes_shared_durable_head_without_leaking_content() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL item stream test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");

    let scope = seed_scope(pool, "item-stream-owner", "item-stream-workspace").await;
    let repository = Arc::new(PostgresItemRepository::new(pool.clone(), scope));
    assert_eq!(repository.delta_head().await.unwrap(), 0);

    let proposals: Arc<dyn ProposalRepository> = Arc::new(InMemoryProposalRepository::default());
    let proposals = Arc::new(ProposalService::new(
        proposals,
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    let token = "postgres-item-stream-token";
    let mut state = AppState::new(
        proposals,
        Arc::new(StaticTokenAuthenticator::from_plaintext(&[token])),
        Readiness::default(),
    );
    state.items = Arc::new(
        ItemService::new(repository.clone(), Arc::new(SystemClock)).with_invalidation_config(
            ItemInvalidationConfig::new(
                Duration::from_millis(10),
                Duration::from_millis(200),
                Duration::from_secs(1),
                2,
            )
            .unwrap(),
        ),
    );
    let app = router(state);

    let initial_cursor = postgres_delta_cursor(&app, token).await;
    let mut stream = app
        .clone()
        .oneshot(postgres_stream_request(token, &initial_cursor))
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);

    // A distinct service has its own process-local hub but commits to the same
    // PostgreSQL change log, modeling another API process or integration writer.
    let external = ItemService::new(repository.clone(), Arc::new(SystemClock));
    let item_id = Uuid::new_v4();
    external
        .create(
            new_item(
                item_id,
                "SYNTHETIC-POSTGRES-PRIVATE-ITEM-STREAM-TITLE",
                ItemKind::Task,
                None,
                0,
            ),
            idempotency("postgres-item-stream-create-001", 44),
        )
        .await
        .unwrap();
    assert!(repository.delta_head().await.unwrap() > 0);
    let head_cursor = postgres_delta_cursor(&app, token).await;

    let frame = timeout(Duration::from_millis(250), stream.body_mut().frame())
        .await
        .expect("durable probe emitted promptly")
        .expect("stream remains open")
        .expect("valid SSE frame")
        .into_data()
        .expect("SSE data frame");
    let frame = String::from_utf8(frame.to_vec()).unwrap();
    assert_eq!(
        frame,
        format!(
            "id: {head_cursor}\nevent: item-invalidation\ndata: {{\"cursor\":\"{head_cursor}\"}}\n\n"
        )
    );
    for forbidden in [
        item_id.to_string(),
        "SYNTHETIC-POSTGRES-PRIVATE-ITEM-STREAM-TITLE".to_owned(),
        "PostgreSQL integration test".to_owned(),
    ] {
        assert!(!frame.contains(&forbidden), "SSE leaked {forbidden}");
    }

    drop(stream);
    drop(app);
    drop(external);
    drop(repository);
    test_database.destroy().await;
}

fn postgres_stream_request(token: &str, cursor: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/v1/items/stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::ACCEPT, "text/event-stream")
        .header("last-event-id", cursor)
        .body(Body::empty())
        .unwrap()
}

async fn postgres_delta_cursor(app: &axum::Router, token: &str) -> String {
    let response: Response<Body> = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/items/delta?limit=200")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn sensitive_item_migration_backfills_historical_json_contracts() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; sensitivity migration test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;

    for migration in MIGRATOR.iter().filter(|migration| migration.version < 9) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-sensitivity migration applies");
    }
    let scope = seed_scope(pool, "sensitivity-migration-owner", "sensitivity-migration").await;
    let item_id = Uuid::new_v4();
    let historical_canary = "SYNTHETIC-SENSITIVE-MIGRATION-CANARY";
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, title, sibling_order) \
         VALUES ($1, $2, $3, 'task', $4, 0)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(historical_canary)
    .execute(pool)
    .await
    .unwrap();
    let historical_response = json!({
        "id": item_id,
        "title": historical_canary,
        "revision": 1,
    });
    sqlx::query(
        "INSERT INTO item_changes (workspace_id, item_id, item_revision, change_kind, payload) \
         VALUES ($1, $2, 1, 'upsert', $3)",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(&historical_response)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         state, resource_type, resource_id, response_json, expires_at) \
         VALUES ($1, 'items.create', $2, $3, 'completed', 'item', $4, $5, \
         clock_timestamp() + interval '1 hour')",
    )
    .bind(scope.workspace_id)
    .bind([7_u8; 32].as_slice())
    .bind([8_u8; 32].as_slice())
    .bind(item_id)
    .bind(&historical_response)
    .execute(pool)
    .await
    .unwrap();

    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 9)
        .expect("sensitivity migration is embedded");
    pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect("sensitivity migration applies");

    let stored_flag: bool = sqlx::query_scalar("SELECT is_sensitive FROM items WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let change_flag: bool = sqlx::query_scalar(
        "SELECT (payload ->> 'is_sensitive')::boolean FROM item_changes WHERE item_id = $1",
    )
    .bind(item_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let replay_flag: bool = sqlx::query_scalar(
        "SELECT (response_json ->> 'is_sensitive')::boolean FROM idempotency_keys \
         WHERE workspace_id = $1 AND namespace = 'items.create'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(!stored_flag && !change_flag && !replay_flag);
    assert_sensitive_json_constraints(pool, scope, item_id).await;

    test_database.destroy().await;
}

async fn assert_sensitive_json_constraints(pool: &PgPool, scope: DatabaseScope, item_id: Uuid) {
    let change_error = sqlx::query(
        "INSERT INTO item_changes (workspace_id, item_id, item_revision, change_kind, payload) \
         VALUES ($1, $2, 2, 'upsert', $3)",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(json!({"id": item_id, "title": "SYNTHETIC-MISSING-SENSITIVITY-CANARY"}))
    .execute(pool)
    .await
    .expect_err("a current upsert snapshot without sensitivity must fail closed");
    assert_eq!(
        change_error
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("item_changes_upsert_sensitivity_check")
    );

    let replay_error = sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         state, resource_type, resource_id, response_json, expires_at) \
         VALUES ($1, 'items.replace', $2, $3, 'completed', 'item', $4, $5, \
         clock_timestamp() + interval '1 hour')",
    )
    .bind(scope.workspace_id)
    .bind([9_u8; 32].as_slice())
    .bind([10_u8; 32].as_slice())
    .bind(item_id)
    .bind(json!({"id": item_id, "title": "SYNTHETIC-MISSING-REPLAY-SENSITIVITY-CANARY"}))
    .execute(pool)
    .await
    .expect_err("a completed current item replay without sensitivity must fail closed");
    assert_eq!(
        replay_error
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("idempotency_item_response_sensitivity_check")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn structural_item_migration_backfills_and_preserves_rich_partial_updates() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; structural migration test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 24) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-structural migration applies");
    }
    let scope = seed_scope(pool, "structural-migration-owner", "structural-migration").await;
    let task_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let imported_id = Uuid::new_v4();
    let owned_task_id = Uuid::new_v4();
    let owned_exact_task_id = Uuid::new_v4();
    let trashed_task_id = Uuid::new_v4();
    let legacy_leaf_goal_id = Uuid::new_v4();
    let mapping_only_calendar_id = Uuid::new_v4();
    let malformed_calendar_marker_id = Uuid::new_v4();
    for (id, kind, title, constraints) in [
        (
            task_id,
            "task",
            "Rich task",
            json!({"has_own_effort": true}),
        ),
        (
            event_id,
            "event",
            "Legacy event",
            json!({"calendar_event": {}}),
        ),
        (
            imported_id,
            "task",
            "Externally mapped Google Task",
            json!({"google_sync": {}}),
        ),
        (
            owned_task_id,
            "task",
            "DayWeave-owned Google Task",
            json!({}),
        ),
        (
            owned_exact_task_id,
            "task",
            "DayWeave-owned exact-deadline task",
            json!({}),
        ),
        (
            trashed_task_id,
            "task",
            "Recoverably trashed Google Task",
            json!({}),
        ),
        (
            legacy_leaf_goal_id,
            "goal",
            "Legacy executable leaf goal",
            json!({}),
        ),
        (
            mapping_only_calendar_id,
            "task",
            "Locally converted calendar mapping",
            json!({}),
        ),
        (
            malformed_calendar_marker_id,
            "task",
            "User metadata resembling a calendar marker",
            json!({"calendar_event": "user-tag"}),
        ),
    ] {
        sqlx::query(
            "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
             timezone_name, duration_seconds, deadline_at, scheduling_constraints) \
             VALUES ($1, $2, $3, $4, 'planned', $5, 'UTC', 3600, \
             '2026-09-03T12:00:00Z'::timestamptz, $6)",
        )
        .bind(id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(kind)
        .bind(title)
        .bind(constraints)
        .execute(pool)
        .await
        .expect("legacy item fixture");
    }
    let provider_account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
         display_label, encrypted_credentials, credential_key_version, status, sync_enabled, \
         is_default) VALUES ($1,$2,$3,'google',$4,'Structural migration provider',$5,1, \
         'active',true,false)",
    )
    .bind(provider_account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("structural-provider-{provider_account_id}"))
    .bind(vec![0xA5_u8; 64])
    .execute(pool)
    .await
    .expect("provider account fixture");
    let task_list_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
         collection_kind, remote_collection_id, display_name, provider_access_role, \
         provider_selected, selected, visible, sync_role, discovered_at, configured_at, \
         created_at, updated_at) VALUES ($1,$2,$3,$4,'task_list','structural-task-list', \
         'Structural task list','owner',true,true,true,'read_only',clock_timestamp(), \
         clock_timestamp(),clock_timestamp(),clock_timestamp())",
    )
    .bind(task_list_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .execute(pool)
    .await
    .expect("task-list fixture");
    let calendar_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
         collection_kind, remote_collection_id, display_name, provider_access_role, \
         provider_selected, selected, visible, sync_role, discovered_at, configured_at, \
         created_at, updated_at) VALUES ($1,$2,$3,$4,'calendar','structural-calendar', \
         'Structural calendar','reader',true,true,true,'read_only',clock_timestamp(), \
         clock_timestamp(),clock_timestamp(),clock_timestamp())",
    )
    .bind(calendar_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .execute(pool)
    .await
    .expect("calendar fixture");
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
        collection_id, entity_kind, local_entity_id, remote_resource_id, local_revision, \
         sync_state, ownership, projection_generation) VALUES ($1,$2,$3,$4, \
         'calendar_occurrence',$5,'mapping-only-calendar-item',1,'synced','external',1)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(provider_account_id)
    .bind(calendar_id)
    .bind(mapping_only_calendar_id)
    .execute(pool)
    .await
    .expect("mapping-only calendar provenance fixture");
    sqlx::query(
        "UPDATE items SET deadline_at = '2026-09-03T00:00:00Z'::timestamptz \
         WHERE workspace_id = $1 AND id IN ($2, $3, $4)",
    )
    .bind(scope.workspace_id)
    .bind(imported_id)
    .bind(owned_task_id)
    .bind(trashed_task_id)
    .execute(pool)
    .await
    .expect("Google Tasks legacy midnight due fixtures");
    sqlx::query(
        "UPDATE items SET trashed_at = '2026-09-03T01:00:00Z'::timestamptz, \
         tombstoned_at = '2026-09-03T01:00:00Z'::timestamptz \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(trashed_task_id)
    .execute(pool)
    .await
    .expect("recoverable Google Task trash fixture");
    for (local_id, remote_id, local_revision, ownership) in [
        (
            imported_id,
            "structural-external-task",
            Some(1_i64),
            "external",
        ),
        (owned_task_id, "structural-owned-task", None, "dayweave"),
        (
            owned_exact_task_id,
            "structural-owned-exact-task",
            Some(1_i64),
            "dayweave",
        ),
        (
            trashed_task_id,
            "structural-trashed-task",
            Some(1_i64),
            "external",
        ),
    ] {
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
             collection_id, entity_kind, local_entity_id, remote_resource_id, local_revision, \
             sync_state, ownership) VALUES ($1,$2,$3,$4,'item',$5,$6,$7,'synced',$8)",
        )
        .bind(Uuid::new_v4())
        .bind(scope.workspace_id)
        .bind(provider_account_id)
        .bind(task_list_id)
        .bind(local_id)
        .bind(remote_id)
        .bind(local_revision)
        .bind(ownership)
        .execute(pool)
        .await
        .expect("active Google Task mapping fixture");
    }
    sqlx::query(
        "INSERT INTO item_changes (workspace_id, item_id, item_revision, change_kind, payload, \
         changed_at) SELECT item.workspace_id, item.id, item.revision, 'upsert', \
         jsonb_build_object( \
             'id', item.id, 'is_sensitive', item.is_sensitive, 'kind', item.kind, \
             'status', item.status, 'title', item.title, 'notes', item.notes, \
             'timezone_name', item.timezone_name, 'duration_seconds', item.duration_seconds, \
             'deadline_at', item.deadline_at, 'earliest_start_at', item.earliest_start_at, \
             'recurrence', item.recurrence, 'flexible_constraints', item.scheduling_constraints, \
             'split_policy', jsonb_build_object('type', 'indivisible'), \
             'importance', item.importance, 'urgency', item.urgency, 'parent_id', NULL, \
             'sibling_order', item.sibling_order, 'is_executable', true, \
             'revision', item.revision, 'created_at', item.created_at, \
             'updated_at', item.updated_at, 'completed_at', item.completed_at, \
             'deleted_at', item.trashed_at \
         ), item.updated_at FROM items AS item WHERE item.workspace_id = $1 \
         AND item.id IN ($2, $3, $4, $5)",
    )
    .bind(scope.workspace_id)
    .bind(imported_id)
    .bind(owned_task_id)
    .bind(owned_exact_task_id)
    .bind(legacy_leaf_goal_id)
    .execute(pool)
    .await
    .expect("legacy Google Task delta fixtures");
    sqlx::query(
        "INSERT INTO item_changes (workspace_id, item_id, item_revision, change_kind, payload, \
         changed_at) SELECT item.workspace_id, item.id, item.revision, 'tombstone', \
         jsonb_build_object('id', item.id, 'revision', item.revision, \
             'deleted_at', item.trashed_at, 'parent_id', NULL), item.updated_at \
         FROM items AS item WHERE item.workspace_id = $1 AND item.id = $2",
    )
    .bind(scope.workspace_id)
    .bind(trashed_task_id)
    .execute(pool)
    .await
    .expect("legacy trashed Google Task delta fixture");
    let old_delta_head: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) FROM item_changes WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("legacy delta head");
    let replay_key_hash = [0x31_u8; 32];
    sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         state, resource_type, resource_id, response_json, expires_at) \
         SELECT $1, 'items.create', $2, $3, 'completed', 'item', $4, payload, \
         clock_timestamp() + interval '1 day' FROM item_changes \
         WHERE workspace_id = $1 AND item_id = $4 AND item_revision = 1",
    )
    .bind(scope.workspace_id)
    .bind(replay_key_hash.as_slice())
    .bind([0x32_u8; 32].as_slice())
    .bind(owned_task_id)
    .execute(pool)
    .await
    .expect("legacy item idempotency replay fixture");
    let goal_replay_key_hash = [0x33_u8; 32];
    sqlx::query(
        "INSERT INTO idempotency_keys (workspace_id, namespace, key_hash, request_fingerprint, \
         state, resource_type, resource_id, response_json, expires_at) \
         SELECT $1, 'items.create', $2, $3, 'completed', 'item', $4, payload, \
         clock_timestamp() + interval '1 day' FROM item_changes \
         WHERE workspace_id = $1 AND item_id = $4 AND item_revision = 1",
    )
    .bind(scope.workspace_id)
    .bind(goal_replay_key_hash.as_slice())
    .bind([0x34_u8; 32].as_slice())
    .bind(legacy_leaf_goal_id)
    .execute(pool)
    .await
    .expect("legacy Goal idempotency replay fixture");
    let pending_outbox_id = Uuid::new_v4();
    let approval_id = Uuid::new_v4();
    let outbound_intent_hash = [0x41_u8; 32];
    let outbound_payload =
        json!({"title": "DayWeave-owned Google Task", "due": "2026-09-03T00:00:00.000Z"});
    sqlx::query(
        "INSERT INTO google_outbound_previews (id, workspace_id, user_id, provider_account_id, \
         collection_id, collection_revision, collection_remote_id, item_id, item_revision, \
         entity_kind, operation, required_scope, provider_resource_id, expected_etag, intent_hash, \
         preview_hash, payload, expires_at, approved_at, capability_hash, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,1,'structural-task-list',$6,1,'task','upsert', \
         'https://www.googleapis.com/auth/tasks','structural-owned-task','etag',$7,$8,$9, \
         clock_timestamp() + interval '1 hour',clock_timestamp(),$10,clock_timestamp(), \
         clock_timestamp())",
    )
    .bind(approval_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(task_list_id)
    .bind(owned_task_id)
    .bind(outbound_intent_hash.as_slice())
    .bind([0x42_u8; 32].as_slice())
    .bind(&outbound_payload)
    .bind([0x43_u8; 32].as_slice())
    .execute(pool)
    .await
    .expect("approved outbound preview fixture");
    sqlx::query(
        "INSERT INTO google_sync_outbox (id, workspace_id, user_id, provider_account_id, \
         collection_id, item_id, item_revision, entity_kind, operation, remote_resource_id, \
         expected_etag, app_owned, payload, state, approval_id, intent_hash, collection_revision, \
         target_remote_collection_id, required_scope, available_at, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,1,'task','upsert','structural-owned-task','etag',true, \
         $7,'pending',$8,$9,1,'structural-task-list','https://www.googleapis.com/auth/tasks', \
         clock_timestamp(),clock_timestamp(),clock_timestamp())",
    )
    .bind(pending_outbox_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(task_list_id)
    .bind(owned_task_id)
    .bind(&outbound_payload)
    .bind(approval_id)
    .bind(outbound_intent_hash.as_slice())
    .execute(pool)
    .await
    .expect("pending owned Task outbox fixture");
    sqlx::query(
        "UPDATE google_outbound_previews SET consumed_at = clock_timestamp(), outbox_id = $3, \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(approval_id)
    .bind(pending_outbox_id)
    .execute(pool)
    .await
    .expect("consume outbound approval fixture");
    let uncertain_outbox_id = Uuid::new_v4();
    let uncertain_approval_id = Uuid::new_v4();
    let uncertain_intent_hash = [0x45_u8; 32];
    let uncertain_payload =
        json!({"title": "Externally mapped Google Task", "due": "2026-09-03T00:00:00.000Z"});
    sqlx::query(
        "INSERT INTO google_outbound_previews (id, workspace_id, user_id, provider_account_id, \
         collection_id, collection_revision, collection_remote_id, item_id, item_revision, \
         entity_kind, operation, required_scope, provider_resource_id, expected_etag, intent_hash, \
         preview_hash, payload, expires_at, approved_at, capability_hash, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,1,'structural-task-list',$6,1,'task','upsert', \
         'https://www.googleapis.com/auth/tasks',NULL,NULL,$7,$8,$9, \
         clock_timestamp() + interval '1 hour',clock_timestamp(),$10,clock_timestamp(), \
         clock_timestamp())",
    )
    .bind(uncertain_approval_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(task_list_id)
    .bind(imported_id)
    .bind(uncertain_intent_hash.as_slice())
    .bind([0x46_u8; 32].as_slice())
    .bind(&uncertain_payload)
    .bind([0x47_u8; 32].as_slice())
    .execute(pool)
    .await
    .expect("approved uncertain-create preview fixture");
    sqlx::query(
        "INSERT INTO google_sync_outbox (id, workspace_id, user_id, provider_account_id, \
         collection_id, item_id, item_revision, entity_kind, operation, remote_resource_id, \
         expected_etag, app_owned, payload, state, approval_id, intent_hash, collection_revision, \
         target_remote_collection_id, required_scope, claim_id, claimed_at, run_claim_id, \
         run_claim_generation, dispatch_nonce, dispatch_authorized_at, dispatch_expires_at, \
         provider_post_may_have_started, send_started_at, available_at, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,1,'task','upsert',NULL,NULL,true,$7,'delivering',$8,$9,1, \
         'structural-task-list','https://www.googleapis.com/auth/tasks',$10,clock_timestamp(), \
         $11,1,$12,clock_timestamp(),clock_timestamp() + interval '1 minute',true, \
         clock_timestamp(),clock_timestamp(),clock_timestamp(),clock_timestamp())",
    )
    .bind(uncertain_outbox_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(provider_account_id)
    .bind(task_list_id)
    .bind(imported_id)
    .bind(&uncertain_payload)
    .bind(uncertain_approval_id)
    .bind(uncertain_intent_hash.as_slice())
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .expect("in-flight uncertain markerless Task create fixture");
    sqlx::query(
        "UPDATE google_outbound_previews SET consumed_at = clock_timestamp(), outbox_id = $3, \
         updated_at = clock_timestamp() WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(uncertain_approval_id)
    .bind(uncertain_outbox_id)
    .execute(pool)
    .await
    .expect("consume uncertain-create approval fixture");

    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 24)
        .expect("structural migration is embedded");
    pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect("structural migration applies");

    let task_shape: (String, i32, i32, String, String, Option<String>, bool, bool) =
        sqlx::query_as(
            "SELECT duration_kind, duration_min_seconds, duration_max_seconds, duration_source, \
             deadline_kind, deadline_strength, has_own_effort, \
             (scheduling_constraints ->> 'has_own_effort')::boolean \
             FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(task_id)
        .fetch_one(pool)
        .await
        .expect("backfilled task shape");
    assert_eq!(
        task_shape,
        (
            "exact".to_owned(),
            3600,
            3600,
            "user".to_owned(),
            "date_time".to_owned(),
            Some("hard".to_owned()),
            true,
            true,
        )
    );
    let event_deadline: (String, Option<String>, bool) = sqlx::query_as(
        "SELECT deadline_kind, deadline_strength, deadline_at IS NOT NULL \
         FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(event_id)
    .fetch_one(pool)
    .await
    .expect("backfilled event deadline");
    assert_eq!(event_deadline, ("none".to_owned(), None, true));
    let event_duration_source: String =
        sqlx::query_scalar("SELECT duration_source FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(scope.workspace_id)
            .bind(event_id)
            .fetch_one(pool)
            .await
            .expect("insert-before-mapping Calendar provenance");
    assert_eq!(event_duration_source, "imported");
    let mapping_only_source: String =
        sqlx::query_scalar("SELECT duration_source FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(scope.workspace_id)
            .bind(mapping_only_calendar_id)
            .fetch_one(pool)
            .await
            .expect("mapping-only Calendar provenance");
    assert_eq!(
        mapping_only_source, "user",
        "a stale Calendar mapping alone cannot overwrite locally-authored duration provenance"
    );
    let malformed_marker_source: String =
        sqlx::query_scalar("SELECT duration_source FROM items WHERE workspace_id = $1 AND id = $2")
            .bind(scope.workspace_id)
            .bind(malformed_calendar_marker_id)
            .fetch_one(pool)
            .await
            .expect("malformed Calendar marker provenance");
    assert_eq!(
        malformed_marker_source, "user",
        "only object-shaped canonical Calendar metadata is import evidence"
    );
    let upgraded_leaf_goal: (i64, bool, bool) = sqlx::query_as(
        "SELECT revision, has_own_effort, \
         scheduling_constraints ? 'has_own_effort' \
         FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(legacy_leaf_goal_id)
    .fetch_one(pool)
    .await
    .expect("revisioned semantic-container executability transition");
    assert_eq!(upgraded_leaf_goal, (2, false, false));
    for id in [imported_id, owned_task_id] {
        let task_mapping_shape: (String, String, chrono::NaiveDate, bool, String, i64) =
            sqlx::query_as(
                "SELECT duration_source, deadline_kind, deadline_date, deadline_at IS NULL, \
                 deadline_strength, revision FROM items WHERE workspace_id = $1 AND id = $2",
            )
            .bind(scope.workspace_id)
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("upgraded Google Task structural shape");
        assert_eq!(
            task_mapping_shape,
            (
                "user".to_owned(),
                "date".to_owned(),
                chrono::NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(),
                true,
                "hard".to_owned(),
                2,
            )
        );
    }
    let owned_exact_shape: (String, String, bool, Option<chrono::NaiveDate>) = sqlx::query_as(
        "SELECT duration_source, deadline_kind, deadline_at = \
         '2026-09-03T12:00:00Z'::timestamptz, deadline_date \
         FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(owned_exact_task_id)
    .fetch_one(pool)
    .await
    .expect("non-midnight owned deadline upgrade shape");
    assert_eq!(
        owned_exact_shape,
        ("user".to_owned(), "date_time".to_owned(), true, None,),
        "owned exact-time intent must be preserved for explicit review"
    );
    let mapping_revisions: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT remote_resource_id, local_revision FROM provider_sync_mappings \
         WHERE workspace_id = $1 AND collection_id = $2 ORDER BY remote_resource_id",
    )
    .bind(scope.workspace_id)
    .bind(task_list_id)
    .fetch_all(pool)
    .await
    .expect("upgraded mapping revisions");
    assert_eq!(
        mapping_revisions,
        vec![
            ("structural-external-task".to_owned(), Some(2)),
            ("structural-owned-exact-task".to_owned(), Some(1)),
            ("structural-owned-task".to_owned(), None),
            ("structural-trashed-task".to_owned(), Some(2)),
        ],
        "only a mapping exactly matching the converted item's old revision advances"
    );
    let converted_deltas: Vec<(Uuid, i64, String, Value)> = sqlx::query_as(
        "SELECT item_id, item_revision, change_kind, payload FROM item_changes \
         WHERE workspace_id = $1 AND sequence > $2 ORDER BY item_id",
    )
    .bind(scope.workspace_id)
    .bind(old_delta_head)
    .fetch_all(pool)
    .await
    .expect("semantic conversion delta");
    assert_eq!(converted_deltas.len(), 4);
    for (item_id, revision, change_kind, payload) in converted_deltas {
        assert!(
            [
                imported_id,
                owned_task_id,
                trashed_task_id,
                legacy_leaf_goal_id,
            ]
            .contains(&item_id)
        );
        assert_eq!(revision, 2);
        if item_id == trashed_task_id {
            assert_eq!(change_kind, "tombstone");
            assert_eq!(payload["id"], trashed_task_id.to_string());
            assert_eq!(payload["revision"], 2);
            assert_eq!(payload["deleted_at"], "2026-09-03T01:00:00+00:00");
            continue;
        }
        assert_eq!(change_kind, "upsert");
        for field in [
            "duration_kind",
            "duration_seconds",
            "duration_min_seconds",
            "duration_max_seconds",
            "duration_source",
            "deadline_kind",
            "deadline_date",
            "deadline_at",
            "deadline_strength",
            "deadline_soft_weight",
            "has_own_effort",
            "blocked_reason_kind",
            "blocked_by_item_id",
            "blocked_reason",
        ] {
            assert!(
                payload.get(field).is_some(),
                "delta omits {field}: {payload}"
            );
        }
        assert_eq!(payload["revision"], 2);
        if item_id == legacy_leaf_goal_id {
            assert_eq!(payload["kind"], "goal");
            assert_eq!(payload["has_own_effort"], false);
            assert_eq!(payload["is_executable"], false);
            continue;
        }
        assert_eq!(payload["deadline_kind"], "date");
        assert_eq!(payload["deadline_date"], "2026-09-03");
        assert!(payload["deadline_at"].is_null());
        assert_eq!(payload["is_executable"], true);
    }
    let upgrade_envelopes: (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
              AND event_type = 'item.google_task_deadline_semantics_upgraded'), \
           (SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
              AND operation_type = 'item.google_task_deadline_semantics_upgraded' \
              AND base_revision = 1 AND result_revision = 2 AND outcome = 'succeeded')",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("semantic conversion mutation envelopes");
    assert_eq!(upgrade_envelopes, (3, 3));
    let container_upgrade_envelopes: (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
              AND event_type = 'item.container_executability_upgraded'), \
           (SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
              AND operation_type = 'item.container_executability_upgraded' \
              AND entity_id = $2 AND base_revision = 1 AND result_revision = 2 \
              AND outcome = 'succeeded')",
    )
    .bind(scope.workspace_id)
    .bind(legacy_leaf_goal_id)
    .fetch_one(pool)
    .await
    .expect("container executability mutation envelopes");
    assert_eq!(container_upgrade_envelopes, (1, 1));
    let replay_deadline: (String, String, bool, String) = sqlx::query_as(
        "SELECT response_json ->> 'deadline_kind', response_json ->> 'deadline_date', \
         response_json -> 'deadline_at' = 'null'::jsonb, \
         response_json ->> 'deadline_strength' FROM idempotency_keys \
         WHERE workspace_id = $1 AND namespace = 'items.create' AND key_hash = $2",
    )
    .bind(scope.workspace_id)
    .bind(replay_key_hash.as_slice())
    .fetch_one(pool)
    .await
    .expect("upgraded idempotency replay");
    assert_eq!(
        replay_deadline,
        (
            "date".to_owned(),
            "2026-09-03".to_owned(),
            true,
            "hard".to_owned(),
        )
    );
    let goal_replay_shape: (i64, bool, bool, String, String, bool) = sqlx::query_as(
        "SELECT (response_json ->> 'revision')::bigint, \
         (response_json ->> 'has_own_effort')::boolean, \
         (response_json ->> 'is_executable')::boolean, \
         response_json ->> 'duration_kind', response_json ->> 'deadline_kind', \
         response_json ?& ARRAY['duration_min_seconds', 'duration_max_seconds', \
           'duration_source', 'deadline_date', 'deadline_strength', \
           'deadline_soft_weight', 'blocked_reason_kind', 'blocked_by_item_id', \
           'blocked_reason'] \
         FROM idempotency_keys WHERE workspace_id = $1 \
           AND namespace = 'items.create' AND key_hash = $2",
    )
    .bind(scope.workspace_id)
    .bind(goal_replay_key_hash.as_slice())
    .fetch_one(pool)
    .await
    .expect("upgraded Goal idempotency replay");
    assert_eq!(
        goal_replay_shape,
        (
            1,
            false,
            false,
            "exact".to_owned(),
            "date_time".to_owned(),
            true,
        ),
        "historical revision/fingerprint stay frozen while replay shape becomes canonical"
    );
    let trashed_deadline_shape: (String, chrono::NaiveDate, bool, i64) = sqlx::query_as(
        "SELECT deadline_kind, deadline_date, trashed_at IS NOT NULL, revision \
         FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(trashed_task_id)
    .fetch_one(pool)
    .await
    .expect("recoverably trashed Task deadline upgrade");
    assert_eq!(
        trashed_deadline_shape,
        (
            "date".to_owned(),
            chrono::NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(),
            true,
            2,
        )
    );
    let retired_outbox: (i64, String, Option<Uuid>, String) = sqlx::query_as(
        "SELECT item_revision, state, claim_id, last_error_code FROM google_sync_outbox \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(pending_outbox_id)
    .fetch_one(pool)
    .await
    .expect("retired old-revision outbox");
    assert_eq!(
        retired_outbox,
        (
            1,
            "superseded".to_owned(),
            None,
            "canonical_deadline_semantics_upgraded".to_owned(),
        ),
        "migration must not forge a replacement approval for the new canonical revision"
    );
    let retained_approval: (i64, Vec<u8>, bool) = sqlx::query_as(
        "SELECT item_revision, intent_hash, consumed_at IS NOT NULL \
         FROM google_outbound_previews WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(approval_id)
    .fetch_one(pool)
    .await
    .expect("retained historical outbound approval");
    assert_eq!(retained_approval, (1, outbound_intent_hash.to_vec(), true));
    let retained_uncertain_create: (i64, String, String, bool, bool) = sqlx::query_as(
        "SELECT item_revision, state, last_error_code, provider_post_may_have_started, \
         send_started_at IS NOT NULL \
         FROM google_sync_outbox WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(uncertain_outbox_id)
    .fetch_one(pool)
    .await
    .expect("retained ambiguous markerless Task create evidence");
    assert_eq!(
        retained_uncertain_create,
        (
            1,
            "conflict".to_owned(),
            "provider_identity_unresolved".to_owned(),
            true,
            true,
        ),
        "migration must clear delivery authority without burying possible provider acceptance"
    );
    let cleared_uncertain_leases: (Option<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT claim_id, run_claim_id, dispatch_nonce FROM google_sync_outbox \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(uncertain_outbox_id)
    .fetch_one(pool)
    .await
    .expect("cleared ambiguous-create delivery leases");
    assert_eq!(cleared_uncertain_leases, (None, None, None));
    let retained_uncertain_approval: (i64, Vec<u8>, bool) = sqlx::query_as(
        "SELECT item_revision, intent_hash, consumed_at IS NOT NULL \
         FROM google_outbound_previews WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(uncertain_approval_id)
    .fetch_one(pool)
    .await
    .expect("retained uncertain-create approval history");
    assert_eq!(
        retained_uncertain_approval,
        (1, uncertain_intent_hash.to_vec(), true)
    );

    sqlx::query(
        "UPDATE items SET duration_kind = 'range', duration_min_seconds = 1800, \
         duration_seconds = 3600, duration_max_seconds = 7200, duration_source = 'assistant', \
         deadline_kind = 'date_time', deadline_strength = 'soft', deadline_soft_weight = 73 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(task_id)
    .execute(pool)
    .await
    .expect("store rich range and soft deadline");
    sqlx::query(
        "UPDATE items SET duration_seconds = 4200, \
         deadline_at = '2026-09-04T13:00:00Z'::timestamptz \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(task_id)
    .execute(pool)
    .await
    .expect("legacy partial scalar update preserves rich companions");
    let rich_shape: (String, i32, i32, i32, String, String, String, i32) = sqlx::query_as(
        "SELECT duration_kind, duration_min_seconds, duration_seconds, duration_max_seconds, \
         duration_source, deadline_kind, deadline_strength, deadline_soft_weight \
         FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(task_id)
    .fetch_one(pool)
    .await
    .expect("preserved rich shape");
    assert_eq!(
        rich_shape,
        (
            "range".to_owned(),
            1800,
            4200,
            7200,
            "assistant".to_owned(),
            "date_time".to_owned(),
            "soft".to_owned(),
            73,
        )
    );

    let project_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_kind, deadline_kind, has_own_effort, blocked_reason_kind, \
         blocked_reason) VALUES ($1,$2,$3,'project','blocked','Blocked project','UTC', \
         'unknown','none',true,'manual','Waiting for a decision')",
    )
    .bind(project_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("project and blocker shape");
    let projected_own_effort: bool = sqlx::query_scalar(
        "SELECT (scheduling_constraints ->> 'has_own_effort')::boolean \
         FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .expect("own-effort legacy projection");
    assert!(projected_own_effort);
    sqlx::query(
        "INSERT INTO item_dependencies (workspace_id, predecessor_item_id, successor_item_id, \
         dependency_kind, lag_seconds, dependency_strength, dependency_soft_weight) \
         VALUES ($1,$2,$3,'start_to_finish',60,'soft',25)",
    )
    .bind(scope.workspace_id)
    .bind(task_id)
    .bind(project_id)
    .execute(pool)
    .await
    .expect("expanded dependency shape");
    for invalid_lag in [-60, 1, 31_622_460] {
        let error = sqlx::query(
            "UPDATE item_dependencies SET lag_seconds = $4 \
             WHERE workspace_id = $1 AND predecessor_item_id = $2 AND successor_item_id = $3",
        )
        .bind(scope.workspace_id)
        .bind(task_id)
        .bind(project_id)
        .bind(invalid_lag)
        .execute(pool)
        .await
        .expect_err("dependency lag must be a bounded nonnegative whole minute");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|database| database.constraint()),
            Some("item_dependencies_lag_seconds_check")
        );
    }

    for (shape, statement, expected_constraint) in [
        (
            "exact duration with null values",
            "UPDATE items SET duration_kind = 'exact', duration_seconds = NULL, \
             duration_min_seconds = NULL, duration_max_seconds = NULL, duration_source = NULL \
             WHERE workspace_id = $1 AND id = $2",
            "items_duration_shape_check",
        ),
        (
            "range duration with null values",
            "UPDATE items SET duration_kind = 'range', duration_seconds = NULL, \
             duration_min_seconds = NULL, duration_max_seconds = NULL, duration_source = NULL \
             WHERE workspace_id = $1 AND id = $2",
            "items_duration_shape_check",
        ),
        (
            "date deadline without strength",
            "UPDATE items SET deadline_kind = 'date', deadline_date = DATE '2026-09-03', \
             deadline_at = NULL, deadline_strength = NULL, deadline_soft_weight = NULL \
             WHERE workspace_id = $1 AND id = $2",
            "items_deadline_shape_check",
        ),
        (
            "soft date deadline without weight",
            "UPDATE items SET deadline_kind = 'date', deadline_date = DATE '2026-09-03', \
             deadline_at = NULL, deadline_strength = 'soft', deadline_soft_weight = NULL \
             WHERE workspace_id = $1 AND id = $2",
            "items_deadline_shape_check",
        ),
    ] {
        let error = sqlx::query(AssertSqlSafe(statement.to_owned()))
            .bind(scope.workspace_id)
            .bind(project_id)
            .execute(pool)
            .await
            .expect_err(shape);
        assert_eq!(
            error
                .as_database_error()
                .and_then(|database| database.constraint()),
            Some(expected_constraint),
            "{shape}"
        );
    }
    let blocked_without_cause = sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_kind, deadline_kind, has_own_effort) \
         VALUES ($1,$2,$3,'task','blocked','Missing blocker','UTC','unknown','none',false)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect_err("blocked status requires an explicit cause");
    assert_eq!(
        blocked_without_cause
            .as_database_error()
            .and_then(|database| database.constraint()),
        Some("items_blocked_reason_shape_check")
    );
    let soft_dependency_without_weight = sqlx::query(
        "UPDATE item_dependencies SET dependency_strength = 'soft', \
         dependency_soft_weight = NULL WHERE workspace_id = $1 \
         AND predecessor_item_id = $2 AND successor_item_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(task_id)
    .bind(project_id)
    .execute(pool)
    .await
    .expect_err("soft dependency requires an explicit weight");
    assert_eq!(
        soft_dependency_without_weight
            .as_database_error()
            .and_then(|database| database.constraint()),
        Some("item_dependencies_strength_check")
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Rehearses all pre-0024 live execution states in isolated schemas.
async fn structural_migration_preflights_live_semantic_container_execution() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; structural execution preflight skipped");
        return;
    };
    for (case, kind, item_status, session_state) in [
        ("active-goal", "goal", "in_progress", "active"),
        ("paused-routine", "routine", "paused", "paused"),
    ] {
        let test_database = TestDatabase::create(&database_url).await;
        let pool = &test_database.pool;
        for migration in MIGRATOR.iter().filter(|migration| migration.version < 24) {
            pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
                .await
                .expect("pre-structural migration applies");
        }
        let scope = seed_scope(
            pool,
            &format!("structural-execution-{case}"),
            &format!("structural-execution-{case}"),
        )
        .await;
        let item_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
             timezone_name, duration_seconds, scheduling_constraints) \
             VALUES ($1,$2,$3,$4,$5,'Legacy executing container','UTC',3600,'{}'::jsonb)",
        )
        .bind(item_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(kind)
        .bind(item_status)
        .execute(pool)
        .await
        .expect("legacy semantic-container fixture");
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
             occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
             accumulated_seconds, actual_seconds, started_at, running_since, paused_at, \
             pause_until, pause_reason, ended_at, created_at, updated_at, observed_running_since) \
             VALUES ($1,$2,$3,1,NULL,0,NULL,$4,$5,1,0,NULL, \
             '2026-09-03T10:00:00Z'::timestamptz, \
             CASE WHEN $5 = 'active' THEN '2026-09-03T10:00:00Z'::timestamptz ELSE NULL END, \
             CASE WHEN $5 = 'paused' THEN '2026-09-03T10:30:00Z'::timestamptz ELSE NULL END, \
             NULL,NULL,NULL,'2026-09-03T10:00:00Z'::timestamptz, \
             '2026-09-03T10:30:00Z'::timestamptz, \
             CASE WHEN $5 = 'active' THEN '2026-09-03T10:00:00Z'::timestamptz ELSE NULL END)",
        )
        .bind(session_id)
        .bind(scope.workspace_id)
        .bind(item_id)
        .bind(Uuid::new_v4())
        .bind(session_state)
        .execute(pool)
        .await
        .expect("legacy open execution fixture");
        sqlx::query(
            "UPDATE execution_state SET revision = 1, active_session_id = $2, \
             updated_at = '2026-09-03T10:30:00Z'::timestamptz WHERE workspace_id = $1",
        )
        .bind(scope.workspace_id)
        .bind(session_id)
        .execute(pool)
        .await
        .expect("legacy current execution fixture");

        let migration = MIGRATOR
            .iter()
            .find(|migration| migration.version == 24)
            .expect("structural migration is embedded");
        let error = pool
            .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect_err("ambiguous open semantic-container execution must fail preflight");
        assert!(
            error
                .to_string()
                .contains("active/paused Goal or Routine without explicit own effort"),
            "unexpected {case} preflight error: {error}"
        );
        test_database.destroy().await;
    }

    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 21) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-structural migration applies");
    }
    let scope = seed_scope(
        pool,
        "structural-deferred-goal-owner",
        "structural-deferred-goal",
    )
    .await;
    let item_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds, scheduling_constraints) \
         VALUES ($1,$2,$3,'goal','planned','Legacy deferred goal','UTC',3600,'{}'::jsonb)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("legacy deferred Goal item");
    let session_id = Uuid::new_v4();
    let mut transaction = pool.begin().await.expect("begin deferred fixture");
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, paused_at, \
         pause_until, pause_reason, ended_at, created_at, updated_at, move_start, move_end, \
         observed_running_since) VALUES ($1,$2,$3,1,NULL,0,NULL,$4,'deferred',2,0,0, \
         '2026-09-03T10:00:00Z'::timestamptz,NULL,'2026-09-03T10:30:00Z'::timestamptz, \
         NULL,NULL,'2026-09-03T10:30:00Z'::timestamptz, \
         '2026-09-03T10:00:00Z'::timestamptz,'2026-09-03T10:30:00Z'::timestamptz, \
         '2026-09-04T11:00:00Z'::timestamptz,'2026-09-04T12:00:00Z'::timestamptz,NULL)",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .execute(&mut *transaction)
    .await
    .expect("legacy deferred execution session");
    sqlx::query(
        "INSERT INTO execution_defer_replacement_claims (workspace_id, \
         source_deferred_session_id, item_id, source_item_revision, execution_epoch, \
         occurrence_id, source_session_index, replacement_session_index, \
         planned_duration_seconds, planned_duration_source, actionable, \
         consumed_before_seconds, consumed_by_source_seconds, remaining_duration_seconds, \
         move_start, move_end, created_at) VALUES ($1,$2,$3,1,1,NULL,0,1,3600, \
         'legacy_move_window',true,0,0,3600,'2026-09-04T11:00:00Z'::timestamptz, \
         '2026-09-04T12:00:00Z'::timestamptz,'2026-09-03T10:30:00Z'::timestamptz)",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .bind(item_id)
    .execute(&mut *transaction)
    .await
    .expect("legacy live defer replacement claim");
    transaction
        .commit()
        .await
        .expect("commit exact deferred fixture");
    for migration in MIGRATOR
        .iter()
        .filter(|migration| (21..24).contains(&migration.version))
    {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("post-claim pre-structural migration applies");
    }
    // Current status/trash/topology are reversible without changing the
    // execution epoch, so they must not hide an unconsumed actionable claim
    // from the migration preflight.
    sqlx::query(
        "UPDATE items SET status = 'cancelled', trashed_at = clock_timestamp(), \
         tombstoned_at = clock_timestamp(), revision = revision + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .execute(pool)
    .await
    .expect("make deferred container temporarily terminal and trashed");

    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 24)
        .expect("structural migration is embedded");
    let error = pool
        .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect_err("live deferred semantic-container authority must fail preflight");
    assert!(
        error
            .to_string()
            .contains("live deferred Goal or Routine without explicit own effort"),
        "unexpected deferred-container preflight error: {error}"
    );
    test_database.destroy().await;

    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 24) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-structural migration applies");
    }
    let scope = seed_scope(
        pool,
        "structural-google-task-execution-owner",
        "structural-google-task-execution",
    )
    .await;
    let item_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds, deadline_at, scheduling_constraints) \
         VALUES ($1,$2,$3,'task','paused','Paused mapped Google Task','UTC',3600, \
         '2026-09-03T00:00:00Z'::timestamptz,'{}'::jsonb)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("legacy mapped Google Task fixture");
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
         display_label, encrypted_credentials, credential_key_version, status, sync_enabled, \
         is_default) VALUES ($1,$2,$3,'google',$4,'Preflight provider',$5,1, \
         'active',true,false)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("structural-preflight-provider-{account_id}"))
    .bind(vec![0xB1_u8; 64])
    .execute(pool)
    .await
    .expect("preflight provider account");
    let collection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
         collection_kind, remote_collection_id, display_name, provider_access_role, \
         provider_selected, selected, visible, sync_role, discovered_at, configured_at, \
         created_at, updated_at) VALUES ($1,$2,$3,$4,'task_list','preflight-task-list', \
         'Preflight task list','owner',true,true,true,'read_only',clock_timestamp(), \
         clock_timestamp(),clock_timestamp(),clock_timestamp())",
    )
    .bind(collection_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .execute(pool)
    .await
    .expect("preflight Task list");
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
         collection_id, entity_kind, local_entity_id, remote_resource_id, local_revision, \
         sync_state, ownership) VALUES ($1,$2,$3,$4,'item',$5,'paused-midnight-task',1, \
         'synced','external')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(account_id)
    .bind(collection_id)
    .bind(item_id)
    .execute(pool)
    .await
    .expect("mapped midnight Task");
    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, paused_at, \
         pause_until, pause_reason, ended_at, created_at, updated_at, observed_running_since) \
         VALUES ($1,$2,$3,1,NULL,0,NULL,$4,'paused',1,0,NULL, \
         '2026-09-03T10:00:00Z'::timestamptz,NULL, \
         '2026-09-03T10:30:00Z'::timestamptz,NULL,NULL,NULL, \
         '2026-09-03T10:00:00Z'::timestamptz,'2026-09-03T10:30:00Z'::timestamptz,NULL)",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .expect("paused mapped Task session");
    sqlx::query(
        "UPDATE execution_state SET revision = 1, active_session_id = $2, \
         updated_at = '2026-09-03T10:30:00Z'::timestamptz WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(session_id)
    .execute(pool)
    .await
    .expect("mapped Task current execution");
    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 24)
        .expect("structural migration is embedded");
    let error = pool
        .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect_err("mapped Task revision upgrade must not strand an open session's Defer");
    assert!(
        error
            .to_string()
            .contains("active/paused mapped Google Task whose deadline requires date-only"),
        "unexpected mapped-Task preflight error: {error}"
    );
    test_database.destroy().await;
}

#[tokio::test]
async fn structural_migration_preflights_unrepresentable_google_task_due_date() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; structural preflight test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 24) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-structural migration applies");
    }
    let scope = seed_scope(
        pool,
        "structural-google-task-date-bound-owner",
        "structural-google-task-date-bound",
    )
    .await;
    let item_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds, deadline_at, scheduling_constraints) \
         VALUES ($1,$2,$3,'task','planned','Unrepresentable mapped Google due','UTC',3600, \
         '9999-12-31T00:00:00Z'::timestamptz,'{}'::jsonb)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("legacy maximum-date Task fixture");
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
         display_label, encrypted_credentials, credential_key_version, status, sync_enabled, \
         is_default) VALUES ($1,$2,$3,'google',$4,'Date boundary provider',$5,1, \
         'active',true,false)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("structural-date-bound-provider-{account_id}"))
    .bind(vec![0xB2_u8; 64])
    .execute(pool)
    .await
    .expect("date-boundary provider account");
    let collection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO google_sync_collections (id, workspace_id, user_id, provider_account_id, \
         collection_kind, remote_collection_id, display_name, provider_access_role, \
         provider_selected, selected, visible, sync_role, discovered_at, configured_at, \
         created_at, updated_at) VALUES ($1,$2,$3,$4,'task_list','date-bound-task-list', \
         'Date-bound task list','owner',true,true,true,'read_only',clock_timestamp(), \
         clock_timestamp(),clock_timestamp(),clock_timestamp())",
    )
    .bind(collection_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(account_id)
    .execute(pool)
    .await
    .expect("date-boundary Task list");
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
         collection_id, entity_kind, local_entity_id, remote_resource_id, local_revision, \
         sync_state, ownership) VALUES ($1,$2,$3,$4,'item',$5,'maximum-date-task',1, \
         'synced','external')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(account_id)
    .bind(collection_id)
    .bind(item_id)
    .execute(pool)
    .await
    .expect("maximum-date Task mapping");

    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 24)
        .expect("structural migration is embedded");
    let error = pool
        .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect_err("an unrepresentable Google due date must fail with repair guidance");
    assert!(
        error
            .to_string()
            .contains("outside the supported date-only range 0001-01-01 through 9999-12-30"),
        "unexpected Google due-date preflight error: {error}"
    );
    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Rehearses each legacy-data preflight in an isolated schema.
async fn structural_migration_preflights_invalid_legacy_dependency_lags() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; structural preflight test skipped");
        return;
    };
    for (case, invalid_lag) in [
        ("negative", -60),
        ("subminute", 1),
        ("oversized", 31_622_460),
    ] {
        let test_database = TestDatabase::create(&database_url).await;
        let pool = &test_database.pool;
        for migration in MIGRATOR.iter().filter(|migration| migration.version < 24) {
            pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
                .await
                .expect("pre-structural migration applies");
        }
        let scope = seed_scope(
            pool,
            &format!("structural-preflight-{case}"),
            &format!("structural-preflight-{case}"),
        )
        .await;
        let predecessor = Uuid::new_v4();
        let successor = Uuid::new_v4();
        for (id, title) in [(predecessor, "Predecessor"), (successor, "Successor")] {
            sqlx::query(
                "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
                 timezone_name, duration_seconds) VALUES ($1,$2,$3,'task','planned',$4,'UTC',60)",
            )
            .bind(id)
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .bind(title)
            .execute(pool)
            .await
            .expect("legacy dependency item");
        }
        sqlx::query(
            "INSERT INTO item_dependencies (workspace_id, predecessor_item_id, \
             successor_item_id, lag_seconds) VALUES ($1,$2,$3,$4)",
        )
        .bind(scope.workspace_id)
        .bind(predecessor)
        .bind(successor)
        .bind(invalid_lag)
        .execute(pool)
        .await
        .expect("pre-0024 schema permits legacy lag");

        let migration = MIGRATOR
            .iter()
            .find(|migration| migration.version == 24)
            .expect("structural migration is embedded");
        let error = pool
            .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect_err("invalid legacy dependency lag must fail preflight");
        assert!(
            error
                .as_database_error()
                .is_some_and(|database| database.message().contains("whole-minute range")),
            "{case}: {error}"
        );
        test_database.destroy().await;
    }
}

#[tokio::test]
async fn structural_migration_preflights_oversized_legacy_duration() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; duration preflight test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 24) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-structural migration applies");
    }
    let scope = seed_scope(
        pool,
        "structural-duration-preflight",
        "structural-duration-preflight",
    )
    .await;
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds) \
         VALUES ($1,$2,$3,'task','planned','Oversized duration','UTC',31622401)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .execute(pool)
    .await
    .expect("pre-0024 schema permits oversized duration");
    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 24)
        .expect("structural migration is embedded");
    let error = pool
        .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect_err("oversized legacy duration must fail preflight");
    assert!(
        error
            .as_database_error()
            .is_some_and(|database| database.message().contains("31622400-second maximum")),
        "{error}"
    );
    test_database.destroy().await;
}

fn new_item(
    id: Uuid,
    title: &str,
    kind: ItemKind,
    parent_id: Option<Uuid>,
    sibling_order: u32,
) -> NewItem {
    NewItem {
        id,
        is_sensitive: false,
        kind,
        status: ItemStatus::Planned,
        title: title.to_owned(),
        notes: Some("PostgreSQL integration test".to_owned()),
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
        recurrence: if kind == ItemKind::Goal {
            None
        } else {
            Some(json!({"type": "weekly", "weekdays": ["monday"]}))
        },
        flexible_constraints: json!({"energy": "deep"}),
        has_own_effort: None,
        split_policy: SplitPolicy::Splittable {
            minimum_chunk_seconds: 1200,
            maximum_chunk_seconds: 2400,
        },
        importance: 80,
        urgency: 60,
        parent_id,
        sibling_order,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

fn replacement(
    item: &dayweave_api::items::Item,
    parent_id: Option<Uuid>,
    status: ItemStatus,
) -> ReplaceItem {
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
        parent_id,
        sibling_order: item.sibling_order,
        blocked_reason_kind: item.blocked_reason_kind,
        blocked_by_item_id: item.blocked_by_item_id,
        blocked_reason: item.blocked_reason.clone(),
    }
}

fn idempotency(key: &str, marker: u8) -> IdempotencyKey {
    IdempotencyKey {
        key: key.to_owned(),
        fingerprint: [marker; 32],
    }
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    _migration_lease: OwnedSemaphorePermit,
}

impl TestDatabase {
    async fn create(database_url: &str) -> Self {
        static DATABASE_LEASE: OnceLock<Arc<Semaphore>> = OnceLock::new();
        // Migration 0024 deliberately replaces a wide set of constraints and
        // triggers. PostgreSQL's default shared lock table cannot safely host
        // several isolated copies of that migration concurrently, including
        // in the stock PostgreSQL service used by CI.
        let migration_lease =
            Arc::clone(DATABASE_LEASE.get_or_init(|| Arc::new(Semaphore::new(1))))
                .acquire_owned()
                .await
                .expect("item test database semaphore remains open");
        let options = PgConnectOptions::from_str(database_url)
            .expect("valid DAYWEAVE_TEST_DATABASE_URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        let schema = format!("dayweave_item_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated test schema");
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
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
            _migration_lease: migration_lease,
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
         VALUES ($1, $2, 'Item test owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(subject)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'Item test workspace', 'Europe/Madrid')",
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
