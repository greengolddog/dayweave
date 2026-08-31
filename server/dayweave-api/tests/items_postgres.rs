use std::{str::FromStr, sync::Arc, time::Duration};

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
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::time::timeout;
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
        duration_seconds: Some(3600),
        deadline_at: None,
        earliest_start_at: None,
        recurrence: Some(json!({"type": "weekly", "weekdays": ["monday"]})),
        flexible_constraints: json!({"energy": "deep"}),
        split_policy: SplitPolicy::Splittable {
            minimum_chunk_seconds: 1200,
            maximum_chunk_seconds: 2400,
        },
        importance: 80,
        urgency: 60,
        parent_id,
        sibling_order,
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
        duration_seconds: item.duration_seconds,
        deadline_at: item.deadline_at,
        earliest_start_at: item.earliest_start_at,
        recurrence: item.recurrence.clone(),
        flexible_constraints: item.flexible_constraints.clone(),
        split_policy: item.split_policy.clone(),
        importance: item.importance,
        urgency: item.urgency,
        parent_id,
        sibling_order: item.sibling_order,
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
