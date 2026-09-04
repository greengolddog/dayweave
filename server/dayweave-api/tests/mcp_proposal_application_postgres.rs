use std::{str::FromStr, sync::Arc};

use chrono::{Duration, Utc};
use dayweave_api::{
    items::{
        IdempotencyKey, Item, ItemKind, ItemService, ItemStatus, NewItem, ReplaceItem, SplitPolicy,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresItemRepository, PostgresProposalApplicationRepository,
        PostgresProposalRepository,
    },
    proposals::{
        ProposalApplyRequest, ProposalChangeSet, ProposalCommand, ProposalConflictCode,
        ProposalPreviewMember, ProposalPreviewRequest, ProposalRepository, SystemClock,
    },
    scheduling::{
        ComposeScheduleRequest, PlanOperation, PlanOperationKind, PlanningSimulationPort,
        PostgresSchedulingRepository, ProposalSubmissionPort, ProposalSubmissionSpec,
        PublishScheduleSpec, ScheduleAccess, SimulationRequest, compose_canonical_schedule,
    },
};
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)] // Keeps the MCP proof, device review fence, and canonical commit in one lifecycle.
async fn mcp_typed_proposal_requires_a_fresh_device_preview_before_canonical_apply() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&database.pool).await;
    let items = ItemService::new(
        Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
        Arc::new(SystemClock),
    );
    let schedules = PostgresSchedulingRepository::new(database.pool.clone(), scope);
    let proposals = PostgresProposalRepository::new(database.pool.clone(), scope);
    let applications = PostgresProposalApplicationRepository::new(database.pool.clone(), scope);
    let access = ScheduleAccess {
        subject: "mcp:e2e-assistant".to_owned(),
        include_sensitive: false,
        workspace_id: Some(scope.workspace_id),
        user_id: Some(scope.user_id),
    };

    let baseline = items
        .create(
            task(Uuid::new_v4(), "Existing planning baseline"),
            idempotency("mcp-bridge-baseline", 1),
        )
        .await
        .expect("baseline item created")
        .item;
    let composition = compose_canonical_schedule(&items, &schedules, compose_request())
        .await
        .expect("canonical schedule composed");
    let input_digest = sha256_bytes(&composition.input_digest);
    let publication = schedules
        .publish(
            &access,
            PublishScheduleSpec {
                idempotency_key: Uuid::new_v4(),
                request_hash: [2; 32],
                input_digest,
                timezone_name: "Europe/Madrid".to_owned(),
                manual_placement_approvals: Vec::new(),
                result: composition,
                published_at: Utc::now(),
            },
        )
        .await
        .expect("schedule published");

    let operation = operation(
        PlanOperationKind::CreateItem,
        json!({
            "is_sensitive": false,
            "kind": "task",
            "status": "inbox",
            "title": "Prepare MCP review notes",
            "notes": "Created only after native-device approval.",
            "timezone_name": "Europe/Madrid",
            "duration_seconds": 2700,
            "deadline_at": "2026-09-03T16:00:00Z",
            "earliest_start_at": "2026-08-31T07:00:00Z",
            "recurrence": null,
            "flexible_constraints": {},
            "split_policy": { "type": "indivisible" },
            "importance": 70,
            "urgency": 55,
            "parent_id": null,
            "sibling_order": 0
        }),
    );
    let simulation_request = SimulationRequest {
        base_revision: publication.revision.revision.clone(),
        operations: vec![operation],
        assumptions: vec!["The user will review the exact item before applying it.".to_owned()],
    };
    let simulation = schedules
        .simulate(&access, simulation_request.clone())
        .await
        .expect("MCP-style plan simulation succeeds");
    assert!(simulation.application_ready);
    assert_eq!(
        simulation.change_set_schema.as_deref(),
        Some("dayweave.proposal-change-set/1")
    );
    assert!(!simulation.simulation_token.is_empty());

    let submitted = schedules
        .submit_proposal(
            &access,
            ProposalSubmissionSpec {
                idempotency_key: "mcp-device-bridge-submit-0001".to_owned(),
                request_fingerprint: [3; 32],
                simulation_token: simulation.simulation_token,
                request: simulation_request,
                title: "Create review-notes task".to_owned(),
                explanation: "The external assistant prepared one reviewable task.".to_owned(),
                source_conversation_label: "MCP bridge end-to-end".to_owned(),
                source_client_label: Some("postgres-integration-test".to_owned()),
                source_request_id: Uuid::new_v4().to_string(),
                expires_at: Utc::now() + Duration::days(1),
            },
        )
        .await
        .expect("simulation token atomically becomes a proposal");
    assert!(!submitted.duplicate);

    let stored = proposals
        .get(submitted.proposal.id)
        .await
        .expect("submitted proposal is durable");
    let change_set = ProposalChangeSet::from_payload(&stored.payload)
        .expect("durable MCP payload uses the executable schema");
    let ProposalCommand::CreateItem { command_id, item } = &change_set.commands[0] else {
        panic!("supported create_item must compile to one typed create command");
    };
    let expected_item = NewItem {
        id: item.id,
        is_sensitive: false,
        kind: ItemKind::Task,
        status: ItemStatus::Inbox,
        title: "Prepare MCP review notes".to_owned(),
        notes: Some("Created only after native-device approval.".to_owned()),
        timezone_name: "Europe/Madrid".to_owned(),
        duration_kind: None,
        duration_seconds: Some(2700),
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: None,
        deadline_date: None,
        deadline_at: Some("2026-09-03T16:00:00Z".parse().unwrap()),
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: Some("2026-08-31T07:00:00Z".parse().unwrap()),
        recurrence: None,
        flexible_constraints: json!({}),
        has_own_effort: None,
        split_policy: SplitPolicy::Indivisible,
        importance: 70,
        urgency: 55,
        parent_id: None,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    };
    let expected_change_set = ProposalChangeSet::new(vec![ProposalCommand::CreateItem {
        command_id: *command_id,
        item: expected_item,
    }])
    .unwrap();
    assert_eq!(change_set, expected_change_set);
    assert_eq!(
        stored.payload,
        serde_json::to_value(&expected_change_set).unwrap()
    );
    assert!(stored.payload.get("safety").is_none());
    let created_item_id = item.id;

    let first_preview = applications
        .preview(ProposalPreviewRequest {
            proposals: vec![ProposalPreviewMember {
                proposal_id: stored.id,
                expected_revision: stored.revision,
            }],
        })
        .await
        .expect("native device previews the typed proposal");
    assert!(first_preview.can_apply);

    let baseline = items
        .get(baseline.id)
        .await
        .expect("baseline remains active");
    let mut changed_baseline = replacement(&baseline);
    changed_baseline.notes = Some("A canonical edit after the first review.".to_owned());
    items
        .replace(
            baseline.id,
            baseline.revision,
            changed_baseline,
            idempotency("mcp-bridge-stale-preview", 4),
        )
        .await
        .expect("intervening canonical edit succeeds");
    assert!(matches!(
        applications
            .apply(
                first_preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: first_preview.review_hash,
                },
                "mcp-device-bridge-stale-apply",
                None,
            )
            .await,
        Err(dayweave_api::persistence::ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewMismatch
        ))
    ));
    assert!(!item_exists(&database.pool, scope, created_item_id).await);

    let fresh_preview = applications
        .preview(ProposalPreviewRequest {
            proposals: vec![ProposalPreviewMember {
                proposal_id: stored.id,
                expected_revision: stored.revision,
            }],
        })
        .await
        .expect("device creates a fresh content-bound preview");
    assert!(fresh_preview.can_apply);
    let applied = applications
        .apply(
            fresh_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: fresh_preview.review_hash,
            },
            "mcp-device-bridge-fresh-apply",
            None,
        )
        .await
        .expect("freshly reviewed typed proposal applies atomically");
    assert!(!applied.replayed);
    assert_eq!(applied.application.affected_item_ids, vec![created_item_id]);
    let canonical = items
        .get(created_item_id)
        .await
        .expect("proposal application creates the canonical item");
    assert_eq!(canonical.title, "Prepare MCP review notes");
    assert_eq!(canonical.status, ItemStatus::Inbox);
    assert_eq!(canonical.duration_seconds, Some(2700));

    database.destroy().await;
}

fn compose_request() -> ComposeScheduleRequest {
    serde_json::from_value(json!({
        "as_of": "2026-08-30T06:00:00Z",
        "horizon_start": "2026-08-31T00:00:00Z",
        "horizon_end": "2026-09-02T00:00:00Z",
        "timezone_name": "Europe/Madrid",
        "availability": [{
            "start": "2026-08-31T07:00:00Z",
            "end": "2026-08-31T18:00:00Z",
            "contexts": [],
            "location": null,
            "energy": "deep"
        }],
        "fixed_blocks": [],
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

fn task(id: Uuid, title: &str) -> NewItem {
    NewItem {
        id,
        is_sensitive: false,
        kind: ItemKind::Task,
        status: ItemStatus::Planned,
        title: title.to_owned(),
        notes: None,
        timezone_name: "Europe/Madrid".to_owned(),
        duration_kind: None,
        duration_seconds: Some(1800),
        duration_min_seconds: None,
        duration_max_seconds: None,
        duration_source: None,
        deadline_kind: None,
        deadline_date: None,
        deadline_at: Some("2026-08-31T16:00:00Z".parse().unwrap()),
        deadline_strength: None,
        deadline_soft_weight: None,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: json!({}),
        has_own_effort: None,
        split_policy: SplitPolicy::Indivisible,
        importance: 50,
        urgency: 50,
        parent_id: None,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

fn replacement(item: &Item) -> ReplaceItem {
    ReplaceItem {
        is_sensitive: item.is_sensitive,
        kind: item.kind,
        status: item.status,
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
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
        blocked_reason_kind: item.blocked_reason_kind,
        blocked_by_item_id: item.blocked_by_item_id,
        blocked_reason: item.blocked_reason.clone(),
    }
}

fn operation(kind: PlanOperationKind, parameters: Value) -> PlanOperation {
    let Value::Object(parameters) = parameters else {
        panic!("test operation parameters must be an object");
    };
    PlanOperation {
        kind,
        target_id: None,
        parameters: parameters.into_iter().collect(),
    }
}

fn idempotency(key: &str, fingerprint: u8) -> IdempotencyKey {
    IdempotencyKey {
        key: key.to_owned(),
        fingerprint: [fingerprint; 32],
    }
}

fn sha256_bytes(value: &str) -> [u8; 32] {
    let encoded = value
        .strip_prefix("sha256:")
        .expect("composition digest uses sha256 prefix")
        .as_bytes();
    let mut output = [0_u8; 32];
    for (index, pair) in encoded.chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    output
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => panic!("composition digest contains non-hex data"),
    }
}

async fn item_exists(pool: &PgPool, scope: DatabaseScope, item_id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id = $1 AND id = $2)")
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'MCP bridge owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(format!("mcp-bridge-owner-{}", scope.user_id.simple()))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'MCP bridge workspace', 'Europe/Madrid')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("mcp-bridge-{}", scope.workspace_id.simple()))
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
        let schema = format!("dayweave_mcp_apply_test_{}", Uuid::new_v4().simple());
        admin
            .execute(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .expect("create isolated test schema");
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
