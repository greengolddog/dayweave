use std::{str::FromStr as _, sync::Arc};

use chrono::{Duration, Utc};
use dayweave_api::{
    item_completion::{
        ItemCompletionCommand, ItemCompletionError, ItemCompletionMode,
        ItemCompletionProvenanceKind,
    },
    items::{
        BlockedReasonKind, IdempotencyKey, Item, ItemRepository, ItemService, ItemStatus, NewItem,
        ReplaceItem,
    },
    persistence::{DatabaseScope, MIGRATOR, PostgresItemRepository},
    proposals::SystemClock,
};
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions as _, Executor as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    scope: DatabaseScope,
    repository: Arc<PostgresItemRepository>,
    service: ItemService,
}

impl Fixture {
    async fn create() -> Option<Self> {
        let Ok(url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
            return None;
        };
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .unwrap();
        let schema = format!("completion_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .unwrap();
        let connection_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(5)
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
        sqlx::query("INSERT INTO users(id,auth_subject,display_name) VALUES($1,$2,'Synthetic completion owner')")
            .bind(scope.user_id).bind(scope.user_id.to_string()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO workspaces(id,owner_user_id,slug,name) VALUES($1,$2,'completion','Synthetic completion')")
            .bind(scope.workspace_id).bind(scope.user_id).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,'owner')",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(&pool)
        .await
        .unwrap();
        let repository = Arc::new(PostgresItemRepository::new(pool.clone(), scope));
        let service = ItemService::new(repository.clone(), Arc::new(SystemClock));
        Some(Self {
            admin,
            pool,
            schema,
            scope,
            repository,
            service,
        })
    }

    async fn cleanup(self) {
        self.pool.close().await;
        self.admin
            .execute(AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .await
            .unwrap();
        self.admin.close().await;
    }

    async fn create_item(&self, input: NewItem) -> Item {
        self.service.create(input, key()).await.unwrap().item
    }

    async fn set_status(&self, id: Uuid, status: ItemStatus) -> Item {
        let current = self.service.get(id).await.unwrap();
        let mut replacement = replacement(&current);
        replacement.status = status;
        if status != ItemStatus::Blocked {
            replacement.blocked_reason_kind = None;
            replacement.blocked_by_item_id = None;
            replacement.blocked_reason = None;
        }
        self.service
            .replace(id, current.revision, replacement, key())
            .await
            .unwrap()
            .item
    }

    async fn command(
        &self,
        id: Uuid,
        mode: ItemCompletionMode,
        required_for_parent: bool,
    ) -> ItemCompletionCommand {
        let snapshot = self.repository.get_completion(id).await.unwrap();
        ItemCompletionCommand {
            schema_version: 1,
            operation_id: Uuid::new_v4(),
            expected_item_revision: snapshot.item_revision,
            expected_completion_revision: snapshot.state.revision,
            expected_evidence_hash: snapshot.evidence_hash,
            required_for_parent,
            mode,
            reopening: None,
        }
    }
}

fn key() -> IdempotencyKey {
    IdempotencyKey {
        key: Uuid::new_v4().to_string(),
        fingerprint: [7; 32],
    }
}

fn input(id: Uuid, parent: Option<Uuid>) -> NewItem {
    serde_json::from_value(json!({"id":id,"is_sensitive":false,"kind":"task","status":"planned",
        "title":"Synthetic completion item","notes":null,"timezone_name":"UTC","duration_seconds":60,
        "deadline_at":null,"earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":1,"urgency":1,"parent_id":parent,"sibling_order":0})).unwrap()
}

fn replacement(item: &Item) -> ReplaceItem {
    let mut value = serde_json::to_value(item).unwrap();
    let object = value.as_object_mut().unwrap();
    for field in [
        "id",
        "is_executable",
        "revision",
        "created_at",
        "updated_at",
        "completed_at",
        "deleted_at",
    ] {
        object.remove(field);
    }
    serde_json::from_value(value).unwrap()
}

struct EarlierClock(chrono::DateTime<Utc>);
impl dayweave_api::proposals::Clock for EarlierClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        self.0
    }
}

#[tokio::test]
async fn completion_revisions_not_wall_clock_order_reopening_custody() {
    let Some(fixture) = Fixture::create().await else {
        return;
    };
    let parent = fixture.create_item(input(Uuid::new_v4(), None)).await;
    let leaf = fixture
        .create_item(input(Uuid::new_v4(), Some(parent.id)))
        .await;
    fixture.set_status(leaf.id, ItemStatus::Completed).await;
    let policy = fixture
        .command(parent.id, ItemCompletionMode::Automatic, true)
        .await;
    let written = fixture
        .repository
        .put_completion(parent.id, policy, Utc::now(), None)
        .await
        .unwrap();
    let earlier = written.completion.state.updated_at.unwrap() - Duration::hours(1);
    let old_clock_service =
        ItemService::new(fixture.repository.clone(), Arc::new(EarlierClock(earlier)));
    let current = fixture.service.get(leaf.id).await.unwrap();
    let mut reopened = replacement(&current);
    reopened.status = ItemStatus::Planned;
    old_clock_service
        .replace(leaf.id, current.revision, reopened, key())
        .await
        .unwrap();
    let after = fixture.repository.get_completion(parent.id).await.unwrap();
    assert!(after.item_revision > written.completion.item_revision);
    assert!(after.state.revision > written.completion.state.revision);
    assert_eq!(after.state.updated_at, Some(earlier));
    assert!(after.state.provenance.is_none());
    assert_eq!(
        fixture.service.get(parent.id).await.unwrap().status,
        ItemStatus::Planned
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn direct_completion_restores_blocked_custody_and_keeps_historical_receipts() {
    let Some(fixture) = Fixture::create().await else {
        return;
    };
    let root_id = Uuid::new_v4();
    let mut root = input(root_id, None);
    root.status = ItemStatus::Blocked;
    root.blocked_reason_kind = Some(BlockedReasonKind::External);
    root.blocked_reason = Some("Synthetic external prerequisite".into());
    fixture.create_item(root).await;
    let child = fixture
        .create_item(input(Uuid::new_v4(), Some(root_id)))
        .await;
    let mut draft = replacement(&child);
    draft.status = ItemStatus::Completed;
    let identity = key();
    let receipt = fixture
        .service
        .replace(child.id, child.revision, draft.clone(), identity.clone())
        .await
        .unwrap();
    let completed = fixture.service.get(root_id).await.unwrap();
    assert_eq!(completed.status, ItemStatus::Completed);
    assert_eq!(completed.blocked_reason, None);
    let state = fixture.repository.get_completion(root_id).await.unwrap();
    let provenance = state.state.provenance.unwrap();
    assert_eq!(provenance.kind, ItemCompletionProvenanceKind::Automatic);
    assert_eq!(provenance.reopen.status, ItemStatus::Blocked);
    assert_eq!(
        provenance.reopen.blocked_reason.as_deref(),
        Some("Synthetic external prerequisite")
    );
    let next = fixture
        .create_item(input(Uuid::new_v4(), Some(root_id)))
        .await;
    let reopened = fixture.service.get(root_id).await.unwrap();
    assert_eq!(reopened.status, ItemStatus::Blocked);
    assert_eq!(
        reopened.blocked_reason_kind,
        Some(BlockedReasonKind::External)
    );
    assert_eq!(
        reopened.blocked_reason.as_deref(),
        Some("Synthetic external prerequisite")
    );
    fixture
        .service
        .trash(next.id, next.revision, key())
        .await
        .unwrap();
    assert_eq!(
        fixture.service.get(root_id).await.unwrap().status,
        ItemStatus::Completed
    );
    let trashed = fixture.repository.get(next.id, true).await.unwrap();
    fixture
        .service
        .restore(next.id, trashed.revision, key())
        .await
        .unwrap();
    assert_eq!(
        fixture.service.get(root_id).await.unwrap().status,
        ItemStatus::Blocked
    );
    let replay = fixture
        .service
        .replace(child.id, child.revision, draft, identity)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item, receipt.item);
    fixture.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the ordered policy/replay history and its custody assertions together.
async fn reviewed_policy_is_revisioned_replayable_and_required_descendants_are_not_waived() {
    let Some(fixture) = Fixture::create().await else {
        return;
    };
    let grandparent = fixture.create_item(input(Uuid::new_v4(), None)).await;
    let parent = fixture
        .create_item(input(Uuid::new_v4(), Some(grandparent.id)))
        .await;
    let leaf = fixture
        .create_item(input(Uuid::new_v4(), Some(parent.id)))
        .await;
    let command = fixture
        .command(parent.id, ItemCompletionMode::Complete, true)
        .await;
    let result = fixture
        .repository
        .put_completion(parent.id, command.clone(), Utc::now(), None)
        .await
        .unwrap();
    assert!(!result.replayed);
    assert_eq!(
        fixture.service.get(parent.id).await.unwrap().status,
        ItemStatus::Completed
    );
    assert_eq!(
        fixture.service.get(grandparent.id).await.unwrap().status,
        ItemStatus::Planned
    );
    let parent_latest = fixture.service.get(parent.id).await.unwrap();
    let mut illegal = replacement(&parent_latest);
    illegal.status = ItemStatus::Planned;
    assert!(
        fixture
            .service
            .replace(parent.id, parent_latest.revision, illegal, key())
            .await
            .is_err()
    );
    fixture.set_status(leaf.id, ItemStatus::Completed).await;
    assert_eq!(
        fixture.service.get(grandparent.id).await.unwrap().status,
        ItemStatus::Completed
    );
    let keep_open = fixture
        .command(parent.id, ItemCompletionMode::KeepOpen, true)
        .await;
    fixture
        .repository
        .put_completion(parent.id, keep_open, Utc::now(), None)
        .await
        .unwrap();
    assert_eq!(
        fixture.service.get(parent.id).await.unwrap().status,
        ItemStatus::Planned
    );
    assert_eq!(
        fixture.service.get(grandparent.id).await.unwrap().status,
        ItemStatus::Planned
    );
    let replay = fixture
        .repository
        .put_completion(parent.id, command.clone(), Utc::now(), None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.completion, result.completion);
    let mut reused = command;
    reused.required_for_parent = false;
    assert_eq!(
        fixture
            .repository
            .put_completion(parent.id, reused, Utc::now(), None)
            .await
            .unwrap_err(),
        ItemCompletionError::OperationReused
    );
    // A disjoint canonical edit invalidates whole-forest review without changing target CAS.
    let stale = fixture
        .command(leaf.id, ItemCompletionMode::Automatic, false)
        .await;
    fixture.create_item(input(Uuid::new_v4(), None)).await;
    assert_eq!(
        fixture
            .repository
            .put_completion(leaf.id, stale, Utc::now(), None)
            .await
            .unwrap_err(),
        ItemCompletionError::EvidenceStale
    );
    let before = fixture.service.get(leaf.id).await.unwrap();
    let optional = fixture
        .command(leaf.id, ItemCompletionMode::Automatic, false)
        .await;
    let result = fixture
        .repository
        .put_completion(leaf.id, optional, Utc::now(), None)
        .await
        .unwrap();
    assert_eq!(result.completion.item_revision, before.revision + 1);
    assert!(!result.completion.state.required_for_parent);
    assert_eq!(
        fixture.service.get(leaf.id).await.unwrap().status,
        ItemStatus::Completed
    );
    fixture.cleanup().await;
}

async fn seed_deep_open_forest(fixture: &Fixture) {
    let now = Utc::now() - Duration::hours(1);
    let items = (1..=5_000_u128)
        .map(|index| {
            let mut input = input(
                Uuid::from_u128(index),
                (index > 1).then(|| Uuid::from_u128(index - 1)),
            );
            if index == 1 {
                input.status = ItemStatus::Blocked;
                input.blocked_reason_kind = Some(BlockedReasonKind::External);
                input.blocked_reason = Some("Synthetic deep reopening cause".into());
            }
            let mut item = Item::new(input, now).unwrap();
            item.is_executable = index == 5_000;
            item
        })
        .collect::<Vec<_>>();
    let mut tx = fixture.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO execution_state(workspace_id) VALUES($1)")
        .bind(fixture.scope.workspace_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text,0))",
    )
    .bind(fixture.scope.workspace_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    for chunk in items.chunks(300) {
        sqlx::query("INSERT INTO items(id,workspace_id,created_by_user_id,kind,status,title,timezone_name,duration_kind,duration_seconds,duration_min_seconds,duration_max_seconds,duration_source,deadline_kind,importance,urgency,revision,created_at,updated_at,blocked_reason_kind,blocked_reason) \
            SELECT (v->>'id')::uuid,$1,$2,'task',v->>'status',v->>'title','UTC','exact',60,60,60,'user','none',1,1,1,(v->>'created_at')::timestamptz,(v->>'updated_at')::timestamptz,v->>'blocked_reason_kind',v->>'blocked_reason' FROM jsonb_array_elements($3) v")
            .bind(fixture.scope.workspace_id).bind(fixture.scope.user_id)
            .bind(serde_json::to_value(chunk).unwrap()).execute(&mut *tx).await.unwrap();
    }
    for chunk in items.chunks(300) {
        sqlx::query("INSERT INTO item_hierarchy(workspace_id,parent_item_id,child_item_id) SELECT $1,(v->>'parent_id')::uuid,(v->>'id')::uuid FROM jsonb_array_elements($2) v WHERE v->>'parent_id' IS NOT NULL")
            .bind(fixture.scope.workspace_id).bind(serde_json::to_value(chunk).unwrap()).execute(&mut *tx).await.unwrap();
        sqlx::query("SELECT set_config('dayweave.item_change_group_id',$1,true)")
            .bind(Uuid::new_v4().to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item_changes(workspace_id,item_id,item_revision,change_kind,payload,change_group_id) SELECT $1,(v->>'id')::uuid,1,'upsert',v,current_setting('dayweave.item_change_group_id')::uuid FROM jsonb_array_elements($2) v")
            .bind(fixture.scope.workspace_id).bind(serde_json::to_value(chunk).unwrap()).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn five_thousand_level_real_completion_cycles_are_atomic_bounded_and_cold_bootstrappable() {
    let Some(fixture) = Fixture::create().await else {
        return;
    };
    seed_deep_open_forest(&fixture).await;
    let root = Uuid::from_u128(1);
    let leaf = Uuid::from_u128(5_000);
    // The fixture only supplies an open starting forest. Every completion and
    // reopening below is a real ordinary canonical mutation plus finalizer.
    for _ in 0..2 {
        let complete = fixture.set_status(leaf, ItemStatus::Completed).await;
        assert_eq!(complete.status, ItemStatus::Completed);
        let completed_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM items WHERE workspace_id=$1 AND status='completed'",
        )
        .bind(fixture.scope.workspace_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(completed_count, 5_000);
        let state = fixture.repository.get_completion(root).await.unwrap();
        assert_eq!(state.counts.required_descendants, 4_999);
        assert_eq!(state.counts.completed, 4_999);
        fixture.set_status(leaf, ItemStatus::Planned).await;
        let reopened = fixture.service.get(root).await.unwrap();
        assert_eq!(reopened.status, ItemStatus::Blocked);
        assert_eq!(
            reopened.blocked_reason.as_deref(),
            Some("Synthetic deep reopening cause")
        );
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM items WHERE workspace_id=$1 AND status='completed'",
        )
        .bind(fixture.scope.workspace_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }
    assert!(fixture.repository.delta_head().await.unwrap() > 20_000);
    let groups: Vec<(i64, i64)> = sqlx::query_as("SELECT count(*)::bigint,sum(octet_length(payload::text))::bigint FROM item_changes WHERE workspace_id=$1 GROUP BY change_group_id")
        .bind(fixture.scope.workspace_id).fetch_all(&fixture.pool).await.unwrap();
    assert!(
        groups
            .iter()
            .all(|(count, bytes)| *count <= 300 && *bytes <= 8 * 1024 * 1024)
    );
    let mut position = None;
    let mut active_count = 0;
    let mut pages = 0;
    let head;
    loop {
        let page = fixture
            .repository
            .bootstrap(position, Utc::now())
            .await
            .unwrap();
        assert!(page.changes.len() <= 300);
        active_count += page.changes.len();
        pages += 1;
        if let Some(next) = page.continuation {
            position = Some(next);
        } else {
            head = page.head;
            break;
        }
    }
    assert_eq!(active_count, 5_000);
    assert_eq!(pages, 17);
    assert!(
        fixture
            .repository
            .delta(head, 50)
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    let effects: i64 =
        sqlx::query_scalar("SELECT count(*) FROM item_completion_effects WHERE workspace_id=$1")
            .bind(fixture.scope.workspace_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(effects, 4 * 4_999);
    fixture.cleanup().await;
}

#[tokio::test]
async fn sql_completion_custody_rejects_partial_or_mutable_evidence() {
    let Some(fixture) = Fixture::create().await else {
        return;
    };
    let parent = fixture.create_item(input(Uuid::new_v4(), None)).await;
    let leaf = fixture
        .create_item(input(Uuid::new_v4(), Some(parent.id)))
        .await;
    fixture.set_status(leaf.id, ItemStatus::Completed).await;
    let mut state = dayweave_api::item_completion::ItemCompletionState::empty(leaf.id);
    state.revision = 1;
    state.updated_at = Some(fixture.service.get(leaf.id).await.unwrap().updated_at);
    let mut partial = fixture.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO item_completion_state(workspace_id,item_id,revision,state_json,updated_at) VALUES($1,$2,1,$3,$4)")
        .bind(fixture.scope.workspace_id).bind(leaf.id).bind(serde_json::to_value(&state).unwrap())
        .bind(state.updated_at).execute(&mut *partial).await.unwrap();
    assert!(
        partial.commit().await.is_err(),
        "a sidecar cannot commit without its exact effect"
    );
    let mut partial = fixture.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO item_completion_evaluations(workspace_id,evaluation_id,cause_kind,evidence_hash,execution_revision,effect_count,recorded_at) VALUES($1,$2,'canonical_write',$3,0,1,clock_timestamp())")
        .bind(fixture.scope.workspace_id).bind(Uuid::new_v4()).bind(format!("sha256:{}", "0".repeat(64)))
        .execute(&mut *partial).await.unwrap();
    assert!(
        partial.commit().await.is_err(),
        "an evaluation must consume its declared effects"
    );
    let keep_open = fixture
        .command(parent.id, ItemCompletionMode::KeepOpen, true)
        .await;
    fixture
        .repository
        .put_completion(parent.id, keep_open, Utc::now(), None)
        .await
        .unwrap();
    for statement in [
        "UPDATE item_completion_operations SET request_json=request_json",
        "UPDATE item_completion_effects SET reason=reason",
        "DELETE FROM item_completion_state",
        "TRUNCATE item_completion_effects",
        "UPDATE item_changes SET payload=payload WHERE (workspace_id,item_id,item_revision) IN (SELECT workspace_id,item_id,after_item_revision FROM item_completion_effects)",
    ] {
        assert!(
            sqlx::query(statement).execute(&fixture.pool).await.is_err(),
            "immutable custody guard: {statement}"
        );
    }
    // A committed header is not a mutable container for later forged effects.
    let late = sqlx::query("INSERT INTO item_completion_effects SELECT workspace_id,evaluation_id,$1,completion_revision,before_item_revision,after_item_revision,before_state_json,after_state_json,reason,recorded_at FROM item_completion_effects LIMIT 1")
        .bind(leaf.id).execute(&fixture.pool).await.unwrap_err();
    assert!(
        late.as_database_error()
            .unwrap()
            .message()
            .contains("original evaluation transaction")
    );
    let operation: Value =
        sqlx::query_scalar("SELECT result_json FROM item_completion_operations LIMIT 1")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(operation["item_id"], parent.id.to_string());
    // New sidecar custody cannot borrow a pre-existing historical 1 -> 2
    // transition, even when every identity/revision/body tuple otherwise fits.
    let mut forged = fixture.pool.begin().await.unwrap();
    let evaluation = Uuid::new_v4();
    sqlx::query("INSERT INTO item_completion_evaluations(workspace_id,evaluation_id,cause_kind,evidence_hash,execution_revision,effect_count,recorded_at) VALUES($1,$2,'canonical_write',$3,0,1,$4)")
        .bind(fixture.scope.workspace_id).bind(evaluation).bind(format!("sha256:{}", "0".repeat(64)))
        .bind(state.updated_at).execute(&mut *forged).await.unwrap();
    sqlx::query("INSERT INTO item_completion_state(workspace_id,item_id,revision,state_json,updated_at) VALUES($1,$2,1,$3,$4)")
        .bind(fixture.scope.workspace_id).bind(leaf.id).bind(serde_json::to_value(&state).unwrap())
        .bind(state.updated_at).execute(&mut *forged).await.unwrap();
    sqlx::query("INSERT INTO item_completion_effects(workspace_id,evaluation_id,item_id,completion_revision,before_item_revision,after_item_revision,before_state_json,after_state_json,reason,recorded_at) VALUES($1,$2,$3,1,1,2,$4,$5,'unchanged',$6)")
        .bind(fixture.scope.workspace_id).bind(evaluation).bind(leaf.id)
        .bind(serde_json::to_value(dayweave_api::item_completion::ItemCompletionState::empty(leaf.id)).unwrap())
        .bind(serde_json::to_value(&state).unwrap()).bind(state.updated_at)
        .execute(&mut *forged).await.unwrap();
    let rejected = forged.commit().await.unwrap_err();
    assert!(
        rejected
            .as_database_error()
            .unwrap()
            .message()
            .contains("new canonical append")
    );
    fixture.cleanup().await;
}
