use std::{str::FromStr, time::Duration};

use chrono::{Duration as ChronoDuration, Utc};
use dayweave_api::{
    persistence::{
        DatabaseScope, IdempotencyDecision, IdempotencyError, MIGRATOR, NewOutboxMessage,
        PostgresIdempotencyRepository, PostgresOutboxRepository, PostgresProposalRepository,
    },
    proposals::{
        NewProposal, Proposal, ProposalKind, ProposalRepository, ProposalSource, RepositoryError,
    },
};
use serde_json::json;
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[test]
fn embedded_migrations_cover_the_durable_domain_without_compile_time_database_access() {
    let versions: Vec<_> = MIGRATOR.iter().map(|migration| migration.version).collect();
    assert_eq!(versions, vec![1, 2, 3]);

    let schema = [
        include_str!("../migrations/0001_identity_and_items.sql"),
        include_str!("../migrations/0002_schedule_sync_and_audit.sql"),
        include_str!("../migrations/0003_proposals_mcp_idempotency_outbox.sql"),
    ]
    .join("\n");
    for table in [
        "users",
        "workspaces",
        "provider_accounts",
        "items",
        "item_hierarchy",
        "item_dependencies",
        "schedule_revisions",
        "schedule_blocks",
        "provider_sync_mappings",
        "provider_sync_cursors",
        "sessions",
        "audit_operations",
        "outbox_messages",
        "proposals",
        "mcp_clients",
        "idempotency_keys",
    ] {
        assert!(schema.contains(&format!("CREATE TABLE {table}")), "{table}");
    }
    assert!(schema.contains("timestamptz"));
    assert!(!schema.contains("timestamp without time zone"));
    assert!(schema.contains("trashed_at"));
    assert!(schema.contains("tombstoned_at"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn postgres_migrations_repository_idempotency_and_outbox_are_transactional() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; PostgreSQL integration test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    MIGRATOR.run(pool).await.expect("migrations are repeatable");

    let scope = seed_scope(pool).await;
    let repository = PostgresProposalRepository::new(pool.clone(), scope);
    let now = Utc::now();
    let original = Proposal::new(
        NewProposal {
            submitted_by: "integration-test".to_owned(),
            source: ProposalSource::ExternalMcp,
            source_reference: Some("conversation-safe-label".to_owned()),
            kind: ProposalKind::SchedulePlan,
            title: "Review a proposed schedule".to_owned(),
            explanation: Some("Stored only in the Suggestions Inbox".to_owned()),
            payload: json!({"proposal_only": true}),
            expires_at: now + ChronoDuration::days(7),
        },
        now,
    )
    .unwrap();
    repository.insert(original.clone()).await.unwrap();
    assert_eq!(
        repository.get(original.id).await.unwrap().title,
        original.title
    );

    let other_scope = seed_other_scope(pool).await;
    let other_repository = PostgresProposalRepository::new(pool.clone(), other_scope);
    assert_eq!(
        other_repository.get(original.id).await.unwrap_err(),
        RepositoryError::NotFound(original.id)
    );

    let mut changed = original.clone();
    changed.revision = 2;
    changed.title = "Updated review title".to_owned();
    changed.updated_at = now + ChronoDuration::minutes(1);
    assert_eq!(
        repository.replace(changed.clone(), 9).await.unwrap_err(),
        RepositoryError::RevisionConflict {
            expected: 9,
            actual: 1,
        }
    );
    repository.replace(changed.clone(), 1).await.unwrap();
    repository.delete(changed.id, 2).await.unwrap();
    assert_eq!(
        repository.get(changed.id).await.unwrap_err(),
        RepositoryError::NotFound(changed.id)
    );
    let retained: bool = sqlx::query_scalar(
        "SELECT trashed_at IS NOT NULL FROM proposals WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(changed.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(retained);
    let mutation_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND aggregate_type = 'proposal' AND aggregate_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(changed.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(mutation_events, 3);
    let audit_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
         AND entity_type = 'proposal' AND entity_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(changed.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(audit_events, 3);

    let idempotency = PostgresIdempotencyRepository::new(pool.clone(), scope);
    let fingerprint = [7_u8; 32];
    let expires_at = Utc::now() + ChronoDuration::hours(1);
    let different_response = json!({"different": true});
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                expires_at
            )
            .await
            .unwrap(),
        IdempotencyDecision::Acquired
    );
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                expires_at
            )
            .await
            .unwrap(),
        IdempotencyDecision::InProgress
    );
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &[8_u8; 32],
                expires_at,
            )
            .await
            .unwrap(),
        IdempotencyDecision::Conflict
    );
    let replay = json!({"proposal_id": changed.id, "review_required": true});
    idempotency
        .complete(
            "mcp.submit_proposal",
            "stable-client-key",
            &fingerprint,
            Some("proposal"),
            Some(changed.id),
            Some(&replay),
        )
        .await
        .unwrap();
    idempotency
        .complete(
            "mcp.submit_proposal",
            "stable-client-key",
            &fingerprint,
            Some("proposal"),
            Some(changed.id),
            Some(&replay),
        )
        .await
        .unwrap();
    assert_eq!(
        idempotency
            .complete(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                Some("proposal"),
                Some(changed.id),
                Some(&different_response),
            )
            .await
            .unwrap_err(),
        IdempotencyError::Conflict
    );
    assert_eq!(
        idempotency
            .reserve(
                "mcp.submit_proposal",
                "stable-client-key",
                &fingerprint,
                expires_at
            )
            .await
            .unwrap(),
        IdempotencyDecision::Replay {
            resource_id: Some(changed.id),
            response: Some(replay),
        }
    );
    let raw_key_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM idempotency_keys WHERE key_hash = $1")
            .bind(b"stable-client-key".as_slice())
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(raw_key_count, 0);

    let outbox = PostgresOutboxRepository::new(pool.clone(), scope);
    let event_id = outbox
        .enqueue(NewOutboxMessage {
            aggregate_type: "sync_cursor".to_owned(),
            aggregate_id: Uuid::new_v4(),
            aggregate_revision: Some(1),
            event_type: "sync.cursor_advanced".to_owned(),
            deduplication_key: Some("sync-cursor-fixture-1".to_owned()),
            payload: json!({"cursor_advanced": true}),
            headers: json!({}),
            available_at: Utc::now(),
        })
        .await
        .unwrap();
    let claimed = outbox
        .claim_batch("integration-worker", 10, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(claimed.iter().any(|message| message.id == event_id));
    outbox
        .mark_published(event_id, "integration-worker")
        .await
        .unwrap();
    let published: bool =
        sqlx::query_scalar("SELECT published_at IS NOT NULL FROM outbox_messages WHERE id = $1")
            .bind(event_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(published);

    test_database.destroy().await;
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
        let schema = format!("dayweave_test_{}", Uuid::new_v4().simple());
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

async fn seed_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    insert_scope(pool, scope, "owner-one", "personal-one").await;
    scope
}

async fn seed_other_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    insert_scope(pool, scope, "owner-two", "personal-two").await;
    scope
}

async fn insert_scope(pool: &PgPool, scope: DatabaseScope, subject: &str, slug: &str) {
    sqlx::query(
        "INSERT INTO users (id, auth_subject, display_name, timezone_name) \
         VALUES ($1, $2, 'Test owner', 'Europe/Madrid')",
    )
    .bind(scope.user_id)
    .bind(subject)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id, owner_user_id, slug, name, timezone_name) \
         VALUES ($1, $2, $3, 'Test workspace', 'Europe/Madrid')",
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
}
