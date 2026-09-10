//! Real, opt-in `PostgreSQL` qualification and read-only custody checks.
//! Each test owns a random schema in `DAYWEAVE_TEST_DATABASE_URL`; no native
//! adapter, external helper process, provider connection or owner service is exercised.
use std::{collections::BTreeMap, str::FromStr as _, sync::Arc, time::Duration as StdDuration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    AppState,
    auth::{AuthenticationError, Authenticator, Principal, PrincipalAudience, Scope},
    execution::{
        ExecutionCommand, ExecutionIdempotencyKey, ExecutionService, PauseExecution, StartExecution,
    },
    habits::{
        HabitIdempotencyKey, HabitOutcomeCommand, HabitOutcomeInput, HabitOutcomeStatus,
        HabitService,
    },
    http::router,
    item_completion::ItemCompletionMode,
    item_progress::{ItemProgressCommand, ItemProgressComponent, ItemProgressValue},
    items::{
        IdempotencyKey, Item, ItemQuery, ItemRepository as _, ItemService, ItemStatus, NewItem,
        ReplaceItem,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresExecutionRepository, PostgresHabitRepository,
        PostgresItemRepository, PostgresRoutineOccurrenceRepository,
    },
    proposals::{InMemoryProposalRepository, ProposalService, SystemClock},
    readiness::Readiness,
    routine_occurrences::{
        RoutineOccurrenceAction, RoutineOccurrenceCommand, RoutineOccurrenceSnapshot,
        RoutinePlanningWitnessError, RoutinePlanningWitnessRequest, RoutinePlanningWitnessResult,
    },
    scheduling::{
        ComposeScheduleRequest, PostgresSchedulingRepository, PublishScheduleSpec, ScheduleAccess,
        compose_canonical_schedule,
    },
};
use dayweave_core::{Minutes, OccurrenceId, RecurrencePartialProgress, ScheduleBlockKind};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions as _, Executor as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tower::ServiceExt as _;
use uuid::Uuid;

struct FixedOwnerAuth(DatabaseScope);

#[async_trait::async_trait]
impl Authenticator for FixedOwnerAuth {
    async fn authenticate(&self, _: &str) -> Result<Principal, AuthenticationError> {
        Ok(Principal {
            subject: self.0.user_id.to_string(),
            scopes: vec![Scope::ItemsRead, Scope::ScheduleSimulate],
            audience: PrincipalAudience::Device,
            workspace_id: Some(self.0.workspace_id),
            user_id: Some(self.0.user_id),
            credential_id: Some(Uuid::from_u128(97)),
            allowed_origins: Vec::new(),
        })
    }
}

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    scope: DatabaseScope,
    items: Arc<ItemService>,
    occurrences: PostgresRoutineOccurrenceRepository,
    schedules: PostgresSchedulingRepository,
    access: ScheduleAccess,
    day: DateTime<Utc>,
}

#[derive(Clone, Copy)]
struct Members {
    root: Uuid,
    leaf: Uuid,
    inbox: Uuid,
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
        let schema = format!("routine_planning_witness_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .unwrap();
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
            .unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        let scope = DatabaseScope {
            workspace_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
        };
        seed_scope(&pool, scope).await;
        let items = Arc::new(ItemService::new(
            Arc::new(PostgresItemRepository::new(pool.clone(), scope)),
            Arc::new(SystemClock),
        ));
        let occurrences = PostgresRoutineOccurrenceRepository::new(pool.clone(), scope);
        let schedules = PostgresSchedulingRepository::new(pool.clone(), scope);
        let access = ScheduleAccess {
            subject: scope.user_id.to_string(),
            include_sensitive: true,
            workspace_id: Some(scope.workspace_id),
            user_id: Some(scope.user_id),
        };
        let day = (Utc::now() + Duration::days(2))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        Some(Self {
            admin,
            pool,
            schema,
            scope,
            items,
            occurrences,
            schedules,
            access,
            day,
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

    async fn seed(&self) -> Members {
        let members = Members {
            root: Uuid::new_v4(),
            leaf: Uuid::new_v4(),
            inbox: Uuid::new_v4(),
        };
        let mut root = input(members.root, None);
        root.kind = dayweave_api::items::ItemKind::Routine;
        root.duration_seconds = None;
        root.recurrence = Some(json!({"type":"daily","times_per_day":1}));
        self.items.create(root, key()).await.unwrap();
        self.items
            .create(input(members.leaf, Some(members.root)), key())
            .await
            .unwrap();
        let mut inbox = input(members.inbox, Some(members.root));
        inbox.status = ItemStatus::Inbox;
        self.items.create(inbox, key()).await.unwrap();
        members
    }

    fn schedule(&self) -> ComposeScheduleRequest {
        serde_json::from_value(json!({"as_of":self.day,"horizon_start":self.day,
            "horizon_end":self.day+Duration::days(2),"timezone_name":"UTC",
            "availability":[{"start":self.day,"end":self.day+Duration::days(2),"contexts":[],"location":null,"energy":"deep"}],
            "fixed_blocks":[],"previous_assignments":[],"recurrence_context":{}})).unwrap()
    }

    async fn publish(&self) -> Uuid {
        let result = compose_canonical_schedule(&self.items, &self.schedules, self.schedule())
            .await
            .unwrap();
        self.schedules
            .publish(
                &self.access,
                PublishScheduleSpec {
                    idempotency_key: Uuid::new_v4(),
                    request_hash: [71; 32],
                    input_digest: digest(&result.input_digest),
                    timezone_name: "UTC".into(),
                    manual_placement_approvals: vec![],
                    result,
                    published_at: Utc::now(),
                },
            )
            .await
            .unwrap()
            .revision
            .id
    }

    async fn sources(&self) -> BTreeMap<Uuid, u64> {
        self.items
            .list(ItemQuery {
                limit: 100,
                ..ItemQuery::default()
            })
            .await
            .unwrap()
            .into_iter()
            .map(|item| (item.id, item.revision))
            .collect()
    }

    async fn request(&self) -> RoutinePlanningWitnessRequest {
        let terminal = self.occurrences.list(None, 100).await.unwrap();
        assert!(!terminal.has_more);
        RoutinePlanningWitnessRequest {
            schema_version: 1,
            schedule: self.schedule(),
            expected_source_item_revisions: self.sources().await,
            terminal_cursor: terminal.cursor,
        }
    }

    async fn qualified(&self, request: &RoutinePlanningWitnessRequest) -> Value {
        let response = self
            .schedules
            .routine_planning_witness(request)
            .await
            .unwrap();
        assert_eq!(response.schema_version, 1);
        match response.result {
            RoutinePlanningWitnessResult::Qualified { witness } => {
                serde_json::to_value(witness).unwrap()
            }
            other @ RoutinePlanningWitnessResult::RemoteRequired { .. } => {
                panic!("synthetic complete current evidence must qualify: {other:?}")
            }
        }
    }

    async fn remote(&self, request: &RoutinePlanningWitnessRequest, reason: &str) {
        let response = self
            .schedules
            .routine_planning_witness(request)
            .await
            .unwrap();
        assert_eq!(response.schema_version, 1);
        assert_eq!(
            serde_json::to_value(response.result).unwrap(),
            json!({"status":"remote_required","reason":reason})
        );
    }

    async fn first(&self) -> RoutineOccurrenceSnapshot {
        self.occurrences.list(None, 100).await.unwrap().changes[0]
            .occurrence
            .clone()
    }

    /// Existing item/list APIs initialize the empty execution mutex. Recreate
    /// its legitimate lazy-absent representation without removing any session,
    /// revision or execution intent, so witness reads cannot hide an INSERT.
    async fn remove_pristine_execution_placeholder(&self) {
        let deleted = sqlx::query("DELETE FROM execution_state state WHERE workspace_id=$1 AND revision=0 AND active_session_id IS NULL AND NOT EXISTS(SELECT 1 FROM execution_sessions session WHERE session.workspace_id=state.workspace_id)")
            .bind(self.scope.workspace_id).execute(&self.pool).await.unwrap();
        assert_eq!(deleted.rows_affected(), 1);
    }

    /// Compare entire rows, not only counts: an illicit rewrite is also a write.
    async fn storage(&self) -> BTreeMap<&'static str, Value> {
        let mut evidence = BTreeMap::new();
        for table in [
            "items",
            "item_hierarchy",
            "item_changes",
            "item_completion_state",
            "item_completion_operations",
            "item_completion_effects",
            "item_completion_evaluations",
            "item_progress",
            "item_progress_operations",
            "routine_occurrences",
            "routine_occurrence_members",
            "routine_occurrence_state",
            "routine_occurrence_changes",
            "routine_occurrence_operations",
            "routine_occurrence_publications",
            "schedule_revisions",
            "schedule_revision_details",
            "schedule_publication_requests",
            "schedule_blocks",
            "schedule_simulations",
            "schedule_deferred_placements",
            "execution_defer_assessments",
            "execution_state",
            "execution_sessions",
            "idempotency_keys",
            "habit_changes",
            "habit_occurrence_evidence",
            "habit_occurrence_outcomes",
            "habit_occurrence_versions",
            "habit_operation_receipts",
            "habit_occurrence_publications",
            "habit_pauses",
            "habit_pause_versions",
            "provider_accounts",
            "provider_sync_mappings",
            "provider_sync_cursors",
            "google_sync_collections",
            "google_calendar_projection_rejections",
        ] {
            let statement = format!(
                "SELECT COALESCE(jsonb_agg(to_jsonb(row) ORDER BY to_jsonb(row)::text),'[]'::jsonb) FROM {table} row WHERE workspace_id=$1"
            );
            let rows: Value = sqlx::query_scalar(AssertSqlSafe(statement))
                .bind(self.scope.workspace_id)
                .fetch_one(&self.pool)
                .await
                .unwrap();
            evidence.insert(table, rows);
        }
        evidence
    }

    fn execution(&self) -> ExecutionService {
        ExecutionService::new(
            Arc::new(PostgresExecutionRepository::new(
                self.pool.clone(),
                self.scope,
            )),
            self.items.clone(),
            Arc::new(SystemClock),
        )
    }

    async fn attested_start(&self, item_id: Uuid, session_id: Uuid) -> StartExecution {
        let published = self
            .schedules
            .current_native_schedule(&self.access)
            .await
            .unwrap();
        let block = published.schedule["plan"]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["item_id"] == json!(item_id))
            .unwrap();
        StartExecution {
            session_id,
            item_id,
            item_revision: self.items.get(item_id).await.unwrap().revision,
            occurrence_id: Some(Uuid::parse_str(block["occurrence_id"].as_str().unwrap()).unwrap()),
            session_index: u16::try_from(block["session_index"].as_u64().unwrap()).unwrap(),
            planned_block_id: Some(Uuid::parse_str(block["id"].as_str().unwrap()).unwrap()),
            device_id: Uuid::new_v4(),
        }
    }
}

async fn seed_scope(pool: &PgPool, scope: DatabaseScope) {
    sqlx::query(
        "INSERT INTO users(id,auth_subject,display_name) VALUES($1,$2,'Synthetic witness owner')",
    )
    .bind(scope.user_id)
    .bind(scope.user_id.to_string())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspaces(id,owner_user_id,slug,name) VALUES($1,$2,$3,'Synthetic witness workspace')")
        .bind(scope.workspace_id).bind(scope.user_id).bind(scope.workspace_id.to_string()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,'owner')")
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(pool)
        .await
        .unwrap();
}

fn input(id: Uuid, parent: Option<Uuid>) -> NewItem {
    serde_json::from_value(json!({"id":id,"is_sensitive":false,"kind":"task","status":"planned",
        "title":"Synthetic planning witness member","notes":null,"timezone_name":"UTC","duration_seconds":60,
        "deadline_at":null,"earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":1,"urgency":1,"parent_id":parent,"sibling_order":0})).unwrap()
}

fn key() -> IdempotencyKey {
    IdempotencyKey {
        key: Uuid::new_v4().to_string(),
        fingerprint: [73; 32],
    }
}

fn execution_key() -> ExecutionIdempotencyKey {
    ExecutionIdempotencyKey {
        key: Uuid::new_v4().to_string(),
        fingerprint: [79; 32],
    }
}

fn digest(value: &str) -> [u8; 32] {
    let value = value.strip_prefix("sha256:").unwrap();
    std::array::from_fn(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
}

fn replacement(item: &Item) -> ReplaceItem {
    let mut value = serde_json::to_value(item).unwrap();
    for field in [
        "id",
        "is_executable",
        "revision",
        "created_at",
        "updated_at",
        "completed_at",
        "deleted_at",
    ] {
        value.as_object_mut().unwrap().remove(field);
    }
    serde_json::from_value(value).unwrap()
}

fn command(
    snapshot: &RoutineOccurrenceSnapshot,
    member: Uuid,
    action: RoutineOccurrenceAction,
) -> RoutineOccurrenceCommand {
    RoutineOccurrenceCommand {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        expected_instance_revision: snapshot.aggregate.revision,
        expected_member_revision: snapshot
            .aggregate
            .members
            .iter()
            .find(|value| value.item_id == member)
            .unwrap()
            .revision,
        expected_evidence_hash: snapshot.evidence_hash.clone(),
        action,
    }
}

fn instance(witness: &Value, occurrence: Uuid) -> &Value {
    witness["occurrence_lifecycle"]["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["occurrence_id"] == json!(occurrence))
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep one complete source/lifecycle and read-only storage capture together.
async fn qualified_witness_is_complete_current_source_private_and_read_only() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    let publication = f.publish().await;
    let request = f.request().await;
    f.remove_pristine_execution_placeholder().await;
    let before = f.storage().await;
    assert_eq!(before["execution_state"], json!([]));
    let witness = f.qualified(&request).await;
    assert_eq!(
        witness,
        f.qualified(&request).await,
        "an identical read has deterministic fingerprints"
    );
    assert_eq!(
        f.storage().await,
        before,
        "qualification cannot write any authority or immutable receipt"
    );
    assert_eq!(witness["workspace_id"], json!(f.scope.workspace_id));
    assert_eq!(witness["user_id"], json!(f.scope.user_id));
    assert_eq!(
        witness["source_item_revisions"],
        json!(request.expected_source_item_revisions)
    );
    assert_eq!(witness["terminal_cursor"], json!(request.terminal_cursor));
    assert_eq!(
        witness["published_schedule_revision_id"],
        json!(publication)
    );
    assert_eq!(witness["execution_snapshot_revision"], json!(0));
    assert_eq!(witness["habit_change_head"], json!(0));
    assert_eq!(witness["schedule"]["as_of"], json!(f.day));
    assert_eq!(witness["schedule"]["horizon_start"], json!(f.day));
    for field in [
        "request_fingerprint",
        "witness_fingerprint",
        "calendar_projection_fingerprint",
    ] {
        assert!(!witness[field].as_str().unwrap().is_empty());
    }
    assert!(
        witness["local_input_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("local-sha256:")
    );
    assert_ne!(
        witness["local_input_fingerprint"],
        witness["request_fingerprint"]
    );
    assert_ne!(
        witness["request_fingerprint"],
        witness["witness_fingerprint"]
    );
    let ledger = f.occurrences.list(None, 100).await.unwrap();
    let current = &witness["occurrence_lifecycle"];
    assert_eq!(
        current["snapshot_revision"],
        json!(ledger.changes.last().unwrap().sequence)
    );
    assert_eq!(current["instances"].as_array().unwrap().len(), 2);
    for change in &ledger.changes {
        let tree = instance(&witness, change.occurrence.aggregate.manifest.occurrence_id);
        assert_eq!(tree["root_item_id"], json!(members.root));
        assert_eq!(
            tree["identity"],
            json!(change.occurrence.aggregate.manifest.identity)
        );
        let captured = tree["members"].as_array().unwrap();
        assert_eq!(captured.len(), 3);
        for (id, parent) in [
            (members.root, None),
            (members.leaf, Some(members.root)),
            (members.inbox, Some(members.root)),
        ] {
            let member = captured
                .iter()
                .find(|value| value["item_id"] == json!(id))
                .unwrap();
            assert_eq!(member["parent_id"], json!(parent));
            assert_eq!(
                member["source_revision"],
                json!(request.expected_source_item_revisions[&id])
            );
            assert_eq!(member["status"], json!("not_started"));
        }
    }
    let preview = compose_canonical_schedule(&f.items, &f.schedules, f.schedule())
        .await
        .unwrap();
    assert!(
        !preview
            .plan
            .blocks
            .iter()
            .any(|block| block.item_id.map(|id| id.0) == Some(members.inbox))
    );
    f.cleanup().await;
}

#[tokio::test]
async fn harmless_source_change_joins_current_revision_not_first_publication_source() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let old_request = f.request().await;
    let old_witness = f.qualified(&old_request).await;
    let original = f.first().await;
    let item = f.items.get(members.inbox).await.unwrap();
    let mut renamed = replacement(&item);
    renamed.title = "Synthetic harmless current Inbox title".into();
    let renamed = f
        .items
        .replace(item.id, item.revision, renamed, key())
        .await
        .unwrap()
        .item;
    assert_eq!(
        f.schedules
            .routine_planning_witness(&old_request)
            .await
            .unwrap_err(),
        RoutinePlanningWitnessError::SourceChanged
    );
    let request = f.request().await;
    assert_eq!(request.terminal_cursor, old_request.terminal_cursor);
    let before = f.storage().await;
    let witness = f.qualified(&request).await;
    assert_eq!(before, f.storage().await);
    let current = instance(&witness, original.aggregate.manifest.occurrence_id)["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["item_id"] == json!(members.inbox))
        .unwrap();
    assert_eq!(current["source_revision"], json!(renamed.revision));
    assert_ne!(
        witness["witness_fingerprint"],
        old_witness["witness_fingerprint"]
    );
    assert_ne!(
        witness["local_input_fingerprint"],
        old_witness["local_input_fingerprint"]
    );
    assert_eq!(
        f.first().await.aggregate,
        original.aggregate,
        "read qualification cannot rewrite the immutable source capture"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn completed_member_overlay_is_exactly_instance_scoped_and_never_template_status() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let original_sources = f.sources().await;
    let selected = f.first().await;
    let instance_id = selected.aggregate.manifest.id;
    let outcome = command(
        &selected,
        members.leaf,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Completed,
        },
    );
    let receipt = f
        .occurrences
        .put(instance_id, members.leaf, outcome.clone(), None)
        .await
        .unwrap();
    let request = f.request().await;
    let before = f.storage().await;
    let witness = f.qualified(&request).await;
    let trees = witness["occurrence_lifecycle"]["instances"]
        .as_array()
        .unwrap();
    assert_eq!(trees.len(), 2);
    for tree in trees {
        let expected = if tree["occurrence_id"] == json!(selected.aggregate.manifest.occurrence_id)
        {
            "completed"
        } else {
            "not_started"
        };
        let leaf = tree["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|member| member["item_id"] == json!(members.leaf))
            .unwrap();
        assert_eq!(leaf["status"], json!(expected));
    }
    assert_eq!(before, f.storage().await);
    assert_eq!(f.sources().await, original_sources);
    assert_eq!(
        f.items.get(members.leaf).await.unwrap().status,
        ItemStatus::Planned
    );
    let replay = f
        .occurrences
        .put(instance_id, members.leaf, outcome, None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.occurrence, receipt.occurrence);
    assert_eq!(before, f.storage().await);
    f.cleanup().await;
}

#[tokio::test]
async fn missing_generated_instances_require_real_publication_without_admitting_or_creating_execution_state()
 {
    let Some(f) = Fixture::create().await else {
        return;
    };
    f.seed().await;
    let request = f.request().await;
    f.remove_pristine_execution_placeholder().await;
    let before = f.storage().await;
    assert_eq!(before["execution_state"], json!([]));
    assert_eq!(before["routine_occurrences"], json!([]));
    f.remote(&request, "first_publication_required").await;
    assert_eq!(f.storage().await, before);
    f.publish().await;
    let mut request = f.request().await;
    request.schedule.horizon_end += Duration::days(1);
    let before = f.storage().await;
    f.remote(&request, "first_publication_required").await;
    assert_eq!(
        f.storage().await,
        before,
        "an incomplete horizon cannot silently admit its missing day"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn full_source_map_and_terminal_cursor_are_mandatory_even_for_remote_fallback() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let before = f.storage().await;
    for defect in 0..3 {
        let mut wrong = request.clone();
        match defect {
            0 => {
                wrong.expected_source_item_revisions.remove(&members.inbox);
            }
            1 => {
                wrong
                    .expected_source_item_revisions
                    .insert(Uuid::new_v4(), 1);
            }
            _ => {
                *wrong
                    .expected_source_item_revisions
                    .get_mut(&members.leaf)
                    .unwrap() += 1;
            }
        }
        wrong.schedule.horizon_end += Duration::days(1);
        assert_eq!(
            f.schedules
                .routine_planning_witness(&wrong)
                .await
                .unwrap_err(),
            RoutinePlanningWitnessError::SourceChanged
        );
    }
    let intermediate = f.occurrences.list(None, 1).await.unwrap();
    assert!(intermediate.has_more);
    let foreign = DatabaseScope {
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
    };
    seed_scope(&f.pool, foreign).await;
    let foreign_cursor = PostgresRoutineOccurrenceRepository::new(f.pool.clone(), foreign)
        .list(None, 100)
        .await
        .unwrap()
        .cursor;
    for cursor in [
        intermediate.cursor,
        foreign_cursor,
        format!("{}x", request.terminal_cursor),
    ] {
        let mut wrong = request.clone();
        wrong.terminal_cursor = cursor;
        assert_eq!(
            f.schedules
                .routine_planning_witness(&wrong)
                .await
                .unwrap_err(),
            RoutinePlanningWitnessError::CursorChanged
        );
    }
    assert_eq!(f.storage().await, before);
    f.cleanup().await;
}

#[tokio::test]
async fn status_preserving_policy_change_requires_new_terminal_capture_and_fingerprint() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let old = f.qualified(&request).await;
    let reviewed = f.first().await;
    let instance_id = reviewed.aggregate.manifest.id;
    let action = command(
        &reviewed,
        members.root,
        RoutineOccurrenceAction::SetPolicy {
            required_for_parent: true,
            mode: ItemCompletionMode::KeepOpen,
        },
    );
    let receipt = f
        .occurrences
        .put(instance_id, members.root, action.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        reviewed
            .aggregate
            .members
            .iter()
            .map(|member| member.status)
            .collect::<Vec<_>>(),
        receipt
            .occurrence
            .aggregate
            .members
            .iter()
            .map(|member| member.status)
            .collect::<Vec<_>>()
    );
    assert_eq!(f.sources().await, request.expected_source_item_revisions);
    let before = f.storage().await;
    assert_eq!(
        f.schedules
            .routine_planning_witness(&request)
            .await
            .unwrap_err(),
        RoutinePlanningWitnessError::CursorChanged
    );
    let current = f.request().await;
    assert_ne!(current.terminal_cursor, request.terminal_cursor);
    let new = f.qualified(&current).await;
    assert_eq!(
        old["occurrence_lifecycle"]["instances"],
        new["occurrence_lifecycle"]["instances"]
    );
    assert_ne!(
        old["occurrence_lifecycle"]["snapshot_revision"],
        new["occurrence_lifecycle"]["snapshot_revision"]
    );
    assert_ne!(old["witness_fingerprint"], new["witness_fingerprint"]);
    assert_ne!(
        old["local_input_fingerprint"],
        new["local_input_fingerprint"]
    );
    assert_eq!(before, f.storage().await);
    let replay = f
        .occurrences
        .put(instance_id, members.root, action, None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.occurrence, receipt.occurrence);
    assert_eq!(
        before,
        f.storage().await,
        "witness reads preserve exact immutable receipt replay"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn changed_semantic_membership_requires_remote_without_rebinding_history() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let original = f.first().await;
    f.items
        .create(input(Uuid::new_v4(), Some(members.root)), key())
        .await
        .unwrap();
    let request = f.request().await;
    let before = f.storage().await;
    f.remote(&request, "source_ineligible").await;
    assert_eq!(before, f.storage().await);
    assert_eq!(f.first().await.aggregate, original.aggregate);
    f.cleanup().await;
}

#[tokio::test]
async fn an_empty_requested_horizon_retains_the_positive_global_occurrence_head() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    let root = f.items.get(members.root).await.unwrap();
    let mut finite = replacement(&root);
    finite.recurrence = Some(json!({"type":"custom",
        "rrule":format!("FREQ=DAILY;UNTIL={}", (f.day+Duration::days(1)).format("%Y%m%d"))}));
    f.items
        .replace(root.id, root.revision, finite, key())
        .await
        .unwrap();
    f.publish().await;
    let mut request = f.request().await;
    let populated = f.qualified(&request).await;
    assert_eq!(
        populated["occurrence_lifecycle"]["instances"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let head = populated["occurrence_lifecycle"]["snapshot_revision"]
        .as_u64()
        .unwrap();
    assert!(head > 0);
    request.schedule.horizon_start += Duration::days(3);
    request.schedule.horizon_end += Duration::days(3);
    for availability in &mut request.schedule.availability {
        availability.start += Duration::days(3);
        availability.end += Duration::days(3);
    }
    let before = f.storage().await;
    let empty = f.qualified(&request).await;
    assert_eq!(
        empty["occurrence_lifecycle"],
        json!({"snapshot_revision":head,"instances":[]})
    );
    assert_eq!(
        empty["source_item_revisions"],
        populated["source_item_revisions"]
    );
    assert_eq!(empty["terminal_cursor"], populated["terminal_cursor"]);
    assert_ne!(
        empty["request_fingerprint"],
        populated["request_fingerprint"]
    );
    assert_ne!(
        empty["witness_fingerprint"],
        populated["witness_fingerprint"]
    );
    assert_ne!(
        empty["local_input_fingerprint"],
        populated["local_input_fingerprint"]
    );
    assert_eq!(before, f.storage().await);
    f.cleanup().await;
}

#[tokio::test]
async fn execution_work_units_cannot_be_omitted_from_a_qualified_witness() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let execution = f.execution();
    let session_id = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(f.attested_start(members.leaf, session_id).await),
            execution_key(),
        )
        .await
        .unwrap();
    execution
        .command(
            1,
            ExecutionCommand::Pause(PauseExecution {
                session_id,
                duration_seconds: None,
                pause_until: None,
                reason: Some("Synthetic witness pause".into()),
            }),
            execution_key(),
        )
        .await
        .unwrap();
    let request = f.request().await;
    let before = f.storage().await;
    f.remote(&request, "execution_evidence_required").await;
    assert_eq!(before, f.storage().await);
    f.cleanup().await;
}

#[tokio::test]
async fn owner_scope_and_account_lifecycle_are_checked_before_disclosing_a_witness() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let foreign = DatabaseScope {
        workspace_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
    };
    seed_scope(&f.pool, foreign).await;
    let wrong_owner = PostgresSchedulingRepository::new(
        f.pool.clone(),
        DatabaseScope {
            user_id: foreign.user_id,
            ..f.scope
        },
    );
    let before = f.storage().await;
    assert_eq!(
        wrong_owner
            .routine_planning_witness(&request)
            .await
            .unwrap_err(),
        RoutinePlanningWitnessError::Unavailable
    );
    assert_eq!(f.storage().await, before);
    sqlx::query("UPDATE users SET trashed_at=clock_timestamp() WHERE id=$1")
        .bind(f.scope.user_id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.schedules
            .routine_planning_witness(&request)
            .await
            .unwrap_err(),
        RoutinePlanningWitnessError::Unavailable
    );
    assert_eq!(
        f.storage().await,
        before,
        "closed account access cannot rewrite any retained evidence"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn capture_waits_for_execution_before_canonical_lock_and_observes_a_competing_start() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let start_command = f.attested_start(members.leaf, Uuid::new_v4()).await;
    let mut blocker = f.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let row: Uuid = sqlx::query_scalar(
        "SELECT workspace_id FROM execution_state WHERE workspace_id=$1 FOR UPDATE",
    )
    .bind(f.scope.workspace_id)
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    assert_eq!(row, f.scope.workspace_id);
    let execution = f.execution();
    let start = tokio::spawn(async move {
        execution
            .command(0, ExecutionCommand::Start(start_command), execution_key())
            .await
    });
    wait_for_blocked_queries(
        &f.pool,
        blocker_pid,
        1,
        "Start behind existing execution row",
    )
    .await;
    let repository = f.schedules.clone();
    let witness = tokio::spawn(async move { repository.routine_planning_witness(&request).await });
    wait_for_blocked_queries(
        &f.pool,
        blocker_pid,
        2,
        "witness behind existing execution row",
    )
    .await;
    let canonical_available: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('dayweave.items.v1:' || $1::text,0))",
    )
    .bind(f.scope.workspace_id)
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    assert!(
        canonical_available,
        "execution waiters must not hold canonical space in reverse order"
    );
    blocker.commit().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), start)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let response = tokio::time::timeout(StdDuration::from_secs(10), witness)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(response.result).unwrap(),
        json!({"status":"remote_required","reason":"execution_evidence_required"})
    );
    assert_eq!(f.execution().snapshot().await.unwrap().revision, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn absent_execution_row_does_not_let_capture_miss_a_first_start_waiting_on_item_rows() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let start_command = f.attested_start(members.leaf, Uuid::new_v4()).await;
    f.remove_pristine_execution_placeholder().await;
    assert_eq!(f.storage().await["execution_state"], json!([]));
    let active_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM items WHERE workspace_id=$1 AND trashed_at IS NULL ORDER BY id",
    )
    .bind(f.scope.workspace_id)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert!(
        active_ids.len() >= 2,
        "Start must own a lower item before reaching the barrier"
    );
    let last_item_id = *active_ids.last().unwrap();
    let mut blocker = f.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM items WHERE workspace_id=$1 AND id=$2 FOR SHARE")
        .bind(f.scope.workspace_id)
        .bind(last_item_id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let execution = f.execution();
    let start = tokio::spawn(async move {
        execution
            .command(0, ExecutionCommand::Start(start_command), execution_key())
            .await
    });
    wait_for_blocked_queries(
        &f.pool,
        blocker_pid,
        1,
        "first Start at greatest active item",
    )
    .await;
    let start_pid: i32 =
        sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid))")
            .bind(blocker_pid)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    // Start owns its uncommitted first execution row, but cannot create its
    // session until the greatest-item barrier is released. Its ordered UPDATE
    // scan already owns at least one lower item, so the later witness SHARE
    // scan must wait on Start. Blocking a random first item would instead let
    // compatible witness SHAREs validly linearize before the pending Start.
    let repository = f.schedules.clone();
    let witness = tokio::spawn(async move { repository.routine_planning_witness(&request).await });
    wait_for_blocked_queries(
        &f.pool,
        start_pid,
        1,
        "witness behind Start's lower item UPDATE",
    )
    .await;
    blocker.commit().await.unwrap();
    tokio::time::timeout(StdDuration::from_secs(10), start)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let response = tokio::time::timeout(StdDuration::from_secs(10), witness)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(response.result).unwrap(),
        json!({"status":"remote_required","reason":"execution_evidence_required"})
    );
    assert_eq!(f.execution().snapshot().await.unwrap().revision, 1);
    f.cleanup().await;
}

#[tokio::test]
async fn absent_execution_row_allows_capture_to_finish_before_a_later_first_start() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let expected = f.qualified(&request).await;
    let start_command = f.attested_start(members.leaf, Uuid::new_v4()).await;
    f.remove_pristine_execution_placeholder().await;
    assert_eq!(f.storage().await["execution_state"], json!([]));
    let mut barrier = f.pool.begin().await.unwrap();
    let barrier_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||$1::text,0))")
        .bind(f.scope.workspace_id).execute(&mut *barrier).await.unwrap();
    let schedules = f.schedules.clone();
    let capture_request = request.clone();
    let witness =
        tokio::spawn(async move { schedules.routine_planning_witness(&capture_request).await });
    wait_for_blocked_queries(
        &f.pool,
        barrier_pid,
        1,
        "empty-state witness owns source SHAREs",
    )
    .await;
    let witness_pid: i32 =
        sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid))")
            .bind(barrier_pid)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let execution = f.execution();
    let start = tokio::spawn(async move {
        execution
            .command(0, ExecutionCommand::Start(start_command), execution_key())
            .await
    });
    wait_for_blocked_queries(
        &f.pool,
        witness_pid,
        1,
        "later Start behind witness source SHAREs",
    )
    .await;
    barrier.commit().await.unwrap();
    let response = tokio::time::timeout(StdDuration::from_secs(10), witness)
        .await
        .expect("earlier witness must finish while later Start waits on its source rows")
        .unwrap()
        .unwrap();
    let RoutinePlanningWitnessResult::Qualified { witness } = response.result else {
        panic!("the earlier empty-execution capture must qualify");
    };
    assert_eq!(witness.execution_snapshot_revision, 0);
    assert_eq!(serde_json::to_value(witness).unwrap(), expected);
    tokio::time::timeout(StdDuration::from_secs(10), start)
        .await
        .expect("later Start must finish after witness releases its source rows")
        .unwrap()
        .unwrap();
    assert_eq!(f.execution().snapshot().await.unwrap().revision, 1);
    let before = f.storage().await;
    f.remote(&request, "execution_evidence_required").await;
    assert_eq!(f.storage().await, before);
    f.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One controlled real-writer race includes exact receipt and unchanged-authority checks.
async fn progress_owner_share_and_late_witness_admission_do_not_form_a_lock_cycle() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let request = f.request().await;
    let expected = f.qualified(&request).await;
    let before = f.storage().await;
    let item_repository = Arc::new(PostgresItemRepository::new(f.pool.clone(), f.scope));
    let reviewed = item_repository.get_progress(members.leaf).await.unwrap();
    let command = ItemProgressCommand {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        expected_item_revision: reviewed.item_revision,
        expected_progress_revision: reviewed.revision,
        components: vec![ItemProgressComponent {
            id: Uuid::new_v4(),
            name: "Synthetic independently recorded progress".into(),
            value: ItemProgressValue::Percentage { basis_points: 2500 },
        }],
    };
    let mut barrier = f.pool.begin().await.unwrap();
    let barrier_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||$1::text,0))")
        .bind(f.scope.workspace_id).execute(&mut *barrier).await.unwrap();
    let schedules = f.schedules.clone();
    let witness = tokio::spawn(async move { schedules.routine_planning_witness(&request).await });
    wait_for_blocked_queries(&f.pool, barrier_pid, 1, "witness at occurrence barrier").await;
    // The witness now owns canonical/items, but has not reached late owner
    // admission. The real progress writer takes its early owner FOR SHARE and
    // waits for canonical. A late owner UPDATE would complete a deadlock cycle.
    let progress_repository = item_repository.clone();
    let progress_command = command.clone();
    let progress = tokio::spawn(async move {
        progress_repository
            .put_progress(members.leaf, progress_command, Utc::now(), None)
            .await
    });
    wait_for_blocked_queries(
        &f.pool,
        barrier_pid,
        2,
        "progress behind witness canonical lock",
    )
    .await;
    barrier.commit().await.unwrap();
    let response = tokio::time::timeout(StdDuration::from_secs(10), witness)
        .await
        .expect("witness must not deadlock with the progress owner's shared admission")
        .unwrap()
        .unwrap();
    let RoutinePlanningWitnessResult::Qualified { witness } = response.result else {
        panic!("independent progress cannot deny an otherwise qualified planning witness");
    };
    assert_eq!(serde_json::to_value(witness).unwrap(), expected);
    let applied = tokio::time::timeout(StdDuration::from_secs(10), progress)
        .await
        .expect("progress must complete after the witness releases canonical")
        .unwrap()
        .unwrap();
    assert!(!applied.replayed);
    assert_eq!(applied.progress.revision, 1);
    assert_eq!(applied.progress.components, command.components);
    assert_eq!(
        item_repository.get_progress(members.leaf).await.unwrap(),
        applied.progress
    );
    let replay = item_repository
        .put_progress(members.leaf, command, Utc::now(), None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.progress, applied.progress);
    let mut after = f.storage().await;
    let mut untouched = before;
    for table in ["item_progress", "item_progress_operations"] {
        assert_eq!(after.remove(table).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(untouched.remove(table).unwrap(), json!([]));
    }
    assert_eq!(
        after, untouched,
        "only the independent progress write may change retained storage"
    );
    f.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One real-router sequence checks private wire/status contracts against unchanged PostgreSQL custody.
async fn real_router_qualifies_and_rejects_against_the_configured_private_postgres_scope() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    let inbox = f.items.get(members.inbox).await.unwrap();
    let mut sensitive = replacement(&inbox);
    sensitive.is_sensitive = true;
    sensitive.title = "Synthetic private witness Inbox".into();
    f.items
        .replace(inbox.id, inbox.revision, sensitive, key())
        .await
        .unwrap();
    f.publish().await;
    let request = f.request().await;
    let expected = f.qualified(&request).await;
    let before = f.storage().await;
    // This intentionally uses synthetic authentication, not Device enrollment.
    let app = router(
        AppState::new(
            Arc::new(ProposalService::new(
                Arc::new(InMemoryProposalRepository::default()),
                Arc::new(SystemClock),
                StdDuration::from_hours(24),
            )),
            Arc::new(FixedOwnerAuth(f.scope)),
            Readiness::default(),
        )
        .with_postgres_scheduling(Arc::new(f.schedules.clone()), Arc::new(Vec::new())),
    );
    let (status, body) = post_witness(&app, &request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"schema_version":1,"result":{"status":"qualified","witness":expected}})
    );
    assert_eq!(
        body["result"]["witness"]["workspace_id"],
        json!(f.scope.workspace_id)
    );
    assert_eq!(body["result"]["witness"]["user_id"], json!(f.scope.user_id));
    assert!(
        body["result"]["witness"]["source_item_revisions"]
            .get(members.inbox.to_string())
            .is_some()
    );
    let mut missing_day = request.clone();
    missing_day.schedule.horizon_end += Duration::days(1);
    let (status, body) = post_witness(&app, &missing_day).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"schema_version":1,"result":{"status":"remote_required","reason":"first_publication_required"}})
    );
    assert!(body["result"].get("witness").is_none());
    let mut stale = request.clone();
    *stale
        .expected_source_item_revisions
        .get_mut(&members.inbox)
        .unwrap() += 1;
    let (status, body) = post_witness(&app, &stale).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "routine_planning_source_changed");
    assert!(body.get("result").is_none());
    assert!(!body.to_string().contains(&members.inbox.to_string()));
    assert!(!body.to_string().contains("Synthetic private witness Inbox"));
    let mut invalid_cursor = request;
    invalid_cursor.terminal_cursor.push('x');
    let (status, body) = post_witness(&app, &invalid_cursor).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "routine_planning_cursor_changed");
    assert_eq!(
        f.storage().await,
        before,
        "HTTP qualification and errors are read-only"
    );
    f.cleanup().await;
}

async fn post_witness(
    app: &Router,
    request: &RoutinePlanningWitnessRequest,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/routine-occurrences/planning-witness")
                .header(header::AUTHORIZATION, "Bearer synthetic-witness-fixed-auth")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    assert_eq!(response.headers()[header::PRAGMA], "no-cache");
    assert!(!response.headers().contains_key("idempotency-replayed"));
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Real Habit admission/outcome and independent routine authority are compared through one exact request.
async fn real_habit_outcome_hydrates_normalized_input_without_accepting_spoofed_completion() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    let habit_id = Uuid::new_v4();
    let mut habit = input(habit_id, None);
    habit.kind = dayweave_api::items::ItemKind::Habit;
    habit.title = "Synthetic authoritative witness habit".into();
    habit.recurrence = Some(json!({"type":"daily","times_per_day":1}));
    f.items.create(habit, key()).await.unwrap();
    f.publish().await;
    let habits = HabitService::new(
        Arc::new(PostgresHabitRepository::new(f.pool.clone(), f.scope)),
        f.items.clone(),
        Arc::new(SystemClock),
    );
    let page = habits
        .list_occurrences(
            habit_id,
            f.day.date_naive(),
            (f.day + Duration::days(1)).date_naive(),
            None,
            100,
        )
        .await
        .unwrap();
    assert!(!page.has_more);
    assert_eq!(page.occurrences.len(), 2);
    let selected = &page.occurrences[0];
    let other = &page.occurrences[1];
    assert!(selected.outcome.is_none());
    let selected_planner_id = OccurrenceId(selected.evidence.planner_occurrence_id);
    let other_planner_id = OccurrenceId(other.evidence.planner_occurrence_id);
    let ordinary = f.first().await;
    let mut request = f.request().await;
    let clean = f.qualified(&request).await;
    request
        .schedule
        .recurrence_context
        .completed_occurrence_ids
        .extend([
            selected_planner_id,
            other_planner_id,
            OccurrenceId(ordinary.aggregate.manifest.occurrence_id),
        ]);
    request.schedule.recurrence_context.partial_progress.insert(
        OccurrenceId(ordinary.aggregate.manifest.occurrence_id),
        RecurrencePartialProgress {
            progress_basis_points: 9_999,
            expected_duration_minutes: Minutes(60),
            remaining_duration_minutes: Some(Minutes(1)),
        },
    );
    let before = f.storage().await;
    let spoofed = f.qualified(&request).await;
    assert_eq!(f.storage().await, before);
    let normalized: ComposeScheduleRequest =
        serde_json::from_value(spoofed["schedule"].clone()).unwrap();
    assert!(
        normalized
            .recurrence_context
            .completed_occurrence_ids
            .is_empty()
    );
    assert!(normalized.recurrence_context.partial_progress.is_empty());
    assert_eq!(
        spoofed["local_input_fingerprint"],
        clean["local_input_fingerprint"]
    );
    assert_ne!(spoofed["request_fingerprint"], clean["request_fingerprint"]);
    let operation = HabitOutcomeCommand {
        operation_id: Uuid::new_v4(),
        expected_revision: 0,
        outcome: HabitOutcomeInput {
            status: HabitOutcomeStatus::Completed,
            progress_basis_points: 10_000,
            quantity: None,
            unit: None,
            actual_seconds: Some(60),
            note: Some("Synthetic private Habit outcome".into()),
            occurred_at: DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap(),
        },
    };
    let operation_key = HabitIdempotencyKey {
        key: Uuid::new_v4().to_string(),
        actor_session_id: None,
    };
    let outcome = habits
        .put_outcome(
            habit_id,
            selected.evidence.id,
            operation.clone(),
            operation_key.clone(),
        )
        .await
        .unwrap();
    assert!(!outcome.replayed);
    assert_eq!(f.sources().await, request.expected_source_item_revisions);
    assert_eq!(f.request().await.terminal_cursor, request.terminal_cursor);
    let before = f.storage().await;
    let completed = f.qualified(&request).await;
    assert_eq!(f.storage().await, before);
    let normalized: ComposeScheduleRequest =
        serde_json::from_value(completed["schedule"].clone()).unwrap();
    assert_eq!(
        normalized.recurrence_context.completed_occurrence_ids,
        std::collections::BTreeSet::from([selected_planner_id])
    );
    assert!(normalized.recurrence_context.partial_progress.is_empty());
    assert!(
        completed["habit_change_head"].as_u64().unwrap()
            > spoofed["habit_change_head"].as_u64().unwrap()
    );
    assert_ne!(
        completed["witness_fingerprint"],
        spoofed["witness_fingerprint"]
    );
    assert_ne!(
        completed["local_input_fingerprint"],
        spoofed["local_input_fingerprint"]
    );
    assert_eq!(
        completed["occurrence_lifecycle"],
        clean["occurrence_lifecycle"]
    );
    let routine_tree = instance(&completed, ordinary.aggregate.manifest.occurrence_id);
    assert_eq!(routine_tree["root_item_id"], json!(members.root));
    assert_eq!(routine_tree["members"].as_array().unwrap().len(), 3);
    assert!(
        routine_tree["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|member| member["item_id"] == json!(members.inbox))
    );
    let preview = compose_canonical_schedule(&f.items, &f.schedules, normalized)
        .await
        .unwrap();
    assert!(
        !preview
            .plan
            .blocks
            .iter()
            .any(|block| block.occurrence_id == Some(selected_planner_id))
    );
    assert!(
        preview
            .plan
            .blocks
            .iter()
            .any(|block| block.occurrence_id == Some(other_planner_id))
    );
    assert!(
        preview
            .plan
            .blocks
            .iter()
            .any(|block| block.item_id.map(|id| id.0) == Some(members.leaf))
    );
    let replay = habits
        .put_outcome(habit_id, selected.evidence.id, operation, operation_key)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.value, outcome.value);
    assert_eq!(before, f.storage().await);
    f.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One configured projection fixture follows completeness, freshness, generation, and real capacity together.
async fn configured_calendar_projection_qualifies_capacity_and_binds_generation_without_provider_io()
 {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let members = f.seed().await;
    f.publish().await;
    let ordinary = f.first().await;
    let event_id = Uuid::new_v4();
    let mut event = input(event_id, None);
    event.kind = dayweave_api::items::ItemKind::Event;
    event.title = "Synthetic private Calendar capacity".into();
    event.is_sensitive = true;
    event.duration_seconds = None;
    event.flexible_constraints = json!({"calendar_event":{
        "start":f.day,"end":f.day+Duration::hours(1),"immutable":true,"all_day":false,"source_calendar_id":null}});
    f.items.create(event, key()).await.unwrap();
    let (account, collection) = seed_calendar_projection(&f, event_id).await;
    let request = f.request().await;
    let before = f.storage().await;
    f.remote(&request, "calendar_projection_incomplete").await;
    assert_eq!(before, f.storage().await);
    let full_start = f.day - Duration::days(1);
    let full_end = f.day + Duration::days(3);
    for (generation, start, refreshed) in [
        (1, f.day + Duration::minutes(1), Utc::now()),
        (2, full_start, Utc::now() - Duration::minutes(31)),
        (3, full_start, Utc::now() + Duration::minutes(1)),
    ] {
        set_calendar_projection(&f.pool, collection, generation, start, full_end, refreshed).await;
        let before = f.storage().await;
        f.remote(&request, "calendar_projection_incomplete").await;
        assert_eq!(before, f.storage().await);
    }
    let final_refreshed_at = Utc::now();
    set_calendar_projection(
        &f.pool,
        collection,
        4,
        full_start,
        full_end,
        final_refreshed_at,
    )
    .await;
    let before = f.storage().await;
    let qualified = f.qualified(&request).await;
    assert_eq!(before, f.storage().await);
    assert_eq!(
        qualified["source_item_revisions"],
        json!(request.expected_source_item_revisions)
    );
    let tree = instance(&qualified, ordinary.aggregate.manifest.occurrence_id);
    assert_eq!(tree["members"].as_array().unwrap().len(), 3);
    let normalized: ComposeScheduleRequest =
        serde_json::from_value(qualified["schedule"].clone()).unwrap();
    let preview = compose_canonical_schedule(&f.items, &f.schedules, normalized)
        .await
        .unwrap();
    let capacity = preview
        .plan
        .blocks
        .iter()
        .filter(|block| block.item_id.map(|id| id.0) == Some(event_id))
        .collect::<Vec<_>>();
    assert_eq!(capacity.len(), 1);
    assert_eq!(capacity[0].kind, ScheduleBlockKind::CalendarEvent);
    assert_eq!(capacity[0].start.unix_timestamp(), f.day.timestamp());
    assert_eq!(
        capacity[0].end.unix_timestamp(),
        (f.day + Duration::hours(1)).timestamp()
    );
    let leaf = preview
        .plan
        .blocks
        .iter()
        .find(|block| {
            block.item_id.map(|id| id.0) == Some(members.leaf)
                && block.occurrence_id.map(|id| id.0)
                    == Some(ordinary.aggregate.manifest.occurrence_id)
        })
        .unwrap();
    assert!(
        leaf.start >= capacity[0].end,
        "actual Calendar capacity excludes the earlier retained assignment"
    );
    let encoded = qualified.to_string();
    for private in [
        account.to_string(),
        collection.to_string(),
        "synthetic-private-calendar-resource".into(),
    ] {
        assert!(
            !encoded.contains(&private),
            "provider identity is represented only by its scoped fingerprint"
        );
    }
    set_calendar_projection(
        &f.pool,
        collection,
        5,
        full_start,
        full_end,
        final_refreshed_at,
    )
    .await;
    let before = f.storage().await;
    let next = f.qualified(&request).await;
    assert_eq!(before, f.storage().await);
    assert_eq!(
        next["request_fingerprint"],
        qualified["request_fingerprint"]
    );
    assert_eq!(
        next["source_item_revisions"],
        qualified["source_item_revisions"]
    );
    assert_eq!(
        next["occurrence_lifecycle"],
        qualified["occurrence_lifecycle"]
    );
    assert_ne!(
        next["calendar_projection_fingerprint"],
        qualified["calendar_projection_fingerprint"]
    );
    assert_ne!(
        next["witness_fingerprint"],
        qualified["witness_fingerprint"]
    );
    // Generation is a witness fence; unchanged capacity can retain the exact
    // helper input fingerprint without pretending it is publication authority.
    assert_eq!(
        next["local_input_fingerprint"],
        qualified["local_input_fingerprint"]
    );
    f.cleanup().await;
}

/// Seed only configured projection storage, mirroring `schedule_postgres.rs`.
/// External Google ingestion/OAuth is deliberately outside this fixture.
async fn seed_calendar_projection(f: &Fixture, event_id: Uuid) -> (Uuid, Uuid) {
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO provider_accounts(id,workspace_id,user_id,provider,external_account_id,display_label,encrypted_credentials,credential_key_version,granted_scopes,status,sync_enabled) VALUES($1,$2,$3,'google',$4,'Synthetic projection account',$5,1,ARRAY['https://www.googleapis.com/auth/calendar.readonly']::text[],'active',true)")
        .bind(account).bind(f.scope.workspace_id).bind(f.scope.user_id).bind(format!("synthetic-account-{account}"))
        .bind(vec![0x53_u8; 32]).execute(&f.pool).await.unwrap();
    let collection = Uuid::new_v4();
    sqlx::query("INSERT INTO google_sync_collections(id,workspace_id,user_id,provider_account_id,collection_kind,remote_collection_id,display_name,provider_access_role,provider_selected,selected,visible,sync_role,discovered_at,configured_at,created_at,updated_at) VALUES($1,$2,$3,$4,'calendar','synthetic-private-calendar-resource','Synthetic blocking calendar','owner',true,true,true,'blocking',clock_timestamp(),clock_timestamp(),clock_timestamp(),clock_timestamp())")
        .bind(collection).bind(f.scope.workspace_id).bind(f.scope.user_id).bind(account).execute(&f.pool).await.unwrap();
    sqlx::query("INSERT INTO provider_sync_mappings(id,workspace_id,provider_account_id,collection_id,entity_kind,local_entity_id,remote_resource_id,ownership,projection_generation,provider_forced_sensitive) VALUES($1,$2,$3,$4,'calendar_occurrence',$5,'synthetic-private-expanded-event','external',1,true)")
        .bind(Uuid::new_v4()).bind(f.scope.workspace_id).bind(account).bind(collection).bind(event_id).execute(&f.pool).await.unwrap();
    (account, collection)
}

async fn set_calendar_projection(
    pool: &PgPool,
    collection: Uuid,
    generation: i64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    refreshed: DateTime<Utc>,
) {
    let mut tx = pool.begin().await.unwrap();
    // Mapping mutations invalidate coverage through the production trigger.
    // Seal the completed generation only after its occurrence mutations.
    sqlx::query("UPDATE provider_sync_mappings SET projection_generation=$2 WHERE collection_id=$1 AND entity_kind='calendar_occurrence' AND tombstoned_at IS NULL")
        .bind(collection).bind(generation).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE google_sync_collections SET planning_projection_state='complete',planning_generation=$2,planning_collection_revision=revision,planning_window_start=$3,planning_window_end=$4,planning_window_refreshed_at=$5,planning_last_error_code=NULL WHERE id=$1")
        .bind(collection).bind(generation).bind(start).bind(end).bind(refreshed).execute(&mut *tx).await.unwrap();
    let sealed: bool = sqlx::query_scalar("SELECT planning_projection_state='complete' AND planning_generation=$2 AND planning_collection_revision=revision AND NOT EXISTS(SELECT 1 FROM provider_sync_mappings mapping WHERE mapping.collection_id=collection.id AND mapping.entity_kind='calendar_occurrence' AND mapping.tombstoned_at IS NULL AND mapping.projection_generation<>$2) FROM google_sync_collections collection WHERE id=$1")
        .bind(collection).bind(generation).fetch_one(&mut *tx).await.unwrap();
    assert!(
        sealed,
        "all occurrence mappings precede the final coverage seal"
    );
    tx.commit().await.unwrap();
}

async fn wait_for_blocked_queries(pool: &PgPool, blocker_pid: i32, minimum: i64, phase: &str) {
    tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let blocked: i64 = sqlx::query_scalar(
                "WITH RECURSIVE blocked(pid) AS ( \
                   SELECT activity.pid FROM pg_stat_activity activity WHERE $1=ANY(pg_blocking_pids(activity.pid)) \
                   UNION SELECT activity.pid FROM pg_stat_activity activity \
                   JOIN blocked prior ON prior.pid=ANY(pg_blocking_pids(activity.pid)) \
                 ) SELECT COUNT(*) FROM blocked")
                .bind(blocker_pid).fetch_one(pool).await.unwrap();
            if blocked >= minimum { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap_or_else(|error| panic!("PostgreSQL waiter phase {phase}: {error}"));
}
