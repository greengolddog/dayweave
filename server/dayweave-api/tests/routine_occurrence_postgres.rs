//! Opt-in integration against an explicitly supplied disposable `PostgreSQL`.
//! All scopes, schemas and content here are synthetic; no provider is used.
use std::{str::FromStr as _, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    execution::{
        DeferAssessmentRequest, DeferExecution, ExecutionCommand, ExecutionIdempotencyKey,
        ExecutionRepositoryError, ExecutionService, ExecutionServiceError, ExecutionStatus,
        PauseExecution, StartExecution,
    },
    item_completion::{ItemCompletionCommand, ItemCompletionMode},
    items::{
        IdempotencyKey, Item, ItemRepository as _, ItemService, ItemStatus, NewItem, ReplaceItem,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresExecutionRepository, PostgresItemRepository,
        PostgresRoutineOccurrenceRepository,
    },
    proposals::SystemClock,
    routine_occurrences::{
        RoutineOccurrenceAction, RoutineOccurrenceCommand, RoutineOccurrenceError,
        RoutineOccurrenceSnapshot,
    },
    scheduling::{
        ComposeScheduleRequest, ComposeScheduleResult, PostgresSchedulingRepository,
        PublishScheduleSpec, ScheduleAccess, SchedulePublicationError, compose_canonical_schedule,
    },
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
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
    items: Arc<ItemService>,
    item_repository: Arc<PostgresItemRepository>,
    occurrences: PostgresRoutineOccurrenceRepository,
    schedules: PostgresSchedulingRepository,
    access: ScheduleAccess,
    day: DateTime<Utc>,
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
        let schema = format!("routine_occurrence_test_{}", Uuid::new_v4().simple());
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
        sqlx::query("INSERT INTO users(id,auth_subject,display_name) VALUES($1,$2,'Synthetic routine owner')")
            .bind(scope.user_id).bind(scope.user_id.to_string()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO workspaces(id,owner_user_id,slug,name) VALUES($1,$2,'routine','Synthetic routine')")
            .bind(scope.workspace_id).bind(scope.user_id).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO workspace_members(workspace_id,user_id,role) VALUES($1,$2,'owner')",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(&pool)
        .await
        .unwrap();
        let item_repository = Arc::new(PostgresItemRepository::new(pool.clone(), scope));
        let items = Arc::new(ItemService::new(
            item_repository.clone(),
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
            item_repository,
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

    async fn seed(&self) -> (Uuid, Uuid, Uuid) {
        let root = Uuid::new_v4();
        let required = Uuid::new_v4();
        let optional = Uuid::new_v4();
        let mut root_input = input(root, None);
        root_input.kind = dayweave_api::items::ItemKind::Routine;
        root_input.duration_seconds = None;
        root_input.recurrence = Some(json!({"type":"daily","times_per_day":1}));
        self.items.create(root_input, key()).await.unwrap();
        self.items
            .create(input(required, Some(root)), key())
            .await
            .unwrap();
        self.items
            .create(input(optional, Some(root)), key())
            .await
            .unwrap();
        let proof = self.item_repository.get_completion(optional).await.unwrap();
        self.item_repository
            .put_completion(
                optional,
                ItemCompletionCommand {
                    schema_version: 1,
                    operation_id: Uuid::new_v4(),
                    expected_item_revision: proof.item_revision,
                    expected_completion_revision: proof.state.revision,
                    expected_evidence_hash: proof.evidence_hash,
                    required_for_parent: false,
                    mode: ItemCompletionMode::Automatic,
                    reopening: None,
                },
                Utc::now(),
                None,
            )
            .await
            .unwrap();
        (root, required, optional)
    }

    fn request(&self) -> ComposeScheduleRequest {
        serde_json::from_value(json!({"as_of":self.day,"horizon_start":self.day,
            "horizon_end":self.day+Duration::days(2),"timezone_name":"UTC",
            "availability":[{"start":self.day,"end":self.day+Duration::days(2),"contexts":[],"location":null,"energy":"deep"}],
            "fixed_blocks":[],"previous_assignments":[],"recurrence_context":{}})).unwrap()
    }

    async fn compose(&self) -> ComposeScheduleResult {
        compose_canonical_schedule(&self.items, &self.schedules, self.request())
            .await
            .unwrap()
    }

    fn spec(result: ComposeScheduleResult) -> PublishScheduleSpec {
        PublishScheduleSpec {
            idempotency_key: Uuid::new_v4(),
            request_hash: [19; 32],
            input_digest: digest(&result.input_digest),
            timezone_name: "UTC".into(),
            manual_placement_approvals: vec![],
            result,
            published_at: Utc::now(),
        }
    }

    async fn publish(&self) -> Uuid {
        self.schedules
            .publish(&self.access, Self::spec(self.compose().await))
            .await
            .unwrap()
            .revision
            .id
    }

    async fn command(
        &self,
        instance: Uuid,
        member: Uuid,
        action: RoutineOccurrenceAction,
    ) -> RoutineOccurrenceCommand {
        let snapshot = self.occurrences.get(instance).await.unwrap();
        command(&snapshot, member, action)
    }

    async fn canonical(&self) -> Value {
        sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(item) ORDER BY id),'[]'::jsonb) FROM items item WHERE workspace_id=$1")
            .bind(self.scope.workspace_id).fetch_one(&self.pool).await.unwrap()
    }
}

fn key() -> IdempotencyKey {
    IdempotencyKey {
        key: Uuid::new_v4().to_string(),
        fingerprint: [17; 32],
    }
}
fn input(id: Uuid, parent: Option<Uuid>) -> NewItem {
    serde_json::from_value(json!({"id":id,"is_sensitive":false,"kind":"task","status":"planned",
        "title":"Synthetic occurrence member","notes":null,"timezone_name":"UTC","duration_seconds":60,
        "deadline_at":null,"earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":1,"urgency":1,"parent_id":parent,"sibling_order":0})).unwrap()
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
fn digest(value: &str) -> [u8; 32] {
    let value = value.strip_prefix("sha256:").unwrap();
    std::array::from_fn(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
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
            .find(|state| state.item_id == member)
            .unwrap()
            .revision,
        expected_evidence_hash: snapshot.evidence_hash.clone(),
        action,
    }
}
fn status(snapshot: &RoutineOccurrenceSnapshot, member: Uuid) -> ItemStatus {
    snapshot
        .aggregate
        .members
        .iter()
        .find(|state| state.item_id == member)
        .unwrap()
        .status
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One transaction scenario follows immutable receipts through current-state and restart reads.
async fn publication_admits_complete_instances_and_member_outcomes_survive_exact_replay() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let (root, required, optional) = f.seed().await;
    assert!(
        f.occurrences
            .list(None, 100)
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    let first_publication = f.publish().await;
    let canonical = f.canonical().await;
    let first = f.occurrences.list(None, 1).await.unwrap();
    assert!(first.has_more);
    let second = f.occurrences.list(Some(&first.cursor), 1).await.unwrap();
    assert!(!second.has_more);
    assert_eq!(first.changes.len(), 1);
    assert_eq!(second.changes.len(), 1);
    let one = &first.changes[0].occurrence;
    let other = second.changes[0].occurrence.clone();
    assert_ne!(one.aggregate.manifest.id, other.aggregate.manifest.id);
    assert_eq!(one.aggregate.manifest.members.len(), 3);
    assert!(one.fresh_edit_eligible);
    assert_eq!(one.aggregate.revision, 1);
    assert!(
        one.aggregate
            .members
            .iter()
            .all(|state| state.revision == 1)
    );
    assert!(
        !one.aggregate
            .members
            .iter()
            .find(|state| state.item_id == optional)
            .unwrap()
            .required_for_parent
    );
    let instance = one.aggregate.manifest.id;
    let reviewed = command(
        one,
        required,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Completed,
        },
    );
    let receipt = f
        .occurrences
        .put(instance, required, reviewed.clone(), None)
        .await
        .unwrap();
    assert!(!receipt.replayed);
    assert_eq!(receipt.occurrence.aggregate.revision, 2);
    assert_eq!(status(&receipt.occurrence, required), ItemStatus::Completed);
    assert_eq!(status(&receipt.occurrence, root), ItemStatus::Completed);
    assert_eq!(status(&receipt.occurrence, optional), ItemStatus::Planned);
    assert!(
        receipt
            .occurrence
            .aggregate
            .members
            .iter()
            .find(|state| state.item_id == root)
            .unwrap()
            .provenance
            .is_some()
    );
    assert_eq!(
        f.occurrences
            .get(other.aggregate.manifest.id)
            .await
            .unwrap()
            .aggregate,
        other.aggregate
    );
    let skipped = f
        .command(
            instance,
            optional,
            RoutineOccurrenceAction::SetOutcome {
                status: ItemStatus::Skipped,
            },
        )
        .await;
    let skipped = f
        .occurrences
        .put(instance, optional, skipped, None)
        .await
        .unwrap();
    assert_eq!(status(&skipped.occurrence, optional), ItemStatus::Skipped);
    assert_eq!(status(&skipped.occurrence, root), ItemStatus::Completed);
    assert_eq!(
        f.canonical().await,
        canonical,
        "occurrence outcomes cannot touch templates"
    );
    let restarted = PostgresRoutineOccurrenceRepository::new(f.pool.clone(), f.scope);
    let replay = restarted
        .put(instance, required, reviewed.clone(), None)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        replay.occurrence, receipt.occurrence,
        "historical receipt is immutable, not current state"
    );
    assert_eq!(
        restarted.get(instance).await.unwrap().aggregate,
        skipped.occurrence.aggregate
    );
    let mut reused = reviewed.clone();
    reused.action = RoutineOccurrenceAction::SetOutcome {
        status: ItemStatus::Skipped,
    };
    assert_eq!(
        restarted
            .put(instance, required, reused, None)
            .await
            .unwrap_err(),
        RoutineOccurrenceError::OperationReused
    );
    assert_eq!(
        restarted
            .put(instance, optional, reviewed, None)
            .await
            .unwrap_err(),
        RoutineOccurrenceError::OperationReused
    );
    let delta = restarted.delta(Some(&second.cursor), 1).await.unwrap();
    assert!(delta.has_more);
    assert_eq!(
        delta.changes[0].occurrence.aggregate,
        receipt.occurrence.aggregate
    );
    assert!(!delta.changes[0].occurrence.fresh_edit_eligible);
    let tail = restarted.delta(Some(&delta.cursor), 1).await.unwrap();
    assert!(!tail.has_more);
    assert_eq!(
        tail.changes[0].occurrence.aggregate,
        skipped.occurrence.aggregate
    );
    assert!(tail.changes[0].sequence > delta.changes[0].sequence);
    assert!(
        restarted
            .delta(Some(&tail.cursor), 1)
            .await
            .unwrap()
            .changes
            .is_empty()
    );
    let links:i64 = sqlx::query_scalar("SELECT count(*) FROM routine_occurrence_publications WHERE workspace_id=$1 AND schedule_revision_id=$2")
        .bind(f.scope.workspace_id).bind(first_publication).fetch_one(&f.pool).await.unwrap();
    assert_eq!(links, 2);
    let untouched:i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM execution_sessions WHERE workspace_id=$1)+(SELECT count(*) FROM item_progress WHERE workspace_id=$1)+(SELECT count(*) FROM habit_occurrence_outcomes WHERE workspace_id=$1)")
        .bind(f.scope.workspace_id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(untouched, 0);
    f.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep stale preview, authoritative remaining work and semantic source drift in one scenario.
async fn lifecycle_fences_publication_keeps_optional_work_and_rejects_definition_drift() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let (root, required, optional) = f.seed().await;
    f.publish().await;
    let list = f.occurrences.list(None, 100).await.unwrap();
    let instance = list.changes[0].occurrence.aggregate.manifest.id;
    let occurrence = list.changes[0].occurrence.aggregate.manifest.occurrence_id;
    let stale = Fixture::spec(f.compose().await);
    let reviewed = f
        .command(
            instance,
            required,
            RoutineOccurrenceAction::SetOutcome {
                status: ItemStatus::Completed,
            },
        )
        .await;
    let receipt = f
        .occurrences
        .put(instance, required, reviewed.clone(), None)
        .await
        .unwrap();
    assert!(matches!(
        f.schedules.publish(&f.access, stale).await,
        Err(SchedulePublicationError::StaleComposition)
    ));
    let partial = f.compose().await;
    assert!(
        !partial
            .plan
            .blocks
            .iter()
            .any(|block| block.item_id.map(|id| id.0) == Some(required)
                && block.occurrence_id.map(|id| id.0) == Some(occurrence))
    );
    assert!(
        partial
            .plan
            .blocks
            .iter()
            .any(|block| block.item_id.map(|id| id.0) == Some(optional)
                && block.occurrence_id.map(|id| id.0) == Some(occurrence))
    );
    let mut spoofed = f.request();
    spoofed
        .recurrence_context
        .completed_occurrence_ids
        .insert(dayweave_core::OccurrenceId(occurrence));
    let spoofed = compose_canonical_schedule(&f.items, &f.schedules, spoofed)
        .await
        .unwrap();
    assert!(
        spoofed
            .plan
            .blocks
            .iter()
            .any(|block| block.item_id.map(|id| id.0) == Some(optional)
                && block.occurrence_id.map(|id| id.0) == Some(occurrence)),
        "caller whole-completion cannot erase managed optional work"
    );
    let published = f
        .schedules
        .publish(&f.access, Fixture::spec(partial))
        .await
        .unwrap();
    let schema: String = sqlx::query_scalar("SELECT result_snapshot->>'schema_version' FROM schedule_revision_details WHERE workspace_id=$1 AND schedule_revision_id=$2")
        .bind(f.scope.workspace_id).bind(published.revision.id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(schema, "6");
    let root_item = f.items.get(root).await.unwrap();
    let mut renamed = replacement(&root_item);
    renamed.title = "Synthetic harmless renamed template".into();
    let renamed = f
        .items
        .replace(root, root_item.revision, renamed, key())
        .await
        .unwrap()
        .item;
    let after_rename = f.occurrences.get(instance).await.unwrap();
    assert!(
        after_rename.fresh_edit_eligible,
        "harmless source revisions remain reviewable"
    );
    assert_ne!(after_rename.evidence_hash, receipt.occurrence.evidence_hash);
    let mut edited = replacement(&renamed);
    edited.recurrence = Some(json!({"type":"daily","times_per_day":2}));
    f.items
        .replace(root, renamed.revision, edited, key())
        .await
        .unwrap();
    let drifted = f.occurrences.get(instance).await.unwrap();
    assert!(!drifted.fresh_edit_eligible);
    let attempt = command(
        &drifted,
        optional,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Skipped,
        },
    );
    assert_eq!(
        f.occurrences
            .put(instance, optional, attempt, None)
            .await
            .unwrap_err(),
        RoutineOccurrenceError::DefinitionChanged
    );
    assert_eq!(
        f.occurrences
            .put(instance, required, reviewed, None)
            .await
            .unwrap()
            .occurrence,
        receipt.occurrence
    );
    f.cleanup().await;
}

#[tokio::test]
async fn managed_done_member_cannot_restart_and_live_member_cannot_be_settled() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let (_, required, _) = f.seed().await;
    f.publish().await;
    let baseline = f
        .occurrences
        .list(None, 100)
        .await
        .unwrap()
        .changes
        .remove(0)
        .occurrence;
    let instance = baseline.aggregate.manifest.id;
    let occurrence = baseline.aggregate.manifest.occurrence_id;
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(f.pool.clone(), f.scope)),
        f.items.clone(),
        Arc::new(SystemClock),
    );
    let done = command(
        &baseline,
        required,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Completed,
        },
    );
    f.occurrences
        .put(instance, required, done, None)
        .await
        .unwrap();
    let item = f.items.get(required).await.unwrap();
    let start = ExecutionCommand::Start(StartExecution {
        session_id: Uuid::new_v4(),
        item_id: required,
        item_revision: item.revision,
        occurrence_id: Some(occurrence),
        session_index: 0,
        planned_block_id: None,
        device_id: Uuid::new_v4(),
    });
    let key = || ExecutionIdempotencyKey {
        key: Uuid::new_v4().to_string(),
        fingerprint: [23; 32],
    };
    assert!(execution.command(0, start.clone(), key()).await.is_err());
    let reopen = f
        .command(
            instance,
            required,
            RoutineOccurrenceAction::Reopen {
                open: baseline
                    .aggregate
                    .members
                    .iter()
                    .find(|member| member.item_id == required)
                    .unwrap()
                    .open
                    .clone(),
            },
        )
        .await;
    f.occurrences
        .put(instance, required, reopen, None)
        .await
        .unwrap();
    execution
        .command(0, start, key())
        .await
        .expect("the exact reopened member can start");
    let done = f
        .command(
            instance,
            required,
            RoutineOccurrenceAction::SetOutcome {
                status: ItemStatus::Completed,
            },
        )
        .await;
    assert_eq!(
        f.occurrences
            .put(instance, required, done, None)
            .await
            .unwrap_err(),
        RoutineOccurrenceError::ExecutionConflict
    );
    f.cleanup().await;
}

#[tokio::test]
async fn immutable_manifest_history_and_atomic_member_seals_reject_direct_corruption() {
    let Some(f) = Fixture::create().await else {
        return;
    };
    let (_, required, _) = f.seed().await;
    f.publish().await;
    let page = f.occurrences.list(None, 100).await.unwrap();
    let snapshot = &page.changes[0].occurrence;
    let id = snapshot.aggregate.manifest.id;
    let original = serde_json::to_value(&snapshot.aggregate).unwrap();
    for statement in [
        "UPDATE routine_occurrences SET definition_hash=definition_hash WHERE workspace_id=$1",
        "DELETE FROM routine_occurrence_members WHERE workspace_id=$1",
        "UPDATE routine_occurrence_changes SET aggregate_json=aggregate_json WHERE workspace_id=$1",
        "UPDATE routine_occurrence_state SET revision=revision+1 WHERE workspace_id=$1",
        "DELETE FROM item_changes WHERE workspace_id=$1 AND (item_id,item_revision) IN (SELECT item_id,source_revision FROM routine_occurrence_members WHERE workspace_id=$1)",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(f.scope.workspace_id)
                .execute(&f.pool)
                .await
                .is_err(),
            "guard must reject {statement}"
        );
    }
    assert!(sqlx::query("INSERT INTO routine_occurrence_members(workspace_id,instance_id,item_id,parent_item_id,source_revision) VALUES($1,$2,$3,NULL,1)")
        .bind(f.scope.workspace_id).bind(id).bind(Uuid::new_v4()).execute(&f.pool).await.is_err(),"manifest cannot gain members after capture");
    for malformed in [
        {
            let mut value = original.clone();
            value["revision"] = json!(2);
            value["members"][1] = value["members"][0].clone();
            value
        },
        {
            let mut value = original.clone();
            value["revision"] = json!(2);
            value["members"][0]["status"] = json!("future_unknown");
            value
        },
        {
            let mut value = original.clone();
            value["revision"] = json!(2);
            value
        },
    ] {
        let mut tx = f.pool.begin().await.unwrap();
        assert!(sqlx::query("INSERT INTO routine_occurrence_changes(workspace_id,instance_id,revision,operation_id,before_json,aggregate_json,effects_json,changed_at) VALUES($1,$2,2,$3,$4,$5,'[]'::jsonb,clock_timestamp())")
            .bind(f.scope.workspace_id).bind(id).bind(Uuid::new_v4()).bind(&original).bind(malformed).execute(&mut *tx).await.is_err());
        tx.rollback().await.unwrap();
    }
    assert_eq!(
        f.occurrences.get(id).await.unwrap().aggregate,
        snapshot.aggregate
    );
    assert_eq!(
        f.occurrences
            .delta(Some(&(page.cursor.clone() + "x")), 100)
            .await
            .unwrap_err(),
        RoutineOccurrenceError::InvalidCursor
    );
    let proof = f
        .command(
            id,
            required,
            RoutineOccurrenceAction::SetOutcome {
                status: ItemStatus::Completed,
            },
        )
        .await;
    let receipt = f
        .occurrences
        .put(id, required, proof.clone(), None)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET trashed_at=clock_timestamp() WHERE id=$1")
        .bind(f.scope.user_id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.occurrences.get(id).await.unwrap_err(),
        RoutineOccurrenceError::Unavailable
    );
    assert_eq!(
        f.occurrences
            .put(id, required, proof, None)
            .await
            .unwrap_err(),
        RoutineOccurrenceError::Unavailable,
        "account lifecycle admission precedes even historical replay"
    );
    let retained:Value=sqlx::query_scalar("SELECT result_json FROM routine_occurrence_operations WHERE workspace_id=$1 AND operation_id=$2")
        .bind(f.scope.workspace_id).bind(receipt.operation_id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(retained, serde_json::to_value(receipt.occurrence).unwrap());
    f.cleanup().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Both branches hold the first sealed publication constant through assessment, intervention and submission.
async fn first_publication_defer_bridge_preserves_capsule_and_later_policy_review_fences_submission()
 {
    for intervene in [false, true] {
        let Some(f) = Fixture::create().await else {
            return;
        };
        let (root, required, _) = f.seed().await;
        let preview = f.compose().await;
        let source = preview
            .plan
            .blocks
            .iter()
            .filter(|block| block.item_id.map(|id| id.0) == Some(required))
            .min_by_key(|block| block.start)
            .unwrap()
            .clone();
        let publication = f
            .schedules
            .publish(&f.access, Fixture::spec(preview))
            .await
            .unwrap();
        assert_eq!(publication.revision.revision_number, 1);
        let capsule_sql = "SELECT jsonb_build_object('revision',to_jsonb(revision),'detail',to_jsonb(detail)) FROM schedule_revisions revision JOIN schedule_revision_details detail ON detail.workspace_id=revision.workspace_id AND detail.schedule_revision_id=revision.id WHERE revision.workspace_id=$1 AND revision.id=$2";
        let capsule: Value = sqlx::query_scalar(capsule_sql)
            .bind(f.scope.workspace_id)
            .bind(publication.revision.id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(
            capsule["detail"]["result_snapshot"]["schema_version"],
            json!(5)
        );
        assert_eq!(
            capsule["detail"]["result_snapshot"]["evidence"]["occurrence_lifecycle"]["snapshot_revision"],
            json!(0)
        );
        assert!(!capsule["revision"]["publication_hash"].is_null());
        let ledger = f.occurrences.list(None, 100).await.unwrap();
        assert_eq!(ledger.changes.len(), 2);
        let idle = ledger
            .changes
            .iter()
            .map(|change| &change.occurrence)
            .find(|snapshot| {
                Some(snapshot.aggregate.manifest.occurrence_id)
                    != source.occurrence_id.map(|id| id.0)
            })
            .unwrap();
        let execution = ExecutionService::new(
            Arc::new(PostgresExecutionRepository::new(f.pool.clone(), f.scope)),
            f.items.clone(),
            Arc::new(SystemClock),
        );
        let command_key = || ExecutionIdempotencyKey {
            key: Uuid::new_v4().to_string(),
            fingerprint: [29; 32],
        };
        let session_id = Uuid::new_v4();
        execution
            .command(
                0,
                ExecutionCommand::Start(StartExecution {
                    session_id,
                    item_id: required,
                    item_revision: f.items.get(required).await.unwrap().revision,
                    occurrence_id: source.occurrence_id.map(|id| id.0),
                    session_index: source.session_index,
                    planned_block_id: Some(source.id),
                    device_id: Uuid::new_v4(),
                }),
                command_key(),
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
                    reason: Some("Synthetic reviewed move".into()),
                }),
                command_key(),
            )
            .await
            .unwrap();
        let assessment = execution
            .assess_defer(DeferAssessmentRequest {
                expected_revision: 2,
                session_id,
                move_start: f.day + Duration::hours(2),
                actual_seconds: Some(0),
            })
            .await
            .expect("first publication admits a valid Defer assessment without publishing twice");
        assert_eq!(assessment.remaining_duration_seconds, 60);
        let before_sql = "SELECT jsonb_build_object('state',(SELECT to_jsonb(state) FROM execution_state state WHERE workspace_id=$1),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM execution_sessions session WHERE workspace_id=$1),'claims',(SELECT COALESCE(jsonb_agg(to_jsonb(claim) ORDER BY source_deferred_session_id),'[]'::jsonb) FROM execution_defer_replacement_claims claim WHERE workspace_id=$1))";
        let before: Value = sqlx::query_scalar(before_sql)
            .bind(f.scope.workspace_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        if intervene {
            // Start/Pause advances global execution evidence, even for an idle
            // occurrence. Review this policy against that current evidence;
            // only the already-issued Defer assessment is intentionally old.
            let fresh_idle = f.occurrences.get(idle.aggregate.manifest.id).await.unwrap();
            assert_eq!(fresh_idle.aggregate, idle.aggregate);
            assert_ne!(fresh_idle.evidence_hash, idle.evidence_hash);
            let reviewed = command(
                &fresh_idle,
                root,
                RoutineOccurrenceAction::SetPolicy {
                    required_for_parent: true,
                    mode: ItemCompletionMode::Automatic,
                },
            );
            let receipt = f
                .occurrences
                .put(idle.aggregate.manifest.id, root, reviewed, None)
                .await
                .unwrap();
            assert_eq!(
                status(&receipt.occurrence, root),
                status(idle, root),
                "review is lifecycle-inert"
            );
            assert_eq!(
                receipt.occurrence.aggregate.revision,
                idle.aggregate.revision + 1
            );
        }
        let approval = assessment
            .approval_required
            .then(|| assessment.assessment_digest.clone());
        let submit_key = command_key();
        let key_hash: [u8; 32] = Sha256::digest(submit_key.key.as_bytes()).into();
        let result = execution
            .command(
                2,
                ExecutionCommand::Defer(DeferExecution {
                    session_id,
                    move_start: assessment.move_start,
                    move_end: assessment.move_end,
                    actual_seconds: Some(assessment.actual_seconds),
                    assessment_digest: Some(assessment.assessment_digest),
                    approved_assessment_digest: approval,
                }),
                submit_key,
            )
            .await;
        if intervene {
            assert!(matches!(
                result,
                Err(ExecutionServiceError::Repository(
                    ExecutionRepositoryError::ScheduleStale
                        | ExecutionRepositoryError::DeferAssessmentStale
                ))
            ));
            let after: Value = sqlx::query_scalar(before_sql)
                .bind(f.scope.workspace_id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
            assert_eq!(
                after, before,
                "stale assessment must not mutate execution or mint a replacement claim"
            );
            let receipts:i64=sqlx::query_scalar("SELECT count(*) FROM idempotency_keys WHERE workspace_id=$1 AND namespace='execution.command' AND key_hash=$2")
                .bind(f.scope.workspace_id).bind(key_hash.as_slice()).fetch_one(&f.pool).await.unwrap();
            assert_eq!(receipts, 0);
        } else {
            let result =
                result.expect("unchanged initial admission bridge permits first-publication Defer");
            assert_eq!(result.revision, 3);
            assert_eq!(result.changed_session.status, ExecutionStatus::Deferred);
            let claims:i64=sqlx::query_scalar("SELECT count(*) FROM execution_defer_replacement_claims WHERE workspace_id=$1 AND source_deferred_session_id=$2")
                .bind(f.scope.workspace_id).bind(session_id).fetch_one(&f.pool).await.unwrap();
            assert_eq!(claims, 1);
        }
        let retained: Value = sqlx::query_scalar(capsule_sql)
            .bind(f.scope.workspace_id)
            .bind(publication.revision.id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(
            retained, capsule,
            "bridging is read-only: original capsule, hash and source proof are immutable"
        );
        let publications: i64 =
            sqlx::query_scalar("SELECT count(*) FROM schedule_revisions WHERE workspace_id=$1")
                .bind(f.scope.workspace_id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(publications, 1);
        f.cleanup().await;
    }
}
