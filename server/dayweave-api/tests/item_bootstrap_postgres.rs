use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{Duration as ChronoDuration, Utc};
use dayweave_api::{
    AppState,
    auth::StaticTokenAuthenticator,
    http::router,
    items::{IdempotencyKey, Item, ItemRepository, ItemService, NewItem, ReplaceItem},
    persistence::{DatabaseScope, MIGRATOR, PostgresItemRepository},
    proposals::{InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions as _, Executor as _, PgPool, Row as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{str::FromStr as _, sync::Arc, time::Duration};
use tower::ServiceExt as _;
use uuid::Uuid;

const TOKEN: &str = "synthetic-bootstrap-pg-token";

fn app(items: Arc<ItemService>) -> Router {
    app_with_token(items, TOKEN)
}

fn app_with_token(items: Arc<ItemService>, token: &str) -> Router {
    let ready = Readiness::default();
    ready.set_ready(true);
    let proposals = Arc::new(ProposalService::new(
        Arc::new(InMemoryProposalRepository::default()),
        Arc::new(SystemClock),
        Duration::from_hours(24),
    ));
    router(
        AppState::new(
            proposals,
            Arc::new(StaticTokenAuthenticator::from_plaintext(&[token])),
            ready,
        )
        .with_items(items),
    )
}

async fn delta(app: &Router, cursor: Option<&str>, bootstrap: bool, status: StatusCode) -> Value {
    let mut uri = "/v1/items/delta?limit=50".to_owned();
    if bootstrap {
        uri.push_str("&bootstrap=current");
    }
    if let Some(cursor) = cursor {
        uri.push_str("&cursor=");
        uri.push_str(cursor);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), status);
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    value
}

fn input(index: u128, parent: Option<Uuid>) -> NewItem {
    serde_json::from_value(json!({"id":Uuid::from_u128(index),"is_sensitive":false,"kind":"task","status":"planned",
        "title":"Synthetic deep item","notes":null,"timezone_name":"UTC","duration_seconds":60,
        "deadline_at":null,"earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":1,"urgency":1,"parent_id":parent,"sibling_order":0})).unwrap()
}

/// Synthetic historical revisions exercise the transport boundary, not completion writers.
/// The current forest and every history group are production-shaped; no guards are disabled.
async fn seed_history(pool: &PgPool, scope: DatabaseScope) -> Vec<Item> {
    let now = Utc::now() - ChronoDuration::days(60);
    let mut items = (1..=5_002_u128)
        .map(|index| {
            let parent = (index > 1 && index <= 5_000).then(|| Uuid::from_u128(index - 1));
            let mut item = Item::new(input(index, parent), now).unwrap();
            item.is_executable = index >= 5_000;
            item.revision = 7;
            if index > 5_000 {
                item.deleted_at =
                    Some(Utc::now() - ChronoDuration::days(if index == 5_001 { 1 } else { 31 }));
                item.updated_at = item.deleted_at.unwrap();
                item.is_executable = false;
            }
            item
        })
        .collect::<Vec<_>>();
    let mut tx = pool.begin().await.unwrap();
    for chunk in items.chunks(300) {
        let values = serde_json::to_value(chunk).unwrap();
        sqlx::query("INSERT INTO items(id,workspace_id,created_by_user_id,kind,status,title,timezone_name, \
            duration_kind,duration_seconds,duration_min_seconds,duration_max_seconds,duration_source, \
            deadline_kind,importance,urgency,revision,created_at,updated_at,trashed_at) \
            SELECT (v->>'id')::uuid,$1,$2,'task','planned',v->>'title','UTC','exact',60,60,60,'user','none',1,1,7, \
            (v->>'created_at')::timestamptz,(v->>'updated_at')::timestamptz,(v->>'deleted_at')::timestamptz \
            FROM jsonb_array_elements($3) v")
            .bind(scope.workspace_id).bind(scope.user_id).bind(values).execute(&mut *tx).await.unwrap();
    }
    for chunk in items[..5_000].chunks(300) {
        sqlx::query("INSERT INTO item_hierarchy(workspace_id,parent_item_id,child_item_id) \
            SELECT $1,(v->>'parent_id')::uuid,(v->>'id')::uuid FROM jsonb_array_elements($2) v WHERE v->>'parent_id' IS NOT NULL")
            .bind(scope.workspace_id).bind(serde_json::to_value(chunk).unwrap()).execute(&mut *tx).await.unwrap();
    }
    for revision in 1..=7 {
        for chunk in items.chunks_mut(300) {
            sqlx::query("SELECT set_config('dayweave.item_change_group_id',$1,true)")
                .bind(Uuid::new_v4().to_string())
                .execute(&mut *tx)
                .await
                .unwrap();
            let changes = chunk.iter().map(|item| { let mut item=item.clone(); item.revision=revision;
                let payload = item.deleted_at.map_or_else(|| serde_json::to_value(&item).unwrap(), |deleted| json!({
                    "id":item.id,"revision":revision,"deleted_at":deleted,"parent_id":item.parent_id}));
                json!({"id":item.id,"revision":revision,"kind":if item.deleted_at.is_some(){"tombstone"}else{"upsert"},"payload":payload})
            }).collect::<Vec<_>>();
            sqlx::query("INSERT INTO item_changes(workspace_id,item_id,item_revision,change_kind,payload,change_group_id) \
                SELECT $1,(v->>'id')::uuid,(v->>'revision')::bigint,v->>'kind',v->'payload', \
                current_setting('dayweave.item_change_group_id')::uuid FROM jsonb_array_elements($2) v")
                .bind(scope.workspace_id).bind(json!(changes)).execute(&mut *tx).await.unwrap();
        }
    }
    tx.commit().await.unwrap();
    items
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One isolated database follows capture, races, exact continuation, expiry and cleanup.
async fn deep_cold_http_bootstrap_pins_current_evidence_across_writes_expiry_and_restart() {
    let Ok(url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        return;
    };
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .disable_statement_logging();
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    let schema = format!("bootstrap_test_{}", Uuid::new_v4().simple());
    admin
        .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .await
        .unwrap();
    let connection_schema = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |conn, _| {
            let statement = format!("SET search_path TO {connection_schema}");
            Box::pin(async move {
                conn.execute(AssertSqlSafe(statement)).await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let scope = DatabaseScope {
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
    };
    sqlx::query("INSERT INTO users(id,auth_subject,display_name) VALUES($1,$2,'Synthetic owner')")
        .bind(scope.user_id)
        .bind(scope.user_id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspaces(id,owner_user_id,slug,name) VALUES($1,$2,'bootstrap','Synthetic workspace')")
        .bind(scope.workspace_id).bind(scope.user_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,'owner')")
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(&pool)
        .await
        .unwrap();
    let items = seed_history(&pool, scope).await;
    let repository = Arc::new(PostgresItemRepository::new(pool.clone(), scope));
    assert!(repository.delta_head().await.unwrap() > 25_600);
    let service = Arc::new(ItemService::new(repository.clone(), Arc::new(SystemClock)));
    let application = app(service.clone());
    // Corrupt unpinned source identity without changing its exact-revision metadata.
    // A complete-looking forest must still fail, and leave no issued ticket.
    sqlx::query("UPDATE item_changes SET payload=jsonb_set(payload,'{revision}','9'::jsonb) WHERE workspace_id=$1 AND item_id=$2 AND item_revision=7")
        .bind(scope.workspace_id).bind(Uuid::from_u128(5000)).execute(&pool).await.unwrap();
    delta(&application, None, true, StatusCode::INTERNAL_SERVER_ERROR).await;
    let issued: i64 = sqlx::query_scalar("SELECT count(*) FROM item_bootstrap_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(issued, 0);
    sqlx::query("UPDATE item_changes SET payload=jsonb_set(payload,'{revision}','7'::jsonb) WHERE workspace_id=$1 AND item_id=$2 AND item_revision=7")
        .bind(scope.workspace_id).bind(Uuid::from_u128(5000)).execute(&pool).await.unwrap();
    let mut incomplete = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO item_bootstrap_snapshots(id,workspace_id,user_id,head_sequence,created_at,cutoff_at,expires_at,member_count,payload_bytes) \
        SELECT $1,$2,$3,(SELECT max(sequence) FROM item_changes WHERE workspace_id=$2),at,at-interval '720 hours',at+interval '10 minutes',0,0 FROM (SELECT clock_timestamp() at) observed")
        .bind(Uuid::new_v4()).bind(scope.workspace_id).bind(scope.user_id).execute(&mut *incomplete).await.unwrap();
    assert!(
        incomplete.commit().await.is_err(),
        "deferred seal refuses an omitted active forest"
    );
    let mut page = delta(&application, None, true, StatusCode::OK).await;
    let first_cursor = page["next_cursor"].as_str().unwrap().to_owned();
    assert_eq!(page["changes"].as_array().unwrap().len(), 300);
    let reuse = delta(&application, None, true, StatusCode::OK).await;
    assert_eq!(reuse, page);
    let mut replacement = serde_json::to_value(input(5_000, Some(Uuid::from_u128(4_998)))).unwrap();
    replacement.as_object_mut().unwrap().remove("id");
    replacement["title"] = json!("Changed after capture");
    service
        .replace(
            Uuid::from_u128(5_000),
            7,
            serde_json::from_value::<ReplaceItem>(replacement).unwrap(),
            IdempotencyKey {
                key: "after-bootstrap-capture".into(),
                fingerprint: [3; 32],
            },
        )
        .await
        .unwrap();
    service
        .trash(
            Uuid::from_u128(5000),
            8,
            IdempotencyKey {
                key: "after-bootstrap-delete".into(),
                fingerprint: [4; 32],
            },
        )
        .await
        .unwrap();
    // A new process/repository continues the original durable ticket.
    let restarted = app(Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
        Arc::new(SystemClock),
    )));
    let mut all = Vec::new();
    let mut pages = 0;
    loop {
        pages += 1;
        all.extend(page["changes"].as_array().unwrap().clone());
        if page["has_more"] == false {
            break;
        }
        page = delta(
            &restarted,
            page["next_cursor"].as_str(),
            false,
            StatusCode::OK,
        )
        .await;
    }
    assert_eq!(pages, 17);
    assert_eq!(all.len(), 5_001);
    assert_eq!(
        all.iter()
            .filter(|change| change["type"] == "tombstone")
            .count(),
        1
    );
    let active = all
        .iter()
        .filter_map(|change| change.get("item"))
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 5_000);
    assert!(active.iter().all(|item| item["revision"] == 7));
    assert!(
        active
            .iter()
            .all(|item| item["title"] == "Synthetic deep item")
    );
    let catchup = delta(
        &restarted,
        page["next_cursor"].as_str(),
        false,
        StatusCode::OK,
    )
    .await;
    assert!(
        catchup["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["item"]["title"] == "Changed after capture")
    );
    let header =
        sqlx::query("SELECT id,head_sequence FROM item_bootstrap_snapshots WHERE workspace_id=$1")
            .bind(scope.workspace_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let snapshot_id: Uuid = header.get("id");
    let source: i64 = sqlx::query_scalar(
        "SELECT change_sequence FROM item_bootstrap_members WHERE snapshot_id=$1 LIMIT 1",
    )
    .bind(snapshot_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let late=sqlx::query("INSERT INTO item_bootstrap_members(snapshot_id,workspace_id,ordinal,change_sequence) VALUES($1,$2,1,$3)")
        .bind(snapshot_id).bind(scope.workspace_id).bind(source).execute(&pool).await.unwrap_err();
    assert!(late.to_string().contains("original capture"));
    assert!(sqlx::query("INSERT INTO item_bootstrap_members(snapshot_id,workspace_id,ordinal,change_sequence) VALUES($1,$2,1,$3)")
        .bind(snapshot_id).bind(Uuid::new_v4()).bind(source).execute(&pool).await.is_err());
    assert!(
        sqlx::query("TRUNCATE item_bootstrap_members")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE item_changes SET payload=payload WHERE sequence=$1")
            .bind(source)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM item_bootstrap_members WHERE snapshot_id=$1")
            .bind(snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE item_bootstrap_snapshots SET head_sequence=head_sequence WHERE id=$1")
            .bind(snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    // Only the isolated expiry fixture guard is disabled; production guard remains unchanged.
    let mut tx = pool.begin().await.unwrap();
    tx.execute(
        "ALTER TABLE item_bootstrap_snapshots DISABLE TRIGGER item_bootstrap_snapshots_guard",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE item_bootstrap_snapshots SET created_at=created_at-interval '11 minutes',cutoff_at=cutoff_at-interval '11 minutes',expires_at=expires_at-interval '11 minutes' WHERE id=$1")
        .bind(snapshot_id).execute(&mut *tx).await.unwrap();
    tx.execute(
        "ALTER TABLE item_bootstrap_snapshots ENABLE TRIGGER item_bootstrap_snapshots_guard",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let expired = delta(&restarted, Some(&first_cursor), false, StatusCode::CONFLICT).await;
    assert_eq!(expired["error"]["code"], "item_bootstrap_expired");
    let fresh = delta(&restarted, None, true, StatusCode::OK).await;
    assert_ne!(fresh["next_cursor"], first_cursor);
    let removed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM item_bootstrap_members WHERE snapshot_id=$1")
            .bind(snapshot_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        removed, 0,
        "bounded expiry cleanup releases the old pinned manifest"
    );
    assert_eq!(items.len(), 5_002);
    pool.close().await;
    admin
        .execute(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .await
        .unwrap();
    admin.close().await;
}

/// Opt-in inert native acceptance host. Only the disposable database and a
/// caller-created private directory are used; no production configuration is read.
#[tokio::test]
#[allow(clippy::too_many_lines)] // Opt-in synthetic fixture owns setup, bounded host lifetime and teardown together.
async fn serve_disposable_native_bootstrap_fixture() {
    use std::{
        io::Write as _,
        os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    };
    let Ok(config_path) = std::env::var("DAYWEAVE_NATIVE_BOOTSTRAP_HOST_CONFIG") else {
        return;
    };
    let config_path = std::path::PathBuf::from(config_path);
    assert!(config_path.is_absolute());
    let directory = config_path.parent().unwrap();
    assert_eq!(
        std::fs::symlink_metadata(directory)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        !std::fs::symlink_metadata(directory)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let url = std::env::var("DAYWEAVE_TEST_DATABASE_URL").expect("synthetic database required");
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .disable_statement_logging();
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    let schema = format!("native_bootstrap_test_{}", Uuid::new_v4().simple());
    admin
        .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .await
        .unwrap();
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
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let scope = DatabaseScope {
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users(id,auth_subject,display_name) VALUES($1,$2,'Synthetic native owner')",
    )
    .bind(scope.user_id)
    .bind(scope.user_id.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspaces(id,owner_user_id,slug,name) VALUES($1,$2,'native-bootstrap','Synthetic native workspace')")
        .bind(scope.workspace_id).bind(scope.user_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,'owner')")
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(&pool)
        .await
        .unwrap();
    seed_history(&pool, scope).await;
    let token = format!("native-bootstrap-{}", Uuid::new_v4());
    let application = app_with_token(
        Arc::new(ItemService::new(
            Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
            Arc::new(SystemClock),
        )),
        &token,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = json!({"schema_version":1,"base_url":format!("http://{}/",listener.local_addr().unwrap()),
        "bearer_token":token,"expected_active_count":5000,"expected_trash_id":Uuid::from_u128(5001),
        "expected_root_id":Uuid::from_u128(1),"expected_leaf_id":Uuid::from_u128(5000)});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&config_path)
        .unwrap();
    file.write_all(serde_json::to_string(&config).unwrap().as_bytes())
        .unwrap();
    file.sync_all().unwrap();
    let stop_path = directory.join("stop");
    axum::serve(listener, application)
        .with_graceful_shutdown(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_mins(15);
            while !stop_path.exists() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .unwrap();
    pool.close().await;
    admin
        .execute(AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .await
        .unwrap();
    admin.close().await;
}
