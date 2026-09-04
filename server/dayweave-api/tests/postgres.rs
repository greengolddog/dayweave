use std::{collections::BTreeSet, str::FromStr, time::Duration};

use chrono::{Duration as ChronoDuration, Utc};
use dayweave_api::{
    google_oauth::{
        AuthorizationCompletion, AuthorizationResolution, CallbackClaim, DisconnectMutation,
        EncryptedCredentials, GoogleAccountStatus, GoogleOAuthRepository,
        GoogleOAuthRepositoryError, NewOAuthSession, OAuthIdempotency, SealedSecret,
    },
    persistence::{
        DatabaseScope, IdempotencyDecision, IdempotencyError, MIGRATOR, NewOutboxMessage,
        PostgresGoogleOAuthRepository, PostgresIdempotencyRepository, PostgresOutboxRepository,
        PostgresProposalRepository,
    },
    proposals::{
        NewProposal, Proposal, ProposalKind, ProposalRepository, ProposalSource, RepositoryError,
    },
    readiness::Readiness,
};
use serde_json::json;
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool, Postgres,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[test]
#[allow(clippy::too_many_lines)] // One inventory assertion covers every embedded migration contract.
fn embedded_migrations_cover_the_durable_domain_without_compile_time_database_access() {
    let versions: Vec<_> = MIGRATOR.iter().map(|migration| migration.version).collect();
    assert_eq!(
        versions,
        vec![
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25
        ]
    );

    let schema = [
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
        include_str!("../migrations/0012_schedule_publication.sql"),
        include_str!("../migrations/0013_schedule_seal_and_mcp_submission.sql"),
        include_str!("../migrations/0014_google_calendar_projection.sql"),
        include_str!("../migrations/0015_transactional_proposal_applications.sql"),
        include_str!("../migrations/0016_mcp_simulation_evidence.sql"),
        include_str!("../migrations/0017_google_refresh_generations.sql"),
        include_str!("../migrations/0018_execution_defer.sql"),
        include_str!("../migrations/0019_schedule_deferred_placements.sql"),
        include_str!("../migrations/0020_execution_progress_ledger.sql"),
        include_str!("../migrations/0021_execution_defer_approval.sql"),
        include_str!("../migrations/0022_google_schedule_publication.sql"),
        include_str!("../migrations/0023_google_task_provider_metadata.sql"),
        include_str!("../migrations/0024_structural_item_fields.sql"),
        include_str!("../migrations/0025_authoritative_dependency_graph.sql"),
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
        "mcp_proposal_submissions",
        "schedule_revision_details",
        "schedule_publication_requests",
        "schedule_simulations",
        "provider_sync_mappings",
        "provider_sync_cursors",
        "sessions",
        "audit_operations",
        "outbox_messages",
        "proposals",
        "mcp_clients",
        "device_enrollments",
        "idempotency_keys",
        "item_changes",
        "execution_sessions",
        "execution_state",
        "schedule_deferred_placements",
        "execution_session_schedule_origins",
        "execution_defer_assessments",
        "execution_defer_replacement_claims",
        "execution_defer_replacement_consumptions",
        "execution_physical_indices",
        "schedule_defer_replacement_placements",
        "google_oauth_sessions",
        "google_oauth_cleanup_tokens",
        "google_oauth_scope_state",
        "google_oauth_guardian_resolutions",
        "google_oauth_legacy_credential_quarantine",
        "google_sync_collections",
        "google_sync_runs",
        "google_sync_outbox",
        "google_calendar_projection_rejections",
        "google_outbound_previews",
        "google_provider_identity_roots",
        "google_sync_refresh_requests",
        "google_schedule_publication_mapping_origins",
        "google_schedule_publication_previews",
        "google_schedule_publication_preview_changes",
        "google_schedule_publication_batches",
        "google_schedule_publication_outbox",
        "google_schedule_publication_observations",
        "proposal_apply_previews",
        "proposal_apply_preview_members",
        "proposal_applications",
        "proposal_application_members",
        "proposal_application_effects",
        "proposal_application_fences",
        "proposal_application_requests",
    ] {
        assert!(schema.contains(&format!("CREATE TABLE {table}")), "{table}");
    }
    assert!(schema.contains("ADD COLUMN move_start timestamptz"));
    assert!(schema.contains("ADD COLUMN move_end timestamptz"));
    assert!(schema.contains("ADD COLUMN observed_running_since timestamptz"));
    assert!(schema.contains("'deferred'"));
    assert!(schema.contains("ended_at = updated_at"));
    assert!(schema.contains("move_start > ended_at"));
    assert!(schema.contains("UPDATE execution_state AS state"));
    assert!(schema.contains("max(updated_at) AS updated_at"));
    assert!(schema.contains("execution_sessions_semantic_head_idx"));
    assert!(schema.contains("deferred_execution_session_id"));
    assert!(schema.contains("guard_schedule_deferred_placement"));
    assert!(schema.contains("guard_execution_session_semantic_start"));
    assert!(schema.contains("ADD COLUMN execution_epoch bigint NOT NULL DEFAULT 1"));
    assert!(schema.contains("execution_defer_replacement_claims_physical_index_uq"));
    assert!(schema.contains("new execution defer replacement claims require v1 authorization"));
    assert!(schema.contains("credited_before_seconds"));
    assert!(schema.contains("credited_source_seconds"));
    assert!(schema.contains("approval_required = (jsonb_array_length(violations) > 0)"));
    assert!(schema.contains("NULLS NOT DISTINCT"));
    assert!(schema.contains("protect_execution_defer_claim_source"));
    assert!(schema.contains("FOR UPDATE"));
    assert!(schema.contains("revision.state IN ('published', 'superseded')"));
    assert!(schema.contains("timestamptz"));
    assert!(schema.contains("ADD COLUMN google_task_metadata jsonb"));
    assert!(schema.contains("provider_sync_mappings_google_task_metadata_shape_ck"));
    assert!(!schema.contains("timestamp without time zone"));
    assert!(schema.contains("trashed_at"));
    assert!(schema.contains("tombstoned_at"));
    assert!(schema.contains("DELETE FROM provider_sync_cursors cursor"));
    assert!(schema.contains("cursor.collection_key = 'calendar:' || collection.id::text"));
    assert!(schema.contains("provider_sync_mappings_sensitivity_floor_item_fk"));
    assert!(schema.contains("'dayweave.items.v1:' || NEW.workspace_id::text"));
    assert!(schema.contains("schedule_simulations_evidence_guard"));
    assert!(schema.contains("mcp_proposal_submissions_verify_simulation"));
    assert!(schema.contains("compiled_payload_hash IS NOT NULL"));
    assert!(schema.contains("provider_sync_mappings_active_schedule_identity_uq"));
    assert!(schema.contains("google_schedule_publication_preview_changes_immutable"));
    assert!(schema.contains("google_schedule_publication_batch_aggregate_exact"));
    for structural_contract in [
        "ADD COLUMN duration_kind varchar(16)",
        "ADD COLUMN deadline_kind varchar(16)",
        "ADD COLUMN has_own_effort boolean",
        "ADD COLUMN blocked_reason_kind varchar(16)",
        "items_duration_shape_check",
        "items_deadline_shape_check",
        "items_blocked_reason_shape_check",
        "'start_to_finish'",
        "ADD COLUMN dependency_strength varchar(16)",
        "item_dependencies_strength_check",
        "duration_seconds above the supported 31622400-second maximum",
        "item_dependencies contains lag_seconds outside the supported 0..31622400 whole-minute range",
        "item_dependencies_aggregate_write_guard",
        "item_dependencies_acyclic",
        "dayweave_dependency_cutover",
        "LOCK TABLE items, item_hierarchy, item_dependencies IN SHARE ROW EXCLUSIVE MODE",
        "items_dependency_projection_forbidden",
        "ADD COLUMN projection_ordinal integer",
        "item_dependencies_projection_ordinal_check",
        "ADD COLUMN change_group_id uuid",
        "item_changes_workspace_group_idx",
        "require_item_change_group",
        "item_changes_group_required",
        "ADD COLUMN review_ordinal smallint",
        "proposal_application_effects_review_ordinal_uq",
        "proposal_application_effects_review_complete",
    ] {
        assert!(
            schema.contains(structural_contract),
            "{structural_contract}"
        );
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The upgrade fixture and its raw-SQL backstops must share one isolated schema.
async fn execution_progress_ledger_migration_backfills_partitioned_fresh_claims() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; execution ledger migration test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 20) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-ledger migration applies");
    }
    let scope = seed_scope(pool).await;
    let first_item = Uuid::new_v4();
    let second_item = Uuid::new_v4();
    let historical_item = Uuid::new_v4();
    let published_high_water_item = Uuid::new_v4();
    let base = Utc::now();
    for (item_id, title) in [
        (first_item, "First legacy defer"),
        (second_item, "Second legacy defer"),
        (historical_item, "Historical completed defer"),
        (published_high_water_item, "Published split high-water"),
    ] {
        sqlx::query(
            "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
             timezone_name, duration_seconds, revision, created_at, updated_at) \
             VALUES ($1, $2, $3, 'task', 'planned', $4, 'UTC', 3600, 1, $5, $5)",
        )
        .bind(item_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(title)
        .bind(base)
        .execute(pool)
        .await
        .expect("seed pre-ledger item");
    }
    let first_defer = Uuid::new_v4();
    let later_first_defer = Uuid::new_v4();
    let second_defer = Uuid::new_v4();
    insert_pre_v20_deferred_session(
        pool,
        scope,
        first_defer,
        first_item,
        2,
        base,
        base + ChronoDuration::hours(1),
    )
    .await;
    let historical_defer = Uuid::new_v4();
    insert_pre_v20_deferred_session(
        pool,
        scope,
        historical_defer,
        historical_item,
        9,
        base + ChronoDuration::seconds(2),
        base + ChronoDuration::hours(3),
    )
    .await;
    insert_pre_v20_completed_session(
        pool,
        scope,
        Uuid::new_v4(),
        historical_item,
        9,
        base + ChronoDuration::seconds(3),
    )
    .await;

    let schedule_revision_id = Uuid::new_v4();
    let schedule_block_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, \
         horizon_start, horizon_end, timezone_name, solver_version, input_digest, \
         created_by_user_id, created_at) VALUES ($1, $2, 1, 'draft', $3, $4, 'UTC', \
         'execution-ledger-migration-test', $5, $6, $7)",
    )
    .bind(schedule_revision_id)
    .bind(scope.workspace_id)
    .bind(base)
    .bind(base + ChronoDuration::days(1))
    .bind(vec![20_u8; 32])
    .bind(scope.user_id)
    .bind(base)
    .execute(pool)
    .await
    .expect("seed pre-v20 current schedule draft");
    sqlx::query(
        "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
         result_snapshot, created_at) VALUES ($1, $2, $3, '{}'::jsonb, $4)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(schedule_revision_id)
    .bind(base)
    .execute(pool)
    .await
    .expect("seed pre-v20 current schedule details");
    sqlx::query(
        "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, schedule_revision_id, \
         item_id, block_kind, title_snapshot, starts_at, ends_at, timezone_name, ordinal, \
         is_fixed, constraint_snapshot, created_at) VALUES ($1, $2, $3, $4, $5, 'planned', \
         'Published high-water', $6, $7, 'UTC', 0, false, $8, $9)",
    )
    .bind(Uuid::new_v4())
    .bind(schedule_block_id)
    .bind(scope.workspace_id)
    .bind(schedule_revision_id)
    .bind(published_high_water_item)
    .bind(base + ChronoDuration::hours(4))
    .bind(base + ChronoDuration::hours(5))
    .bind(json!({"occurrence_id": null, "session_index": 8}))
    .bind(base)
    .execute(pool)
    .await
    .expect("seed pre-v20 current schedule high-water block");
    sqlx::query(
        "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(schedule_revision_id)
    .bind(base)
    .execute(pool)
    .await
    .expect("publish pre-v20 high-water schedule");
    insert_pre_v20_deferred_session(
        pool,
        scope,
        later_first_defer,
        first_item,
        5,
        base + ChronoDuration::seconds(1),
        base + ChronoDuration::hours(2),
    )
    .await;
    insert_pre_v20_deferred_session(
        pool,
        scope,
        second_defer,
        second_item,
        2,
        base,
        base + ChronoDuration::hours(1),
    )
    .await;

    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 20)
        .expect("execution ledger migration is embedded");
    pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect("execution ledger migration applies");

    let claims: Vec<(Uuid, i32, String, i64, bool)> = sqlx::query_as(
        "SELECT source_deferred_session_id, replacement_session_index, \
         planned_duration_source, planned_duration_seconds, actionable \
         FROM execution_defer_replacement_claims WHERE workspace_id = $1 \
         ORDER BY source_deferred_session_id",
    )
    .bind(scope.workspace_id)
    .fetch_all(pool)
    .await
    .expect("load backfilled claims");
    assert_eq!(claims.len(), 4);
    let replacement_for = |session_id| {
        claims
            .iter()
            .find(|(source, _, _, _, _)| *source == session_id)
            .expect("backfilled source claim")
    };
    assert_eq!(replacement_for(first_defer).1, 6);
    assert_eq!(replacement_for(later_first_defer).1, 7);
    assert_eq!(replacement_for(second_defer).1, 3);
    assert_eq!(replacement_for(historical_defer).1, 10);
    assert!(replacement_for(first_defer).4);
    assert!(replacement_for(later_first_defer).4);
    assert!(replacement_for(second_defer).4);
    assert!(!replacement_for(historical_defer).4);
    assert!(claims.iter().all(|(_, _, source, duration, _)| {
        source == "legacy_move_window" && *duration == 30 * 60
    }));
    let epochs: Vec<i64> = sqlx::query_scalar(
        "SELECT execution_epoch FROM execution_sessions WHERE workspace_id = $1 ORDER BY id",
    )
    .bind(scope.workspace_id)
    .fetch_all(pool)
    .await
    .expect("load backfilled execution epochs");
    assert_eq!(epochs, vec![1, 1, 1, 1, 1]);
    let historical_registry_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_physical_indices WHERE workspace_id = $1 \
         AND item_id = $2 AND occurrence_id IS NULL AND session_index = 9",
    )
    .bind(scope.workspace_id)
    .bind(historical_item)
    .fetch_one(pool)
    .await
    .expect("load deduplicated historical physical index");
    assert_eq!(historical_registry_rows, 1);

    let immutable_source = sqlx::query(
        "UPDATE execution_sessions SET actual_seconds = actual_seconds + 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_defer)
    .execute(pool)
    .await
    .expect_err("claimed source cannot drift");
    assert!(
        immutable_source
            .to_string()
            .contains("claimed deferred execution sessions are immutable")
    );
    let immutable_claim = sqlx::query(
        "UPDATE execution_defer_replacement_claims SET created_at = created_at \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_defer)
    .execute(pool)
    .await
    .expect_err("replacement claim is immutable");
    assert!(
        immutable_claim
            .to_string()
            .contains("execution defer replacement claims are immutable")
    );

    let unclaimed_session = Uuid::new_v4();
    let mut unclaimed = pool.begin().await.expect("begin unclaimed raw defer");
    insert_pre_v20_deferred_session(
        &mut *unclaimed,
        scope,
        unclaimed_session,
        first_item,
        100,
        base + ChronoDuration::seconds(4),
        base + ChronoDuration::hours(4),
    )
    .await;
    let unclaimed_error = unclaimed
        .commit()
        .await
        .expect_err("raw deferred session cannot commit without a replacement claim");
    assert!(
        unclaimed_error
            .to_string()
            .contains("deferred execution session lacks its replacement claim")
    );
    let unclaimed_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM execution_sessions WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(unclaimed_session)
    .fetch_one(pool)
    .await
    .expect("count rolled-back raw defer");
    assert_eq!(unclaimed_rows, 0);

    let bypass_source = Uuid::new_v4();
    let bypass_terminal_at = base + ChronoDuration::seconds(2);
    let bypass_move_start = base + ChronoDuration::hours(3);
    let mut bypass = pool.begin().await.expect("begin raw claim bypass");
    insert_pre_v20_deferred_session(
        &mut *bypass,
        scope,
        bypass_source,
        published_high_water_item,
        0,
        bypass_terminal_at,
        bypass_move_start,
    )
    .await;
    let stale_replacement = sqlx::query(
        "INSERT INTO execution_defer_replacement_claims (workspace_id, \
         source_deferred_session_id, item_id, source_item_revision, execution_epoch, \
         occurrence_id, source_session_index, replacement_session_index, \
         planned_duration_seconds, planned_duration_source, consumed_before_seconds, \
         consumed_by_source_seconds, remaining_duration_seconds, move_start, move_end, created_at) \
         VALUES ($1, $2, $3, 1, 1, NULL, 0, 8, 1800, 'legacy_move_window', 0, 0, \
         1800, $4, $5, $6)",
    )
    .bind(scope.workspace_id)
    .bind(bypass_source)
    .bind(published_high_water_item)
    .bind(bypass_move_start)
    .bind(bypass_move_start + ChronoDuration::minutes(30))
    .bind(bypass_terminal_at)
    .execute(&mut *bypass)
    .await
    .expect_err("direct claim cannot reuse historical physical index");
    assert!(
        stale_replacement
            .to_string()
            .contains("execution defer replacement index is not fresh")
    );
    bypass.rollback().await.expect("roll back raw claim bypass");

    let approval_migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 21)
        .expect("execution defer approval migration is embedded");
    pool.execute(AssertSqlSafe(approval_migration.sql.as_str().to_owned()))
        .await
        .expect("execution defer approval migration applies over populated v20 evidence");
    let legacy_authorizations: Vec<(i16, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT authorization_schema_version, authorization_kind, assessment_id \
         FROM execution_defer_replacement_claims WHERE workspace_id = $1 \
         ORDER BY source_deferred_session_id",
    )
    .bind(scope.workspace_id)
    .fetch_all(pool)
    .await
    .expect("load upgraded legacy claim authorization");
    assert_eq!(legacy_authorizations.len(), 4);
    assert!(
        legacy_authorizations
            .iter()
            .all(|(version, kind, assessment)| {
                *version == 0 && kind == "legacy_unassessed" && assessment.is_none()
            })
    );

    let forbidden_v0 = sqlx::query(
        "INSERT INTO execution_defer_replacement_claims (workspace_id, \
         source_deferred_session_id, item_id, source_item_revision, execution_epoch, \
         occurrence_id, source_session_index, replacement_session_index, \
         planned_duration_seconds, planned_duration_source, consumed_before_seconds, \
         consumed_by_source_seconds, remaining_duration_seconds, move_start, move_end, created_at, \
         authorization_schema_version, authorization_kind) VALUES ($1, $2, $3, 1, 1, NULL, \
         200, 201, 1800, 'legacy_move_window', 0, 0, 1800, $4, $5, $6, \
         0, 'legacy_unassessed')",
    )
    .bind(scope.workspace_id)
    .bind(Uuid::new_v4())
    .bind(first_item)
    .bind(base + ChronoDuration::hours(5))
    .bind(base + ChronoDuration::hours(5) + ChronoDuration::minutes(30))
    .bind(base + ChronoDuration::seconds(5))
    .execute(pool)
    .await
    .expect_err("new v0 replacement claims are forbidden after upgrade");
    assert!(
        forbidden_v0
            .to_string()
            .contains("new execution defer replacement claims require v1 authorization")
    );

    test_database.destroy().await;
}

#[tokio::test]
async fn execution_progress_ledger_migration_fails_closed_at_maximum_index() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; execution ledger overflow test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 20) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-ledger migration applies");
    }
    let scope = seed_scope(pool).await;
    let item_id = Uuid::new_v4();
    let base = Utc::now();
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds, revision, created_at, updated_at) \
         VALUES ($1, $2, $3, 'task', 'planned', 'Exhausted legacy defer', 'UTC', \
         3600, 1, $4, $4)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(base)
    .execute(pool)
    .await
    .expect("seed overflow item");
    insert_pre_v20_deferred_session(
        pool,
        scope,
        Uuid::new_v4(),
        item_id,
        i32::from(u16::MAX),
        base,
        base + ChronoDuration::hours(1),
    )
    .await;
    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 20)
        .expect("execution ledger migration is embedded");
    let error = pool
        .execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .expect_err("replacement index overflow aborts migration");
    assert!(
        error
            .to_string()
            .contains("replacement session index space is exhausted during migration")
    );
    let ledger_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('execution_defer_replacement_claims')::text")
            .fetch_one(pool)
            .await
            .expect("inspect failed migration rollback");
    assert!(ledger_table.is_none());
    let epoch_column_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'execution_sessions' \
           AND column_name = 'execution_epoch')",
    )
    .fetch_one(pool)
    .await
    .expect("inspect failed epoch-column rollback");
    assert!(!epoch_column_exists);

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn deferred_placement_migration_seals_exact_restart_evidence() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 20) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-ledger migration applies");
    }

    let scope = seed_scope(pool).await;
    let item_id = Uuid::new_v4();
    let first_session_id = Uuid::new_v4();
    let source_device_id = Uuid::new_v4();
    let started_at = Utc::now();
    let deferred_at = started_at + ChronoDuration::minutes(1);
    let move_start = deferred_at + ChronoDuration::hours(2);
    let move_end = move_start + ChronoDuration::minutes(45);
    sqlx::query(
        "INSERT INTO items (id, workspace_id, created_by_user_id, kind, status, title, \
         timezone_name, duration_seconds, revision, created_at, updated_at) \
         VALUES ($1, $2, $3, 'task', 'planned', 'Deferred migration proof', 'UTC', \
         2700, 1, $4, $4)",
    )
    .bind(item_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(started_at)
    .execute(pool)
    .await
    .expect("seed executable item");

    // A semantic session with no history remains a valid legacy first Start.
    // The trigger also materializes execution_state before inspecting history.
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, NULL, $4, \
         'active', 1, 0, NULL, $5, $5, $5, NULL, NULL, NULL, NULL, NULL, NULL, $5, $5)",
    )
    .bind(first_session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(source_device_id)
    .bind(started_at)
    .execute(pool)
    .await
    .expect("legacy first Start");
    let materialized_state: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_state WHERE workspace_id = $1)")
            .bind(scope.workspace_id)
            .fetch_one(pool)
            .await
            .expect("inspect materialized execution state");
    assert!(materialized_state);
    sqlx::query(
        "UPDATE execution_state SET revision = 1, active_session_id = $2, updated_at = $3 \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(first_session_id)
    .bind(started_at)
    .execute(pool)
    .await
    .expect("point at first session");
    sqlx::query(
        "UPDATE execution_sessions SET state = 'deferred', revision = 2, \
         accumulated_seconds = 60, actual_seconds = 60, running_since = NULL, \
         observed_running_since = NULL, move_start = $3, move_end = $4, ended_at = $5, \
         updated_at = $5 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session_id)
    .bind(move_start)
    .bind(move_end)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect("defer first session");
    sqlx::query(
        "UPDATE execution_state SET revision = 2, active_session_id = NULL, updated_at = $2 \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect("close first execution lease");

    let unbound_restart = sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, $4, $5, \
         'active', 1, 0, NULL, $6, $6, $6, NULL, NULL, NULL, NULL, NULL, NULL, $6, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(Uuid::new_v4())
    .bind(source_device_id)
    .bind(move_start)
    .execute(pool)
    .await
    .expect_err("deferred restart requires attestation");
    assert!(
        unbound_restart
            .to_string()
            .contains("deferred execution requires an exact published schedule binding")
    );

    let revision_id = Uuid::new_v4();
    let source_block_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, \
         horizon_start, horizon_end, timezone_name, solver_version, input_digest, \
         created_by_user_id, created_at) VALUES ($1, $2, 1, 'draft', $3, $4, 'UTC', \
         'deferred-placement-test', $5, $6, $7)",
    )
    .bind(revision_id)
    .bind(scope.workspace_id)
    .bind(move_start - ChronoDuration::hours(1))
    .bind(move_end + ChronoDuration::hours(1))
    .bind(vec![7_u8; 32])
    .bind(scope.user_id)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect("draft schedule revision");
    sqlx::query(
        "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, \
         schedule_revision_id, item_id, block_kind, title_snapshot, starts_at, ends_at, \
         timezone_name, ordinal, is_fixed, is_sensitive, constraint_snapshot) \
         VALUES ($1, $2, $3, $4, $5, 'pinned', 'Deferred migration proof', $6, $7, \
         'UTC', 0, true, false, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(source_block_id)
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(item_id)
    .bind(move_start)
    .bind(move_end)
    .bind(json!({
        "schema_version": 1,
        "source_block_id": source_block_id,
        "occurrence_id": null,
        "session_index": 0,
        "core_kind": "pinned",
        "explanations": [],
    }))
    .execute(pool)
    .await
    .expect("exact pinned block");

    let mismatched_binding = sqlx::query(
        "INSERT INTO schedule_deferred_placements (workspace_id, schedule_revision_id, \
         deferred_execution_session_id, source_block_id, item_id, item_revision, \
         occurrence_id, session_index, move_start, move_end, created_at) \
         VALUES ($1, $2, $3, $4, $5, 1, NULL, 0, $6, $7, $8)",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(first_session_id)
    .bind(source_block_id)
    .bind(item_id)
    .bind(move_start)
    .bind(move_end - ChronoDuration::minutes(1))
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect_err("mismatched defer window is rejected");
    assert!(
        mismatched_binding
            .to_string()
            .contains("does not match the deferred execution session")
    );
    sqlx::query(
        "INSERT INTO schedule_deferred_placements (workspace_id, schedule_revision_id, \
         deferred_execution_session_id, source_block_id, item_id, item_revision, \
         occurrence_id, session_index, move_start, move_end, created_at) \
         VALUES ($1, $2, $3, $4, $5, 1, NULL, 0, $6, $7, $8)",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(first_session_id)
    .bind(source_block_id)
    .bind(item_id)
    .bind(move_start)
    .bind(move_end)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect("exact deferred placement binding");

    for statement in [
        "UPDATE schedule_deferred_placements SET created_at = created_at + interval '1 second' \
         WHERE workspace_id = $1 AND schedule_revision_id = $2",
        "DELETE FROM schedule_deferred_placements \
         WHERE workspace_id = $1 AND schedule_revision_id = $2",
    ] {
        let error = sqlx::query(statement)
            .bind(scope.workspace_id)
            .bind(revision_id)
            .execute(pool)
            .await
            .expect_err("deferred placement evidence is immutable");
        assert!(error.to_string().contains("evidence is immutable"));
    }
    let block_mutation = sqlx::query(
        "UPDATE schedule_blocks SET starts_at = starts_at + interval '1 second' \
         WHERE workspace_id = $1 AND schedule_revision_id = $2 AND source_block_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(source_block_id)
    .execute(pool)
    .await
    .expect_err("bound schedule block is immutable");
    assert!(
        block_mutation
            .to_string()
            .contains("bound deferred schedule blocks are immutable")
    );
    let session_mutation = sqlx::query(
        "UPDATE execution_sessions SET pause_reason = 'tampered' \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session_id)
    .execute(pool)
    .await
    .expect_err("bound deferred session is immutable");
    assert!(
        session_mutation
            .to_string()
            .contains("bound deferred execution sessions are immutable")
    );

    // A binding remains unusable until its immutable revision is sealed.
    let draft_restart = sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, $4, $5, \
         'active', 1, 0, NULL, $6, $6, $6, NULL, NULL, NULL, NULL, NULL, NULL, $6, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(source_block_id)
    .bind(source_device_id)
    .bind(move_start)
    .execute(pool)
    .await
    .expect_err("draft binding is not Start authority");
    assert!(
        draft_restart
            .to_string()
            .contains("deferred execution requires an exact published schedule binding")
    );

    sqlx::query(
        "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
         result_snapshot, created_at) VALUES ($1, $2, $3, '{}'::jsonb, $4)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(revision_id)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect("schedule revision detail");
    sqlx::query(
        "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect("publish bound schedule revision");
    let late_binding = sqlx::query(
        "INSERT INTO schedule_deferred_placements (workspace_id, schedule_revision_id, \
         deferred_execution_session_id, source_block_id, item_id, item_revision, \
         occurrence_id, session_index, move_start, move_end, created_at) \
         VALUES ($1, $2, $3, $4, $5, 1, NULL, 0, $6, $7, $8)",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(first_session_id)
    .bind(source_block_id)
    .bind(item_id)
    .bind(move_start)
    .bind(move_end)
    .bind(deferred_at)
    .execute(pool)
    .await
    .expect_err("sealed revisions reject placement insertion");
    assert!(
        late_binding
            .to_string()
            .contains("require a draft revision")
    );

    let restarted_session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, $4, $5, \
         'active', 1, 0, NULL, $6, $6, $6, NULL, NULL, NULL, NULL, NULL, NULL, $6, $6)",
    )
    .bind(restarted_session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(source_block_id)
    .bind(source_device_id)
    .bind(move_start)
    .execute(pool)
    .await
    .expect("exact published binding authorizes Start");
    sqlx::query(
        "UPDATE execution_state SET revision = 3, active_session_id = $2, updated_at = $3 \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(restarted_session_id)
    .bind(move_start)
    .execute(pool)
    .await
    .expect("point at restarted session");
    let completed_at = move_start + ChronoDuration::minutes(5);
    sqlx::query(
        "UPDATE execution_sessions SET state = 'completed', revision = 2, \
         accumulated_seconds = 300, actual_seconds = 300, running_since = NULL, \
         observed_running_since = NULL, ended_at = $3, updated_at = $3 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(restarted_session_id)
    .bind(completed_at)
    .execute(pool)
    .await
    .expect("complete restarted session");
    sqlx::query(
        "UPDATE execution_state SET revision = 4, active_session_id = NULL, updated_at = $2 \
         WHERE workspace_id = $1",
    )
    .bind(scope.workspace_id)
    .bind(completed_at)
    .execute(pool)
    .await
    .expect("close restarted execution lease");
    let terminal_rewrite = sqlx::query(
        "UPDATE execution_sessions SET state = 'active', revision = revision + 1, \
         actual_seconds = NULL, running_since = $3, observed_running_since = $3, \
         ended_at = NULL, updated_at = $3 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(restarted_session_id)
    .bind(completed_at + ChronoDuration::seconds(1))
    .execute(pool)
    .await
    .expect_err("terminal history cannot be rewritten into an active lease");
    assert!(
        terminal_rewrite
            .to_string()
            .contains("terminal execution semantics cannot be rewritten as active")
    );
    let resurrection = sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, \
         observed_running_since, paused_at, pause_until, pause_reason, move_start, move_end, \
         ended_at, created_at, updated_at) VALUES ($1, $2, $3, 1, NULL, 0, $4, $5, \
         'active', 1, 0, NULL, $6, $6, $6, NULL, NULL, NULL, NULL, NULL, NULL, $6, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(source_block_id)
    .bind(source_device_id)
    .bind(completed_at + ChronoDuration::minutes(1))
    .execute(pool)
    .await
    .expect_err("completed semantic session cannot be resurrected");
    assert!(
        resurrection
            .to_string()
            .contains("completed or skipped execution semantics cannot be restarted")
    );

    let ledger_migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 20)
        .expect("execution ledger migration is embedded");
    pool.execute(AssertSqlSafe(ledger_migration.sql.as_str().to_owned()))
        .await
        .expect("v19 restart history upgrades to the v20 ledger");
    let (replacement_session_index, actionable): (i32, bool) = sqlx::query_as(
        "SELECT replacement_session_index, actionable \
         FROM execution_defer_replacement_claims \
         WHERE workspace_id = $1 AND source_deferred_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session_id)
    .fetch_one(pool)
    .await
    .expect("inspect migrated legacy defer claim");
    assert_eq!(replacement_session_index, 1);
    assert!(
        !actionable,
        "completed v19 restart retires its migrated claim"
    );
    let retained_v19_binding: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schedule_deferred_placements \
         WHERE workspace_id = $1 AND deferred_execution_session_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(first_session_id)
    .fetch_one(pool)
    .await
    .expect("inspect retained v19 placement evidence");
    assert_eq!(retained_v19_binding, 1);

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn mcp_simulation_evidence_upgrade_retires_legacy_capabilities_and_guards_proof() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
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
        include_str!("../migrations/0012_schedule_publication.sql"),
        include_str!("../migrations/0013_schedule_seal_and_mcp_submission.sql"),
        include_str!("../migrations/0014_google_calendar_projection.sql"),
        include_str!("../migrations/0015_transactional_proposal_applications.sql"),
    ] {
        pool.execute(migration)
            .await
            .expect("pre-simulation-evidence migration");
    }

    let scope = seed_scope(pool).await;
    let now = Utc::now();
    let revision_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_revisions (id, workspace_id, revision_number, state, \
         horizon_start, horizon_end, timezone_name, solver_version, input_digest, \
         created_by_user_id, created_at) VALUES ($1, $2, 1, 'draft', $3, $4, 'UTC', \
         'migration-test', $5, $6, $7)",
    )
    .bind(revision_id)
    .bind(scope.workspace_id)
    .bind(now - ChronoDuration::hours(1))
    .bind(now + ChronoDuration::days(1))
    .bind(vec![1_u8; 32])
    .bind(scope.user_id)
    .bind(now)
    .execute(pool)
    .await
    .expect("draft schedule revision");
    sqlx::query(
        "INSERT INTO schedule_revision_details (workspace_id, user_id, schedule_revision_id, \
         result_snapshot, created_at) VALUES ($1, $2, $3, '{}'::jsonb, $4)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(revision_id)
    .bind(now)
    .execute(pool)
    .await
    .expect("schedule revision evidence");
    sqlx::query(
        "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .bind(now)
    .execute(pool)
    .await
    .expect("published schedule revision");

    let legacy_simulation_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO schedule_simulations (id, workspace_id, user_id, token_hash, subject_hash, \
         request_digest, base_revision_id, base_revision_label, result_snapshot, created_at, \
         expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, '1', '{}'::jsonb, $8, $9)",
    )
    .bind(legacy_simulation_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![2_u8; 32])
    .bind(vec![3_u8; 32])
    .bind(vec![4_u8; 16])
    .bind(revision_id)
    .bind(now)
    .bind(now + ChronoDuration::minutes(5))
    .execute(pool)
    .await
    .expect("legacy simulation capability");
    let legacy_proposal_id = Uuid::new_v4();
    insert_mcp_proposal(
        pool,
        scope,
        legacy_proposal_id,
        now,
        json!({"legacy": true}),
    )
    .await;
    sqlx::query(
        "INSERT INTO mcp_proposal_submissions (workspace_id, user_id, subject_hash, key_hash, \
         request_fingerprint, proposal_id, completed_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![5_u8; 32])
    .bind(vec![6_u8; 32])
    .bind(vec![7_u8; 32])
    .bind(legacy_proposal_id)
    .bind(now)
    .execute(pool)
    .await
    .expect("legacy submission receipt");

    pool.execute(include_str!(
        "../migrations/0016_mcp_simulation_evidence.sql"
    ))
    .await
    .expect("simulation evidence migration");

    let legacy_simulation_retained: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM schedule_simulations WHERE id = $1)")
            .bind(legacy_simulation_id)
            .fetch_one(pool)
            .await
            .expect("legacy simulation retirement");
    assert!(!legacy_simulation_retained);
    let legacy_receipt_proof: Option<Uuid> = sqlx::query_scalar(
        "SELECT simulation_id FROM mcp_proposal_submissions \
         WHERE workspace_id = $1 AND proposal_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(legacy_proposal_id)
    .fetch_one(pool)
    .await
    .expect("legacy receipt remains readable");
    assert_eq!(legacy_receipt_proof, None);

    let request_hash = vec![8_u8; 32];
    let request_digest = vec![8_u8; 16];
    let typed_payload = json!({"schema_version": 1, "commands": []});
    let result_snapshot = json!({
        "proposal_evidence": {
            "schema_version": 1,
            "proposal_kind": "create_item",
            "change_set": typed_payload,
            "manual_review_reasons": [],
        }
    });
    let invalid_actionable = sqlx::query(
        "INSERT INTO schedule_simulations (id, workspace_id, user_id, token_hash, subject_hash, \
         request_digest, base_revision_id, base_revision_label, result_snapshot, created_at, \
         expires_at, evidence_schema, request_hash, evidence_hash, compilation_outcome, \
         compiled_payload_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, '1', $8, $9, $10, 1, \
         $11, $12, 'actionable', NULL)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![9_u8; 32])
    .bind(vec![10_u8; 32])
    .bind(&request_digest)
    .bind(revision_id)
    .bind(&result_snapshot)
    .bind(now)
    .bind(now + ChronoDuration::minutes(10))
    .bind(&request_hash)
    .bind(vec![11_u8; 32])
    .execute(pool)
    .await
    .expect_err("actionable evidence requires a compiled payload hash");
    assert_eq!(
        postgres_error_code(&invalid_actionable).as_deref(),
        Some("23514")
    );

    let simulation_id = Uuid::new_v4();
    let simulation_subject_hash = vec![12_u8; 32];
    let evidence_hash = vec![13_u8; 32];
    let compiled_payload_hash = vec![14_u8; 32];
    let expires_at = now + ChronoDuration::minutes(10);
    sqlx::query(
        "INSERT INTO schedule_simulations (id, workspace_id, user_id, token_hash, subject_hash, \
         request_digest, base_revision_id, base_revision_label, result_snapshot, created_at, \
         expires_at, evidence_schema, request_hash, evidence_hash, compilation_outcome, \
         compiled_payload_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, '1', $8, $9, $10, 1, \
         $11, $12, 'actionable', $13)",
    )
    .bind(simulation_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![15_u8; 32])
    .bind(&simulation_subject_hash)
    .bind(&request_digest)
    .bind(revision_id)
    .bind(&result_snapshot)
    .bind(now)
    .bind(expires_at)
    .bind(&request_hash)
    .bind(&evidence_hash)
    .bind(&compiled_payload_hash)
    .execute(pool)
    .await
    .expect("actionable simulation evidence");

    let live_delete = sqlx::query(
        "DELETE FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .execute(pool)
    .await
    .expect_err("a live unconsumed capability cannot be pruned");
    assert_eq!(postgres_error_code(&live_delete).as_deref(), Some("P0001"));
    let evidence_mutation = sqlx::query(
        "UPDATE schedule_simulations SET evidence_hash = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .bind(vec![16_u8; 32])
    .execute(pool)
    .await
    .expect_err("simulation evidence is immutable");
    assert_eq!(
        postgres_error_code(&evidence_mutation).as_deref(),
        Some("P0001")
    );
    let future_consumption = sqlx::query(
        "UPDATE schedule_simulations SET consumed_at = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .bind(now + ChronoDuration::minutes(1))
    .execute(pool)
    .await
    .expect_err("a simulation cannot be pre-consumed with a future timestamp");
    assert_eq!(
        postgres_error_code(&future_consumption).as_deref(),
        Some("P0001")
    );
    sqlx::query(
        "UPDATE schedule_simulations SET consumed_at = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .bind(now)
    .execute(pool)
    .await
    .expect("single consumption transition");
    let second_consumption = sqlx::query(
        "UPDATE schedule_simulations SET consumed_at = $4 \
         WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .bind(now)
    .execute(pool)
    .await
    .expect_err("consumed evidence cannot transition twice");
    assert_eq!(
        postgres_error_code(&second_consumption).as_deref(),
        Some("P0001")
    );

    let unproved_proposal_id = Uuid::new_v4();
    insert_mcp_proposal(
        pool,
        scope,
        unproved_proposal_id,
        now,
        json!({"manual": true}),
    )
    .await;
    let unproved_receipt = sqlx::query(
        "INSERT INTO mcp_proposal_submissions (workspace_id, user_id, subject_hash, key_hash, \
         request_fingerprint, proposal_id, completed_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![17_u8; 32])
    .bind(vec![18_u8; 32])
    .bind(vec![19_u8; 32])
    .bind(unproved_proposal_id)
    .bind(now)
    .execute(pool)
    .await
    .expect_err("new receipts cannot omit simulation proof");
    assert_eq!(
        postgres_error_code(&unproved_receipt).as_deref(),
        Some("P0001")
    );

    let mismatched_proposal_id = Uuid::new_v4();
    insert_mcp_proposal(
        pool,
        scope,
        mismatched_proposal_id,
        now,
        json!({"different": true}),
    )
    .await;
    let mismatched_receipt = insert_simulation_proof_receipt(
        pool,
        scope,
        mismatched_proposal_id,
        simulation_id,
        now,
        expires_at,
        &simulation_subject_hash,
        &request_digest,
        &request_hash,
        revision_id,
        &evidence_hash,
        &compiled_payload_hash,
        20,
    )
    .await
    .expect_err("actionable proposal payload must equal its compiled evidence");
    assert_eq!(
        postgres_error_code(&mismatched_receipt).as_deref(),
        Some("P0001")
    );

    let proposal_id = Uuid::new_v4();
    insert_mcp_proposal(pool, scope, proposal_id, now, typed_payload).await;
    insert_simulation_proof_receipt(
        pool,
        scope,
        proposal_id,
        simulation_id,
        now,
        expires_at,
        &simulation_subject_hash,
        &request_digest,
        &request_hash,
        revision_id,
        &evidence_hash,
        &compiled_payload_hash,
        21,
    )
    .await
    .expect("matching immutable simulation proof");

    sqlx::query(
        "DELETE FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 AND id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .execute(pool)
    .await
    .expect("consumed evidence may be pruned after proof is copied");
    let durable_proof: (Uuid, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT simulation_id, simulation_evidence_hash, proposal_payload_hash \
         FROM mcp_proposal_submissions WHERE workspace_id = $1 AND proposal_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .fetch_one(pool)
    .await
    .expect("receipt retains the evidence commitment");
    assert_eq!(
        durable_proof,
        (simulation_id, evidence_hash, compiled_payload_hash)
    );
    let receipt_mutation = sqlx::query(
        "UPDATE mcp_proposal_submissions SET proposal_payload_hash = $3 \
         WHERE workspace_id = $1 AND proposal_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .bind(vec![22_u8; 32])
    .execute(pool)
    .await
    .expect_err("submission proof is immutable");
    assert_eq!(
        postgres_error_code(&receipt_mutation).as_deref(),
        Some("P0001")
    );

    test_database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn calendar_projection_upgrade_retires_legacy_items_and_resets_only_calendar_cursor() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; Calendar upgrade test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
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
        include_str!("../migrations/0012_schedule_publication.sql"),
        include_str!("../migrations/0013_schedule_seal_and_mcp_submission.sql"),
    ] {
        pool.execute(migration)
            .await
            .expect("pre-projection migration");
    }

    let scope = seed_scope(pool).await;
    let account_id = Uuid::new_v4();
    let paused_account_id = Uuid::new_v4();
    let inactive_account_id = Uuid::new_v4();
    let calendar_id = Uuid::new_v4();
    let unselected_calendar_id = Uuid::new_v4();
    let paused_calendar_id = Uuid::new_v4();
    let inactive_calendar_id = Uuid::new_v4();
    let task_list_id = Uuid::new_v4();
    let now = Utc::now();
    for (id, status, sync_enabled) in [
        (account_id, "active", true),
        (paused_account_id, "paused", false),
        (inactive_account_id, "reauthorization_required", false),
    ] {
        sqlx::query(
            "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
             external_account_id, display_label, encrypted_credentials, credential_key_version, \
             granted_scopes, status, sync_enabled, is_default) \
             VALUES ($1, $2, $3, 'google', $4, 'Synthetic upgrade account', $5, 1, \
             ARRAY['https://www.googleapis.com/auth/calendar.readonly'], $6, $7, false)",
        )
        .bind(id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(format!("upgrade-{id}"))
        .bind(vec![7_u8; 64])
        .bind(status)
        .bind(sync_enabled)
        .execute(pool)
        .await
        .expect("upgrade account");
    }
    for (id, owner_account_id, kind, remote_id, role, selected) in [
        (
            calendar_id,
            account_id,
            "calendar",
            "calendar-remote",
            "blocking",
            true,
        ),
        (
            unselected_calendar_id,
            account_id,
            "calendar",
            "calendar-unselected",
            "blocking",
            false,
        ),
        (
            paused_calendar_id,
            paused_account_id,
            "calendar",
            "calendar-paused",
            "blocking",
            true,
        ),
        (
            inactive_calendar_id,
            inactive_account_id,
            "calendar",
            "calendar-inactive",
            "blocking",
            true,
        ),
        (
            task_list_id,
            account_id,
            "task_list",
            "tasks-remote",
            "read_only",
            true,
        ),
    ] {
        sqlx::query(
            "INSERT INTO google_sync_collections (id, workspace_id, user_id, \
             provider_account_id, collection_kind, remote_collection_id, display_name, \
             provider_access_role, selected, visible, sync_role, discovered_at, configured_at, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'Synthetic collection', \
             'owner', $7, true, $8, $9, $9, $9, $9)",
        )
        .bind(id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(owner_account_id)
        .bind(kind)
        .bind(remote_id)
        .bind(selected)
        .bind(role)
        .bind(now)
        .execute(pool)
        .await
        .expect("upgrade collection");
    }
    let calendar_key = format!("calendar:{calendar_id}");
    let tasks_key = format!("tasks:{task_list_id}");
    for key in [&calendar_key, &tasks_key] {
        sqlx::query(
            "INSERT INTO provider_sync_cursors (workspace_id, provider_account_id, \
             collection_key, encrypted_cursor, cursor_key_version, updated_at) \
             VALUES ($1, $2, $3, $4, 1, $5)",
        )
        .bind(scope.workspace_id)
        .bind(account_id)
        .bind(key)
        .bind(vec![8_u8; 64])
        .bind(now)
        .execute(pool)
        .await
        .expect("legacy cursor");
    }

    let selected_external_id = Uuid::new_v4();
    let unselected_external_id = Uuid::new_v4();
    let paused_external_id = Uuid::new_v4();
    let inactive_external_id = Uuid::new_v4();
    let dayweave_owned_id = Uuid::new_v4();
    let task_item_id = Uuid::new_v4();
    let divergent_external_id = Uuid::new_v4();
    let mixed_revision_external_id = Uuid::new_v4();
    let missing_revision_external_id = Uuid::new_v4();
    let shared_non_calendar_id = Uuid::new_v4();
    let already_trashed_id = Uuid::new_v4();
    let already_trashed_at = now - ChronoDuration::days(1);
    for (item_id, title, revision, trashed_at) in [
        (selected_external_id, "Selected legacy event", 1_i64, None),
        (unselected_external_id, "Unselected legacy event", 1, None),
        (paused_external_id, "Paused legacy event", 1, None),
        (inactive_external_id, "Inactive legacy event", 1, None),
        (dayweave_owned_id, "DayWeave-owned event", 1, None),
        (task_item_id, "Task-list item", 1, None),
        (
            divergent_external_id,
            "Locally edited legacy event",
            2,
            None,
        ),
        (
            mixed_revision_external_id,
            "Mixed-revision legacy event",
            3,
            None,
        ),
        (
            missing_revision_external_id,
            "Missing-revision legacy event",
            2,
            None,
        ),
        (
            shared_non_calendar_id,
            "Legacy event shared with Tasks",
            1,
            None,
        ),
        (
            already_trashed_id,
            "Already retired legacy event",
            1,
            Some(already_trashed_at),
        ),
    ] {
        sqlx::query(
            "INSERT INTO items (id, workspace_id, created_by_user_id, is_sensitive, kind, \
             status, title, timezone_name, scheduling_constraints, split_allowed, revision, \
             created_at, updated_at, trashed_at) VALUES ($1, $2, $3, false, 'event', \
             'scheduled', $4, 'UTC', '{}'::jsonb, false, $5, $6, $6, $7)",
        )
        .bind(item_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(title)
        .bind(revision)
        .bind(now - ChronoDuration::days(10))
        .bind(trashed_at)
        .execute(pool)
        .await
        .expect("legacy canonical item");
    }
    let dayweave_mapping_id = Uuid::new_v4();
    let shadow_external_mapping_id = Uuid::new_v4();
    let divergent_mapping_id = Uuid::new_v4();
    let mixed_matching_mapping_id = Uuid::new_v4();
    let mixed_stale_mapping_id = Uuid::new_v4();
    let missing_revision_mapping_id = Uuid::new_v4();
    let shared_calendar_mapping_id = Uuid::new_v4();
    let shared_task_mapping_id = Uuid::new_v4();
    let legacy_mappings = [
        (
            Uuid::new_v4(),
            account_id,
            calendar_id,
            selected_external_id,
            "upgrade-provider-selected",
            Some(1_i64),
            "external",
        ),
        (
            Uuid::new_v4(),
            account_id,
            unselected_calendar_id,
            unselected_external_id,
            "upgrade-provider-unselected",
            Some(1),
            "external",
        ),
        (
            Uuid::new_v4(),
            paused_account_id,
            paused_calendar_id,
            paused_external_id,
            "upgrade-provider-paused",
            Some(1),
            "external",
        ),
        (
            Uuid::new_v4(),
            inactive_account_id,
            inactive_calendar_id,
            inactive_external_id,
            "upgrade-provider-inactive",
            Some(1),
            "external",
        ),
        (
            dayweave_mapping_id,
            account_id,
            calendar_id,
            dayweave_owned_id,
            "upgrade-provider-dayweave-owned",
            Some(1),
            "dayweave",
        ),
        (
            shadow_external_mapping_id,
            account_id,
            unselected_calendar_id,
            dayweave_owned_id,
            "upgrade-provider-shadow-external",
            Some(1),
            "external",
        ),
        (
            Uuid::new_v4(),
            account_id,
            task_list_id,
            task_item_id,
            "upgrade-provider-task-control",
            Some(1),
            "external",
        ),
        (
            Uuid::new_v4(),
            account_id,
            calendar_id,
            already_trashed_id,
            "upgrade-provider-already-trashed",
            Some(1),
            "external",
        ),
        (
            divergent_mapping_id,
            account_id,
            calendar_id,
            divergent_external_id,
            "upgrade-provider-divergent",
            Some(1),
            "external",
        ),
        (
            mixed_matching_mapping_id,
            account_id,
            calendar_id,
            mixed_revision_external_id,
            "upgrade-provider-mixed-matching",
            Some(3),
            "external",
        ),
        (
            mixed_stale_mapping_id,
            paused_account_id,
            paused_calendar_id,
            mixed_revision_external_id,
            "upgrade-provider-mixed-stale",
            Some(2),
            "external",
        ),
        (
            missing_revision_mapping_id,
            account_id,
            unselected_calendar_id,
            missing_revision_external_id,
            "upgrade-provider-missing-revision",
            None,
            "external",
        ),
        (
            shared_calendar_mapping_id,
            account_id,
            calendar_id,
            shared_non_calendar_id,
            "upgrade-provider-shared-calendar",
            Some(1),
            "external",
        ),
        (
            shared_task_mapping_id,
            account_id,
            task_list_id,
            shared_non_calendar_id,
            "upgrade-provider-shared-task",
            Some(1),
            "external",
        ),
    ];
    for (
        mapping_id,
        owner_account_id,
        collection_id,
        item_id,
        remote_id,
        local_revision,
        ownership,
    ) in legacy_mappings
    {
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
             collection_id, entity_kind, local_entity_id, remote_resource_id, local_revision, \
             sync_state, ownership, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'item', $5, $6, $7, 'synced', $8, $9, $9)",
        )
        .bind(mapping_id)
        .bind(scope.workspace_id)
        .bind(owner_account_id)
        .bind(collection_id)
        .bind(item_id)
        .bind(remote_id)
        .bind(local_revision)
        .bind(ownership)
        .bind(now - ChronoDuration::days(5))
        .execute(pool)
        .await
        .expect("legacy provider mapping");
    }

    pool.execute(include_str!(
        "../migrations/0014_google_calendar_projection.sql"
    ))
    .await
    .expect("Calendar projection migration");
    let remaining: Vec<String> = sqlx::query_scalar(
        "SELECT collection_key FROM provider_sync_cursors WHERE workspace_id = $1 \
         AND provider_account_id = $2 ORDER BY collection_key",
    )
    .bind(scope.workspace_id)
    .bind(account_id)
    .fetch_all(pool)
    .await
    .expect("remaining cursor keys");
    assert_eq!(remaining, vec![tasks_key]);

    let retired_item_ids = [
        selected_external_id,
        unselected_external_id,
        paused_external_id,
        inactive_external_id,
    ];
    for item_id in retired_item_ids {
        let item: (i64, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .expect("retired legacy item");
        assert_eq!(item.0, 2);
        assert!(item.1.is_some());

        let mapping: (i64, String) = sqlx::query_as(
            "SELECT local_revision, sync_state FROM provider_sync_mappings \
             WHERE workspace_id = $1 AND local_entity_id = $2 AND entity_kind = 'item'",
        )
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .expect("recoverable legacy mapping");
        assert_eq!(mapping, (2, "pending_pull".to_owned()));

        let tombstone: serde_json::Value = sqlx::query_scalar(
            "SELECT payload FROM item_changes WHERE workspace_id = $1 AND item_id = $2 \
             AND item_revision = 2 AND change_kind = 'tombstone'",
        )
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .expect("upgrade tombstone delta");
        assert_eq!(
            tombstone.get("id").and_then(|value| value.as_str()),
            Some(item_id.to_string()).as_deref()
        );
        assert_eq!(
            tombstone
                .get("revision")
                .and_then(serde_json::Value::as_i64),
            Some(2)
        );
        assert!(
            tombstone
                .get("deleted_at")
                .is_some_and(|value| !value.is_null())
        );
        assert!(
            tombstone
                .get("parent_id")
                .is_some_and(serde_json::Value::is_null)
        );
    }

    for (item_id, revision) in [
        (dayweave_owned_id, 1_i64),
        (task_item_id, 1),
        (divergent_external_id, 2),
        (mixed_revision_external_id, 3),
        (missing_revision_external_id, 2),
        (shared_non_calendar_id, 1),
    ] {
        let item: (i64, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
            "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .expect("preserved control item");
        assert_eq!(item, (revision, None));
    }
    let owned_mapping: (Option<Uuid>, Option<i64>, String, Option<serde_json::Value>) =
        sqlx::query_as(
            "SELECT local_entity_id, local_revision, sync_state, conflict_metadata \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(dayweave_mapping_id)
        .fetch_one(pool)
        .await
        .expect("DayWeave owner mapping remains attached");
    assert_eq!(
        owned_mapping,
        (Some(dayweave_owned_id), Some(1), "synced".to_owned(), None)
    );

    let preservation_evidence = [
        (
            shadow_external_mapping_id,
            dayweave_owned_id,
            Some(1_i64),
            1_i64,
            true,
            "calendar_projection_upgrade_shared_canonical_item",
        ),
        (
            divergent_mapping_id,
            divergent_external_id,
            Some(1),
            2,
            false,
            "calendar_projection_upgrade_local_revision_diverged",
        ),
        (
            mixed_matching_mapping_id,
            mixed_revision_external_id,
            Some(3),
            3,
            true,
            "calendar_projection_upgrade_local_revision_diverged",
        ),
        (
            mixed_stale_mapping_id,
            mixed_revision_external_id,
            Some(2),
            3,
            false,
            "calendar_projection_upgrade_local_revision_diverged",
        ),
        (
            missing_revision_mapping_id,
            missing_revision_external_id,
            None,
            2,
            false,
            "calendar_projection_upgrade_local_revision_diverged",
        ),
        (
            shared_calendar_mapping_id,
            shared_non_calendar_id,
            Some(1),
            1,
            true,
            "calendar_projection_upgrade_shared_canonical_item",
        ),
    ];
    for (mapping_id, item_id, mapping_revision, item_revision, mapping_revision_matches, reason) in
        preservation_evidence
    {
        let mapping: (Option<Uuid>, Option<i64>, String, serde_json::Value) = sqlx::query_as(
            "SELECT local_entity_id, local_revision, sync_state, conflict_metadata \
             FROM provider_sync_mappings WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(mapping_id)
        .fetch_one(pool)
        .await
        .expect("preserved legacy Calendar mapping evidence");
        assert_eq!(mapping.0, None);
        assert_eq!(mapping.1, None);
        assert_eq!(mapping.2, "conflict");
        assert_eq!(
            mapping.3,
            json!({
                "reason": reason,
                "local_item_id": item_id,
                "item_revision": item_revision,
                "mapping_local_revision": mapping_revision,
                "mapping_revision_matches": mapping_revision_matches,
            })
        );

        let audit: (
            Option<Uuid>,
            Option<i64>,
            Option<i64>,
            String,
            serde_json::Value,
        ) = sqlx::query_as(
            "SELECT entity_id, base_revision, result_revision, outcome, metadata \
             FROM audit_operations WHERE workspace_id = $1 \
               AND operation_type = \
                   'item.google_calendar_legacy_projection_preserved_on_upgrade' \
               AND metadata->>'mapping_id' = $2",
        )
        .bind(scope.workspace_id)
        .bind(mapping_id.to_string())
        .fetch_one(pool)
        .await
        .expect("durable preserved legacy Calendar audit evidence");
        assert_eq!(audit.0, Some(item_id));
        assert_eq!(audit.1, mapping_revision);
        assert_eq!(audit.2, Some(item_revision));
        assert_eq!(audit.3, "conflicted");
        assert_eq!(
            audit.4,
            json!({
                "source": "google_sync",
                "reason": reason,
                "mapping_id": mapping_id,
                "item_id": item_id,
                "item_revision": item_revision,
                "mapping_local_revision": mapping_revision,
                "mapping_revision_matches": mapping_revision_matches,
            })
        );
    }
    let shared_task_mapping: (Option<Uuid>, Option<i64>, String) = sqlx::query_as(
        "SELECT local_entity_id, local_revision, sync_state FROM provider_sync_mappings \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(shared_task_mapping_id)
    .fetch_one(pool)
    .await
    .expect("non-Calendar sibling mapping remains attached");
    assert_eq!(
        shared_task_mapping,
        (Some(shared_non_calendar_id), Some(1), "synced".to_owned())
    );
    let preserved_item_mutations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM item_changes WHERE workspace_id = $1 \
         AND item_id = ANY($2)",
    )
    .bind(scope.workspace_id)
    .bind(vec![
        dayweave_owned_id,
        divergent_external_id,
        mixed_revision_external_id,
        missing_revision_external_id,
        shared_non_calendar_id,
    ])
    .fetch_one(pool)
    .await
    .expect("preserved items have no migration delta");
    assert_eq!(preserved_item_mutations, 0);
    let already_trashed: (i64, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "SELECT revision, trashed_at FROM items WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(already_trashed_id)
    .fetch_one(pool)
    .await
    .expect("already-trashed control item");
    assert_eq!(already_trashed.0, 1);
    assert_eq!(
        already_trashed.1.map(|value| value.timestamp_micros()),
        Some(already_trashed_at.timestamp_micros())
    );

    let retirement_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
         AND operation_type = 'item.google_calendar_legacy_projection_retired_on_upgrade'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("upgrade audit count");
    assert_eq!(retirement_audits, 4);
    let preservation_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_operations WHERE workspace_id = $1 \
         AND operation_type = \
             'item.google_calendar_legacy_projection_preserved_on_upgrade'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("upgrade preservation audit count");
    assert_eq!(preservation_audits, 6);
    let retirement_outbox: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_messages WHERE workspace_id = $1 \
         AND event_type = 'item.google_calendar_legacy_projection_retired_on_upgrade'",
    )
    .bind(scope.workspace_id)
    .fetch_one(pool)
    .await
    .expect("upgrade outbox count");
    assert_eq!(retirement_outbox, 4);
    for raw_remote_id in [
        "upgrade-provider-selected",
        "upgrade-provider-unselected",
        "upgrade-provider-paused",
        "upgrade-provider-inactive",
        "upgrade-provider-shadow-external",
        "upgrade-provider-divergent",
        "upgrade-provider-mixed-matching",
        "upgrade-provider-mixed-stale",
        "upgrade-provider-missing-revision",
        "upgrade-provider-shared-calendar",
        "upgrade-provider-shared-task",
    ] {
        let leaked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM item_changes WHERE workspace_id = $1 \
               AND payload::text LIKE '%' || $2 || '%' \
             UNION ALL SELECT 1 FROM outbox_messages WHERE workspace_id = $1 \
               AND (payload::text LIKE '%' || $2 || '%' OR headers::text LIKE '%' || $2 || '%') \
             UNION ALL SELECT 1 FROM audit_operations WHERE workspace_id = $1 \
               AND metadata::text LIKE '%' || $2 || '%')",
        )
        .bind(scope.workspace_id)
        .bind(raw_remote_id)
        .fetch_one(pool)
        .await
        .expect("raw provider identity scan");
        assert!(!leaked, "raw provider identity escaped: {raw_remote_id}");
    }

    sqlx::query(
        "UPDATE google_sync_collections SET planning_projection_state = 'complete', \
         planning_generation = 1, planning_collection_revision = revision, \
         planning_window_start = $3, planning_window_end = $4, \
         planning_window_refreshed_at = $5 WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(calendar_id)
    .bind(now - ChronoDuration::days(30))
    .bind(now + ChronoDuration::days(120))
    .bind(now)
    .execute(pool)
    .await
    .expect("first generation is accepted");
    sqlx::query(
        "UPDATE google_sync_collections SET planning_generation = planning_generation \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(calendar_id)
    .execute(pool)
    .await
    .expect("no-op generation update is accepted");
    sqlx::query(
        "UPDATE google_sync_collections SET planning_generation = 2 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(calendar_id)
    .execute(pool)
    .await
    .expect("next generation is accepted");
    let regression = sqlx::query(
        "UPDATE google_sync_collections SET planning_generation = 1 \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(calendar_id)
    .execute(pool)
    .await
    .expect_err("generation regression must be rejected by SQL");
    let regression_code = regression
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    assert_eq!(regression_code.as_deref(), Some("23514"));
    let generation: i64 = sqlx::query_scalar(
        "SELECT planning_generation FROM google_sync_collections \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(calendar_id)
    .fetch_one(pool)
    .await
    .expect("generation after rejected regression");
    assert_eq!(generation, 2);

    // The provider path takes the canonical workspace lock and a key lock on
    // the exact sensitive item tuple. A concurrent direct declassification
    // must wait for that path, then reject rather than committing write skew.
    let provider_wins_item_id = Uuid::new_v4();
    let item_wins_item_id = Uuid::new_v4();
    for (item_id, title) in [
        (provider_wins_item_id, "Provider sensitivity race"),
        (item_wins_item_id, "Local sensitivity race"),
    ] {
        sqlx::query(
            "INSERT INTO items (id, workspace_id, created_by_user_id, is_sensitive, kind, \
             status, title, timezone_name, scheduling_constraints, split_allowed, revision, \
             created_at, updated_at) VALUES ($1, $2, $3, true, 'event', 'scheduled', $4, \
             'UTC', '{}'::jsonb, false, 1, $5, $5)",
        )
        .bind(item_id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(title)
        .bind(now)
        .execute(pool)
        .await
        .expect("sensitivity race item");
    }
    let provider_wins_mapping_id = Uuid::new_v4();
    let item_wins_mapping_id = Uuid::new_v4();
    for (mapping_id, item_id, remote_id) in [
        (
            provider_wins_mapping_id,
            provider_wins_item_id,
            "synthetic-provider-wins-occurrence",
        ),
        (
            item_wins_mapping_id,
            item_wins_item_id,
            "synthetic-item-wins-occurrence",
        ),
    ] {
        sqlx::query(
            "INSERT INTO provider_sync_mappings (id, workspace_id, provider_account_id, \
             collection_id, entity_kind, local_entity_id, remote_resource_id, local_revision, \
             sync_state, ownership, projection_generation, provider_forced_sensitive, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, 'calendar_occurrence', $5, $6, \
             1, 'synced', 'external', 1, false, $7, $7)",
        )
        .bind(mapping_id)
        .bind(scope.workspace_id)
        .bind(account_id)
        .bind(calendar_id)
        .bind(item_id)
        .bind(remote_id)
        .bind(now)
        .execute(pool)
        .await
        .expect("sensitivity race mapping");
    }

    let mut provider_wins = pool.begin().await.expect("provider-wins transaction");
    let provider_wins_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *provider_wins)
        .await
        .expect("provider-wins backend pid");
    sqlx::query(
        "UPDATE provider_sync_mappings SET provider_forced_sensitive = true \
         WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(provider_wins_mapping_id)
    .execute(&mut *provider_wins)
    .await
    .expect("provider path establishes sensitivity floor");
    let concurrent_pool = (*pool).clone();
    let declassify = tokio::spawn(async move {
        sqlx::query(
            "UPDATE items SET is_sensitive = false \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(provider_wins_item_id)
        .execute(&concurrent_pool)
        .await
    });
    wait_for_postgres_blocker(pool, provider_wins_pid).await;
    provider_wins
        .commit()
        .await
        .expect("commit provider sensitivity floor");
    let declassify_error = declassify
        .await
        .expect("declassification task")
        .expect_err("declassification must fail after provider transaction commits");
    let declassify_code = declassify_error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    assert_eq!(declassify_code.as_deref(), Some("23514"));
    let provider_wins_state: (bool, bool) = sqlx::query_as(
        "SELECT item.is_sensitive, mapping.provider_forced_sensitive \
         FROM items item JOIN provider_sync_mappings mapping \
           ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
         WHERE item.workspace_id = $1 AND item.id = $2 AND mapping.id = $3",
    )
    .bind(scope.workspace_id)
    .bind(provider_wins_item_id)
    .bind(provider_wins_mapping_id)
    .fetch_one(pool)
    .await
    .expect("provider-wins invariant state");
    assert_eq!(provider_wins_state, (true, true));
    let hard_delete = sqlx::query("DELETE FROM items WHERE workspace_id = $1 AND id = $2")
        .bind(scope.workspace_id)
        .bind(provider_wins_item_id)
        .execute(pool)
        .await
        .expect_err("active provider sensitivity floor must reject a hard delete");
    let hard_delete_code = hard_delete
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    assert_eq!(hard_delete_code.as_deref(), Some("23503"));

    // The opposite order is safe too: the local item mutation owns the item
    // row, so the provider trigger waits without taking locks in reverse order,
    // then observes the committed non-sensitive value and rejects its floor.
    let mut item_wins = pool.begin().await.expect("item-wins transaction");
    let item_wins_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *item_wins)
        .await
        .expect("item-wins backend pid");
    sqlx::query("UPDATE items SET is_sensitive = false WHERE workspace_id = $1 AND id = $2")
        .bind(scope.workspace_id)
        .bind(item_wins_item_id)
        .execute(&mut *item_wins)
        .await
        .expect("local path declassifies before provider floor");
    let concurrent_pool = (*pool).clone();
    let force_sensitive = tokio::spawn(async move {
        sqlx::query(
            "UPDATE provider_sync_mappings SET provider_forced_sensitive = true \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(item_wins_mapping_id)
        .execute(&concurrent_pool)
        .await
    });
    wait_for_postgres_blocker(pool, item_wins_pid).await;
    item_wins
        .commit()
        .await
        .expect("commit local declassification");
    let force_error = force_sensitive
        .await
        .expect("provider floor task")
        .expect_err("provider floor must fail after local transaction commits");
    let force_code = force_error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    assert_eq!(force_code.as_deref(), Some("23514"));
    let item_wins_state: (bool, bool) = sqlx::query_as(
        "SELECT item.is_sensitive, mapping.provider_forced_sensitive \
         FROM items item JOIN provider_sync_mappings mapping \
           ON mapping.workspace_id = item.workspace_id AND mapping.local_entity_id = item.id \
         WHERE item.workspace_id = $1 AND item.id = $2 AND mapping.id = $3",
    )
    .bind(scope.workspace_id)
    .bind(item_wins_item_id)
    .bind(item_wins_mapping_id)
    .fetch_one(pool)
    .await
    .expect("item-wins invariant state");
    assert_eq!(item_wins_state, (false, false));

    test_database.destroy().await;
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

fn google_idempotency(
    namespace: &'static str,
    key_marker: u8,
    fingerprint_marker: u8,
    now: chrono::DateTime<Utc>,
) -> OAuthIdempotency {
    OAuthIdempotency {
        namespace,
        key_hash: [key_marker; 32],
        request_fingerprint: [fingerprint_marker; 32],
        expires_at: now + ChronoDuration::days(1),
    }
}

fn google_session(
    id: Uuid,
    state_marker: u8,
    ciphertext_marker: u8,
    now: chrono::DateTime<Utc>,
) -> NewOAuthSession {
    NewOAuthSession {
        id,
        owner_subject_hash: [4_u8; 32],
        state_hash: [state_marker; 32],
        encrypted_verifier: SealedSecret {
            key_version: 1,
            ciphertext: vec![ciphertext_marker; 48],
        },
        encrypted_authorization_url: SealedSecret {
            key_version: 1,
            ciphertext: vec![ciphertext_marker.wrapping_add(1); 96],
        },
        requested_scopes: [
            "https://www.googleapis.com/auth/calendar".to_owned(),
            "https://www.googleapis.com/auth/tasks".to_owned(),
        ]
        .into_iter()
        .collect(),
        expected_account_id: None,
        expected_account_revision: None,
        make_default: false,
        created_at: now,
        expires_at: now + ChronoDuration::minutes(10),
    }
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn postgres_expired_disconnect_idempotency_recovers_only_the_same_key() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool).await;
    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let now = Utc::now();
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', \
         'expired-disconnect-user', 'expired-disconnect@example.test', $4, 1, \
         'active', true, true)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![73_u8; 64])
    .execute(pool)
    .await
    .expect("seed disconnect account");

    let original_key = google_idempotency("google.account.disconnect", 71, 81, now);
    let first_claim_id = Uuid::new_v4();
    let first = repository
        .claim_disconnect(
            account_id,
            1,
            first_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            original_key,
        )
        .await
        .expect("initial disconnect claimed");
    let DisconnectMutation::Execute(first) = first else {
        panic!("initial disconnect must execute");
    };
    repository
        .fail_disconnect(account_id, first_claim_id, first.credential_generation, now)
        .await
        .expect("failed revocation retains its exact fence");

    let recovery_now = now + ChronoDuration::days(1) + ChronoDuration::seconds(1);
    let different_key = google_idempotency("google.account.disconnect", 72, 81, recovery_now);
    assert!(matches!(
        repository
            .claim_disconnect(
                account_id,
                1,
                Uuid::new_v4(),
                recovery_now,
                recovery_now - ChronoDuration::minutes(2),
                recovery_now - ChronoDuration::minutes(2),
                different_key.clone(),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    let different_key_recorded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = $2 AND key_hash = $3)",
    )
    .bind(scope.workspace_id)
    .bind(different_key.namespace)
    .bind(different_key.key_hash.as_slice())
    .fetch_one(pool)
    .await
    .expect("different-key lookup");
    assert!(!different_key_recorded);

    let recovered_key = google_idempotency("google.account.disconnect", 71, 81, recovery_now);
    let retry_claim_id = Uuid::new_v4();
    let recovered = repository
        .claim_disconnect(
            account_id,
            1,
            retry_claim_id,
            recovery_now,
            recovery_now - ChronoDuration::minutes(2),
            recovery_now - ChronoDuration::minutes(2),
            recovered_key.clone(),
        )
        .await
        .expect("same key reconstructs the expired retry record");
    let DisconnectMutation::Execute(recovered) = recovered else {
        panic!("recovered disconnect must execute");
    };
    let reconstructed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM idempotency_keys WHERE workspace_id = $1 \
         AND namespace = $2 AND key_hash = $3 AND state = 'in_progress' \
         AND resource_type = 'google_disconnect' AND resource_id = $4)",
    )
    .bind(scope.workspace_id)
    .bind(recovered_key.namespace)
    .bind(recovered_key.key_hash.as_slice())
    .bind(account_id)
    .fetch_one(pool)
    .await
    .expect("reconstructed idempotency lookup");
    assert!(reconstructed);

    let revoked = repository
        .complete_disconnect(
            account_id,
            retry_claim_id,
            recovered.credential_generation,
            recovery_now,
            recovered_key,
        )
        .await
        .expect("recovered disconnect completes");
    assert_eq!(revoked.account.status, GoogleAccountStatus::Revoked);

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn postgres_google_oauth_is_fenced_recoverable_scoped_and_idempotent() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool).await;
    let other_scope = seed_other_scope(pool).await;
    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let other_repository = PostgresGoogleOAuthRepository::new(pool.clone(), other_scope);
    let now = Utc::now();
    let exchange_stale_before = now - ChronoDuration::minutes(2);
    let first_session = google_session(Uuid::new_v4(), 3, 9, now);
    let first_start_key = google_idempotency("google_oauth_start", 1, 11, now);
    let first_started = repository
        .create_session(
            first_session.clone(),
            first_start_key.clone(),
            exchange_stale_before,
        )
        .await
        .expect("first session created");
    assert!(!first_started.replayed);
    let replayed_start = repository
        .create_session(
            first_session.clone(),
            first_start_key.clone(),
            exchange_stale_before,
        )
        .await
        .expect("start replayed");
    assert!(replayed_start.replayed);
    assert_eq!(replayed_start.id, first_started.id);
    assert_eq!(
        replayed_start.encrypted_authorization_url.ciphertext,
        first_started.encrypted_authorization_url.ciphertext
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 4, 10, now),
                google_idempotency("google_oauth_start", 1, 12, now),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::IdempotencyConflict)
    ));

    // A newer pending flow atomically supersedes the older one. Once exchange
    // starts, a third flow is rejected so only one refresh credential can be issued.
    let session_id = Uuid::new_v4();
    let state_hash = [5_u8; 32];
    repository
        .create_session(
            google_session(session_id, 5, 11, now),
            google_idempotency("google_oauth_start", 2, 13, now),
            exchange_stale_before,
        )
        .await
        .expect("newest pending session created");
    assert!(matches!(
        repository
            .claim_callback(first_session.state_hash, now, exchange_stale_before)
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));

    let (first, second) = tokio::join!(
        repository.claim_callback(state_hash, now, exchange_stale_before),
        repository.claim_callback(state_hash, now, exchange_stale_before)
    );
    assert!(first.is_ok() ^ second.is_ok());
    let CallbackClaim::Exchange(claimed) = first.or(second).expect("one callback claim") else {
        panic!("pending callback must claim the exchange lease");
    };
    assert!(matches!(
        repository
            .claim_callback(state_hash, now, exchange_stale_before)
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 6, 12, now),
                google_idempotency("google_oauth_start", 3, 14, now),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::AuthorizationInProgress)
    ));

    repository
        .hold_cleanup_token(
            session_id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![77; 64],
            },
            now,
        )
        .await
        .expect("new refresh token durably held before staging");

    let account_id = Uuid::new_v4();
    repository
        .stage_authorization(AuthorizationCompletion {
            session_id,
            owner_subject_hash: claimed.owner_subject_hash,
            expected_account_revision: None,
            account_id,
            make_default: false,
            external_account_id: "google-user-postgres".to_owned(),
            display_label: "owner@example.test".to_owned(),
            credentials: EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 1,
                    ciphertext: vec![8; 64],
                },
            },
            granted_scopes: claimed.requested_scopes,
            token_expires_at: now + ChronoDuration::hours(1),
            now,
        })
        .await
        .expect("authorization durably staged");
    assert!(matches!(
        repository
            .claim_callback(state_hash, now, exchange_stale_before)
            .await,
        Ok(CallbackClaim::Staged {
            session_id: staged_id
        }) if staged_id == session_id
    ));
    assert!(matches!(
        repository
            .resolve_authorization(session_id)
            .await
            .expect("exact staged resolution"),
        AuthorizationResolution::Staged
    ));
    let (first_consumer, concurrent_consumer) = tokio::join!(
        repository.complete_staged_authorization(session_id),
        repository.complete_staged_authorization(session_id)
    );
    let account = first_consumer.expect("first staged consumer");
    assert_eq!(
        concurrent_consumer.expect("concurrent consumer gets installed account"),
        account
    );
    assert!(matches!(
        repository
            .resolve_authorization(session_id)
            .await
            .expect("exact consumed resolution"),
        AuthorizationResolution::Consumed(ref installed) if *installed == account
    ));
    assert_eq!(
        repository
            .cleanup_status()
            .await
            .expect("held cleanup removed with installation")
            .held,
        0
    );
    assert_eq!(account.status, GoogleAccountStatus::Active);
    assert_eq!(account.external_account_id, "google-user-postgres");
    let staged_secrets_scrubbed: bool = sqlx::query_scalar(
        "SELECT encrypted_pkce_verifier IS NULL AND verifier_key_version IS NULL \
         AND staged_encrypted_credentials IS NULL AND staged_credential_key_version IS NULL \
         FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(pool)
    .await
    .expect("PKCE scrub check");
    assert!(staged_secrets_scrubbed);
    assert!(matches!(
        repository
            .claim_callback(state_hash, now, exchange_stale_before)
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));
    assert!(
        other_repository
            .account()
            .await
            .expect("other scope")
            .is_none()
    );

    let reconnect_id = Uuid::new_v4();
    let reconnect_state = [16_u8; 32];
    let mut reconnect_session = google_session(reconnect_id, 16, 17, now);
    reconnect_session.expected_account_id = Some(account.id);
    reconnect_session.expected_account_revision = Some(account.revision);
    repository
        .create_session(
            reconnect_session,
            google_idempotency("google_oauth_start", 16, 16, now),
            exchange_stale_before,
        )
        .await
        .expect("reauthorization session created");
    let CallbackClaim::Exchange(reconnect_claim) = repository
        .claim_callback(reconnect_state, now, exchange_stale_before)
        .await
        .expect("reauthorization claimed")
    else {
        panic!("reauthorization must claim exchange");
    };
    repository
        .stage_authorization(AuthorizationCompletion {
            session_id: reconnect_id,
            owner_subject_hash: reconnect_claim.owner_subject_hash,
            expected_account_revision: Some(account.revision),
            account_id: account.id,
            make_default: false,
            external_account_id: account.external_account_id.clone(),
            display_label: "updated@example.test".to_owned(),
            credentials: EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 1,
                    ciphertext: vec![18; 64],
                },
            },
            granted_scopes: reconnect_claim.requested_scopes.clone(),
            token_expires_at: now + ChronoDuration::hours(2),
            now,
        })
        .await
        .expect("reauthorization staged");
    let account = repository
        .complete_staged_authorization(reconnect_id)
        .await
        .expect("reauthorization installed");
    assert_eq!(account.revision, 2);
    assert_eq!(account.display_label, "updated@example.test");

    let second_account_id = Uuid::new_v4();
    let second_session_id = Uuid::new_v4();
    let second_state = [60_u8; 32];
    repository
        .create_session(
            google_session(second_session_id, 60, 61, now),
            google_idempotency("google_oauth_start", 60, 60, now),
            exchange_stale_before,
        )
        .await
        .expect("second identity session created");
    let CallbackClaim::Exchange(second_claim) = repository
        .claim_callback(second_state, now, exchange_stale_before)
        .await
        .expect("second identity exchange claimed")
    else {
        panic!("second identity must claim exchange");
    };
    repository
        .stage_authorization(AuthorizationCompletion {
            session_id: second_session_id,
            owner_subject_hash: second_claim.owner_subject_hash,
            expected_account_revision: None,
            account_id: second_account_id,
            make_default: false,
            external_account_id: "google-user-postgres-two".to_owned(),
            display_label: "second@example.test".to_owned(),
            credentials: EncryptedCredentials {
                sealed: SealedSecret {
                    key_version: 1,
                    ciphertext: vec![61; 64],
                },
            },
            granted_scopes: second_claim.requested_scopes,
            token_expires_at: now + ChronoDuration::hours(1),
            now,
        })
        .await
        .expect("second identity staged");
    let second_account = repository
        .complete_staged_authorization(second_session_id)
        .await
        .expect("second identity installed");
    assert_eq!(second_account.id, second_account_id);
    assert!(!second_account.is_default);
    let all_accounts = repository.accounts().await.expect("multi-account list");
    assert_eq!(all_accounts.len(), 2);
    assert_eq!(
        all_accounts
            .iter()
            .filter(|snapshot| snapshot.account.is_default)
            .count(),
        1
    );
    assert_eq!(all_accounts[0].account.id, account.id);

    let pause_key = google_idempotency("google_oauth_pause", 21, 31, now);
    let paused = repository
        .set_paused(
            account.id,
            account.revision,
            true,
            now,
            exchange_stale_before,
            pause_key.clone(),
        )
        .await
        .expect("paused");
    assert!(!paused.replayed);
    let paused_replay = repository
        .set_paused(
            account.id,
            account.revision,
            true,
            now,
            exchange_stale_before,
            pause_key.clone(),
        )
        .await
        .expect("pause replayed");
    assert!(paused_replay.replayed);
    assert_eq!(paused_replay.account, paused.account);
    assert!(matches!(
        repository
            .set_paused(
                account.id,
                account.revision,
                true,
                now,
                exchange_stale_before,
                google_idempotency("google_oauth_pause", 21, 99, now),
            )
            .await,
        Err(GoogleOAuthRepositoryError::IdempotencyConflict)
    ));
    let resume_key = google_idempotency("google_oauth_resume", 22, 32, now);
    let resumed = repository
        .set_paused(
            paused.account.id,
            paused.account.revision,
            false,
            now,
            exchange_stale_before,
            resume_key.clone(),
        )
        .await
        .expect("resumed");
    let resumed_replay = repository
        .set_paused(
            paused.account.id,
            paused.account.revision,
            false,
            now,
            exchange_stale_before,
            resume_key,
        )
        .await
        .expect("resume replayed");
    assert!(resumed_replay.replayed);
    assert_eq!(resumed_replay.account, resumed.account);
    let pause_replay_after_newer_mutation = repository
        .set_paused(
            account.id,
            account.revision,
            true,
            now,
            exchange_stale_before,
            pause_key,
        )
        .await
        .expect("historical response replayed exactly");
    assert_eq!(pause_replay_after_newer_mutation.account, paused.account);

    let first_claim_id = Uuid::new_v4();
    let disconnect_key = google_idempotency("google_oauth_disconnect", 23, 33, now);
    let disconnect = repository
        .claim_disconnect(
            resumed.account.id,
            resumed.account.revision,
            first_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            exchange_stale_before,
            disconnect_key.clone(),
        )
        .await
        .expect("disconnect claimed");
    let DisconnectMutation::Execute(disconnect) = disconnect else {
        panic!("first disconnect must execute");
    };
    assert_eq!(disconnect.credentials.sealed.ciphertext, vec![18; 64]);
    assert_eq!(disconnect.protected_accounts.len(), 2);
    assert!(
        repository
            .cleanup_status()
            .await
            .expect("disconnect fence status")
            .revocation_fenced
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 71, 72, now),
                google_idempotency("google_oauth_start", 71, 71, now),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    assert!(matches!(
        repository
            .claim_volatile_revocation(
                Uuid::new_v4(),
                Uuid::new_v4(),
                now,
                now - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    assert!(matches!(
        repository
            .complete_disconnect(
                resumed.account.id,
                first_claim_id,
                disconnect.credential_generation + 1,
                now,
                disconnect_key.clone(),
            )
            .await,
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    ));
    assert_eq!(
        repository
            .fail_disconnect(
                resumed.account.id,
                first_claim_id,
                disconnect.credential_generation + 1,
                now,
            )
            .await,
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    );
    repository
        .fail_disconnect(
            resumed.account.id,
            first_claim_id,
            disconnect.credential_generation,
            now,
        )
        .await
        .expect("revocation failure retained");
    let retained = repository
        .account()
        .await
        .expect("account lookup")
        .expect("credentials retained");
    assert_eq!(
        retained.account.status,
        GoogleAccountStatus::RevocationFailed
    );
    assert_eq!(retained.credentials.sealed.ciphertext, vec![18; 64]);

    let retry_claim_id = Uuid::new_v4();
    let retried = repository
        .claim_disconnect(
            resumed.account.id,
            resumed.account.revision,
            retry_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            exchange_stale_before,
            disconnect_key.clone(),
        )
        .await
        .expect("disconnect retried");
    let DisconnectMutation::Execute(retried) = retried else {
        panic!("disconnect retry must execute");
    };
    assert_eq!(retried.protected_accounts.len(), 2);
    let revoked = repository
        .complete_disconnect(
            retained.account.id,
            retry_claim_id,
            retried.credential_generation,
            now,
            disconnect_key.clone(),
        )
        .await
        .expect("disconnect completed");
    assert_eq!(revoked.account.status, GoogleAccountStatus::Revoked);
    assert!(!revoked.replayed);
    let disconnect_replay = repository
        .claim_disconnect(
            retained.account.id,
            resumed.account.revision,
            Uuid::new_v4(),
            now,
            now - ChronoDuration::minutes(2),
            exchange_stale_before,
            disconnect_key,
        )
        .await
        .expect("completed disconnect replayed");
    assert!(matches!(
        disconnect_replay,
        DisconnectMutation::Replay(ref account) if *account == revoked.account
    ));
    let promoted = repository
        .account()
        .await
        .expect("account lookup")
        .expect("remaining identity promoted deterministically");
    assert_eq!(promoted.account.id, second_account.id);
    assert!(promoted.account.is_default);
    assert_eq!(promoted.account.revision, second_account.revision + 1);
    let credentials_scrubbed: bool = sqlx::query_scalar(
        "SELECT encrypted_credentials IS NULL AND credential_key_version IS NULL \
         FROM provider_accounts WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(account_id)
    .fetch_one(pool)
    .await
    .expect("credential scrub check");
    assert!(credentials_scrubbed);

    let expired_id = Uuid::new_v4();
    let mut expired_session = google_session(expired_id, 40, 41, now);
    expired_session.expires_at = now + ChronoDuration::minutes(1);
    repository
        .create_session(
            expired_session.clone(),
            google_idempotency("google_oauth_start", 40, 40, now),
            exchange_stale_before,
        )
        .await
        .expect("expiring session created");
    assert!(matches!(
        repository
            .claim_callback(
                expired_session.state_hash,
                now + ChronoDuration::minutes(2),
                exchange_stale_before,
            )
            .await,
        Err(GoogleOAuthRepositoryError::InvalidCallbackState)
    ));
    let expired_secrets_scrubbed: bool = sqlx::query_scalar(
        "SELECT encrypted_pkce_verifier IS NULL AND verifier_key_version IS NULL \
         AND encrypted_authorization_url IS NULL AND authorization_url_key_version IS NULL \
         FROM google_oauth_sessions WHERE id = $1",
    )
    .bind(expired_id)
    .fetch_one(pool)
    .await
    .expect("expired session scrub check");
    assert!(expired_secrets_scrubbed);

    // A process crash after claiming exchange cannot wedge the scope forever.
    let stale_session = google_session(Uuid::new_v4(), 50, 51, now);
    repository
        .create_session(
            stale_session.clone(),
            google_idempotency("google_oauth_start", 50, 50, now),
            exchange_stale_before,
        )
        .await
        .expect("stale exchange fixture created");
    repository
        .claim_callback(stale_session.state_hash, now, exchange_stale_before)
        .await
        .expect("stale exchange fixture claimed");
    repository
        .hold_cleanup_token(
            stale_session.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![88; 64],
            },
            now,
        )
        .await
        .expect("post-exchange refresh survives process crash");
    repository
        .identify_cleanup_token(stale_session.id, "google-orphan-postgres", now)
        .await
        .expect("verified orphan identity is retained with cleanup custody");
    let recovery_time = now + ChronoDuration::minutes(3);
    repository
        .recover_startup(recovery_time, recovery_time - ChronoDuration::minutes(2))
        .await
        .expect("startup explicitly recovers stale exchange custody");
    repository
        .create_session(
            google_session(Uuid::new_v4(), 51, 52, recovery_time),
            google_idempotency("google_oauth_start", 51, 51, recovery_time),
            recovery_time - ChronoDuration::minutes(2),
        )
        .await
        .expect("stale exchange lease recovered");
    let stale_status: String =
        sqlx::query_scalar("SELECT status FROM google_oauth_sessions WHERE id = $1")
            .bind(stale_session.id)
            .fetch_one(pool)
            .await
            .expect("stale session status");
    assert_eq!(stale_status, "failed");
    let cleanup_status = repository
        .cleanup_status()
        .await
        .expect("stale held cleanup becomes retryable");
    assert_eq!(cleanup_status.held, 0);
    assert_eq!(cleanup_status.pending, 1);
    let first_cleanup_claim_id = Uuid::new_v4();
    let first_cleanup = repository
        .claim_cleanup(
            first_cleanup_claim_id,
            recovery_time,
            recovery_time - ChronoDuration::minutes(2),
            recovery_time - ChronoDuration::minutes(2),
            None,
        )
        .await
        .expect("cleanup claim")
        .expect("cleanup credential retained");
    assert_eq!(first_cleanup.session_id, stale_session.id);
    assert_eq!(first_cleanup.attempt, 1);
    assert_eq!(
        first_cleanup.encrypted_refresh_token.ciphertext,
        vec![88; 64]
    );
    repository
        .fail_cleanup(
            stale_session.id,
            first_cleanup_claim_id,
            first_cleanup.credential_generation,
            recovery_time,
            recovery_time + ChronoDuration::seconds(1),
        )
        .await
        .expect("failed revocation remains pending");
    let failed_cleanup_status = repository
        .cleanup_status()
        .await
        .expect("failed cleanup status");
    assert_eq!(failed_cleanup_status.pending, 1);
    assert_eq!(failed_cleanup_status.last_failure_at, Some(recovery_time));
    let second_cleanup_claim_id = Uuid::new_v4();
    let second_cleanup = repository
        .claim_cleanup(
            second_cleanup_claim_id,
            recovery_time + ChronoDuration::seconds(1),
            recovery_time - ChronoDuration::minutes(2),
            recovery_time - ChronoDuration::minutes(2),
            None,
        )
        .await
        .expect("cleanup retry claim")
        .expect("failed cleanup remains retryable");
    assert_eq!(second_cleanup.attempt, 2);
    repository
        .complete_cleanup(
            stale_session.id,
            second_cleanup_claim_id,
            second_cleanup.credential_generation,
            recovery_time + ChronoDuration::seconds(1),
        )
        .await
        .expect("successful or invalid-token revocation clears cleanup");
    let cleanup_status = repository.cleanup_status().await.expect("cleanup cleared");
    assert_eq!(
        cleanup_status.held + cleanup_status.pending + cleanup_status.retrying,
        0
    );

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn postgres_cleanup_fence_serializes_claim_backoff_and_all_install_phases() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    MIGRATOR.run(pool).await.expect("migrations apply");
    let scope = seed_scope(pool).await;
    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let now = Utc::now();

    for (id, external, is_default, marker) in [
        (Uuid::new_v4(), "google-one", true, 31_u8),
        (Uuid::new_v4(), "google-two", false, 32_u8),
    ] {
        sqlx::query(
            "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
             external_account_id, display_label, encrypted_credentials, credential_key_version, \
             status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, $4, $5, 1, \
             'active', true, $6)",
        )
        .bind(id)
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(external)
        .bind(vec![marker; 64])
        .bind(is_default)
        .execute(pool)
        .await
        .expect("seed protected account");
    }

    let race_account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', 'google-race', \
         'google-race', $4, 1, 'active', true, false)",
    )
    .bind(race_account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![33_u8; 64])
    .execute(pool)
    .await
    .expect("seed race account");
    let race_session = google_session(Uuid::new_v4(), 88, 89, now);
    let race_claim_id = Uuid::new_v4();
    let race_disconnect_key = google_idempotency("google_oauth_disconnect", 88, 88, now);
    let disconnect_repository = repository.clone();
    let connect_repository = repository.clone();
    let (disconnect_race, connect_race) = tokio::join!(
        disconnect_repository.claim_disconnect(
            race_account_id,
            1,
            race_claim_id,
            now,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            race_disconnect_key.clone(),
        ),
        connect_repository.create_session(
            race_session.clone(),
            google_idempotency("google_oauth_start", 88, 89, now),
            now - ChronoDuration::minutes(2),
        ),
    );
    let disconnect_claim = match (disconnect_race, connect_race) {
        (
            Ok(DisconnectMutation::Execute(claim)),
            Err(GoogleOAuthRepositoryError::RevocationInProgress),
        ) => claim,
        (Err(GoogleOAuthRepositoryError::AuthorizationInProgress), Ok(_)) => {
            repository
                .claim_callback(
                    race_session.state_hash,
                    now,
                    now - ChronoDuration::minutes(2),
                )
                .await
                .expect("winning connect session is claimable");
            repository
                .fail_authorization(race_session.id, now)
                .await
                .expect("clear winning connect session");
            let DisconnectMutation::Execute(claim) = repository
                .claim_disconnect(
                    race_account_id,
                    1,
                    race_claim_id,
                    now,
                    now - ChronoDuration::minutes(2),
                    now - ChronoDuration::minutes(2),
                    race_disconnect_key.clone(),
                )
                .await
                .expect("disconnect proceeds after connect is closed")
            else {
                panic!("disconnect executes");
            };
            claim
        }
        outcome => panic!("connect/disconnect must serialize exclusively: {outcome:?}"),
    };
    assert_eq!(disconnect_claim.protected_accounts.len(), 3);
    repository
        .complete_disconnect(
            race_account_id,
            disconnect_claim.claim_id,
            disconnect_claim.credential_generation,
            now,
            race_disconnect_key,
        )
        .await
        .expect("race account disconnect completes under its exact fence");

    let session = google_session(Uuid::new_v4(), 91, 92, now);
    repository
        .create_session(
            session.clone(),
            google_idempotency("google_oauth_start", 91, 91, now),
            now - ChronoDuration::minutes(2),
        )
        .await
        .expect("create cleanup source session");
    let CallbackClaim::Exchange(claimed) = repository
        .claim_callback(session.state_hash, now, now - ChronoDuration::minutes(2))
        .await
        .expect("claim callback")
    else {
        panic!("new session exchanges");
    };
    repository
        .hold_cleanup_token(
            claimed.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![93; 64],
            },
            now,
        )
        .await
        .expect("hold cleanup");
    repository
        .abandon_authorization(claimed.id, now)
        .await
        .expect("promote cleanup");
    let first = repository
        .claim_cleanup(
            Uuid::new_v4(),
            now,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            Some(claimed.id),
        )
        .await
        .expect("claim cleanup")
        .expect("pending cleanup");
    assert_eq!(first.protected_accounts.len(), 2);
    assert!(matches!(
        repository
            .claim_volatile_revocation(
                claimed.id,
                Uuid::new_v4(),
                now,
                now - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));

    let create_repository = repository.clone();
    let complete_repository = repository.clone();
    let stage_repository = repository.clone();
    let blocked_session = google_session(Uuid::new_v4(), 94, 95, now);
    let dummy_completion = AuthorizationCompletion {
        session_id: claimed.id,
        owner_subject_hash: [4; 32],
        expected_account_revision: None,
        account_id: Uuid::new_v4(),
        make_default: false,
        external_account_id: "unused".to_owned(),
        display_label: "unused".to_owned(),
        credentials: EncryptedCredentials {
            sealed: SealedSecret {
                key_version: 1,
                ciphertext: vec![96; 64],
            },
        },
        granted_scopes: BTreeSet::default(),
        token_expires_at: now + ChronoDuration::hours(1),
        now,
    };
    let (create_result, complete_result, stage_result) = tokio::join!(
        create_repository.create_session(
            blocked_session,
            google_idempotency("google_oauth_start", 94, 94, now),
            now - ChronoDuration::minutes(2),
        ),
        complete_repository.complete_staged_authorization(claimed.id),
        stage_repository.stage_authorization(dummy_completion),
    );
    assert!(matches!(
        create_result,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    assert_eq!(
        complete_result,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    );
    assert_eq!(
        stage_result,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    );

    let retry_at = now + ChronoDuration::seconds(5);
    repository
        .fail_cleanup(
            first.session_id,
            first.claim_id,
            first.credential_generation,
            now,
            retry_at,
        )
        .await
        .expect("ambiguous revoke retains fence");
    assert!(
        repository
            .claim_cleanup(
                Uuid::new_v4(),
                now + ChronoDuration::seconds(4),
                now - ChronoDuration::minutes(2),
                now - ChronoDuration::minutes(2),
                None,
            )
            .await
            .expect("backoff query")
            .is_none()
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 97, 98, now),
                google_idempotency("google_oauth_start", 97, 97, now),
                now - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    let second = repository
        .claim_cleanup(
            Uuid::new_v4(),
            retry_at,
            now - ChronoDuration::minutes(2),
            now - ChronoDuration::minutes(2),
            None,
        )
        .await
        .expect("due retry")
        .expect("same fenced cleanup is retryable");
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.attempt, 2);
    repository
        .complete_cleanup(
            second.session_id,
            second.claim_id,
            second.credential_generation,
            retry_at,
        )
        .await
        .expect("definitive provider result clears fence");
    let guardian_session = google_session(Uuid::new_v4(), 99, 100, retry_at);
    repository
        .create_session(
            guardian_session.clone(),
            google_idempotency("google_oauth_start", 99, 99, retry_at),
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("installs resume after fence is atomically cleared");
    repository
        .claim_callback(
            guardian_session.state_hash,
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("guardian source exchanges");
    let first_guardian = repository
        .claim_volatile_revocation(
            guardian_session.id,
            Uuid::new_v4(),
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("guardian acquires durable scope fence");
    assert_eq!(first_guardian.protected_accounts.len(), 2);
    assert!(matches!(
        repository
            .claim_volatile_revocation(
                guardian_session.id,
                Uuid::new_v4(),
                retry_at,
                retry_at - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    let guardian_recovery_at = retry_at + ChronoDuration::minutes(3);
    let replacement_guardian = repository
        .claim_volatile_revocation(
            guardian_session.id,
            Uuid::new_v4(),
            guardian_recovery_at,
            guardian_recovery_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("a crashed guardian lease is stolen only after its bounded timeout");
    assert_eq!(
        replacement_guardian.credential_generation,
        first_guardian.credential_generation
    );
    assert_eq!(
        repository
            .release_volatile_revocation(
                guardian_session.id,
                first_guardian.claim_id,
                first_guardian.credential_generation,
            )
            .await,
        Err(GoogleOAuthRepositoryError::CleanupClaimLost)
    );
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 101, 102, retry_at),
                google_idempotency("google_oauth_start", 101, 101, retry_at),
                retry_at - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));
    repository
        .hold_cleanup_token(
            guardian_session.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![103; 64],
            },
            guardian_recovery_at,
        )
        .await
        .expect("owning guardian may transfer token to durable hold");
    repository
        .release_volatile_revocation(
            guardian_session.id,
            replacement_guardian.claim_id,
            replacement_guardian.credential_generation,
        )
        .await
        .expect("exact guardian claim releases fence without deleting hold");
    repository
        .release_volatile_revocation(
            guardian_session.id,
            replacement_guardian.claim_id,
            replacement_guardian.credential_generation,
        )
        .await
        .expect("lost release acknowledgement replays exactly");
    repository
        .abandon_authorization(guardian_session.id, guardian_recovery_at)
        .await
        .expect("durable hold becomes pending");
    let durable = repository
        .claim_cleanup(
            Uuid::new_v4(),
            guardian_recovery_at,
            guardian_recovery_at - ChronoDuration::minutes(2),
            guardian_recovery_at - ChronoDuration::minutes(2),
            Some(guardian_session.id),
        )
        .await
        .expect("durable cleanup claim")
        .expect("released guardian retained durable token");
    repository
        .complete_cleanup(
            durable.session_id,
            durable.claim_id,
            durable.credential_generation,
            guardian_recovery_at,
        )
        .await
        .expect("durable recovery cleanup completes");

    let definitive_session = google_session(Uuid::new_v4(), 104, 105, retry_at);
    repository
        .create_session(
            definitive_session.clone(),
            google_idempotency("google_oauth_start", 104, 104, retry_at),
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("definitive guardian session");
    repository
        .claim_callback(
            definitive_session.state_hash,
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("definitive guardian exchange");
    repository
        .hold_cleanup_token(
            definitive_session.id,
            SealedSecret {
                key_version: 1,
                ciphertext: vec![106; 64],
            },
            retry_at,
        )
        .await
        .expect("simulate lost hold acknowledgement");
    let definitive = repository
        .claim_volatile_revocation(
            definitive_session.id,
            Uuid::new_v4(),
            retry_at,
            retry_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("definitive revocation fence");
    repository
        .complete_volatile_revocation(
            definitive_session.id,
            definitive.claim_id,
            definitive.credential_generation,
        )
        .await
        .expect("definitive provider result atomically clears ambiguous hold and fence");
    repository
        .complete_volatile_revocation(
            definitive_session.id,
            definitive.claim_id,
            definitive.credential_generation,
        )
        .await
        .expect("lost definitive acknowledgement replays exactly");
    let definitive_hold_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 \
         AND user_id = $2 AND session_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(definitive_session.id)
    .fetch_one(pool)
    .await
    .expect("definitive hold count");
    assert_eq!(definitive_hold_count, 0);
    assert!(
        !repository
            .cleanup_status()
            .await
            .expect("guardian fence cleared")
            .revocation_fenced
    );

    let ambiguous_at = guardian_recovery_at + ChronoDuration::minutes(5);
    let ambiguous_session = google_session(Uuid::new_v4(), 107, 108, ambiguous_at);
    repository
        .create_session(
            ambiguous_session.clone(),
            google_idempotency("google_oauth_start", 107, 107, ambiguous_at),
            ambiguous_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("ambiguous guardian session");
    repository
        .claim_callback(
            ambiguous_session.state_hash,
            ambiguous_at,
            ambiguous_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("ambiguous exchange claimed");
    repository
        .claim_volatile_revocation(
            ambiguous_session.id,
            Uuid::new_v4(),
            ambiguous_at,
            ambiguous_at - ChronoDuration::minutes(2),
        )
        .await
        .expect("guardian fence is durable before in-memory custody");
    let restart_at = ambiguous_at + ChronoDuration::minutes(3);
    repository
        .recover_startup(restart_at, restart_at - ChronoDuration::minutes(2))
        .await
        .expect("startup converts an identity-unknown guardian into recovery");
    let ambiguous_status = repository.cleanup_status().await.expect("recovery status");
    assert!(ambiguous_status.operator_recovery_required);
    assert!(ambiguous_status.revocation_fenced);
    assert_eq!(ambiguous_status.uncertain_authorizations, 1);
    let readiness = Readiness::with_database(pool.clone(), scope.workspace_id, scope.user_id);
    readiness.set_ready(true);
    assert!(!readiness.check().await);
    let recovery = repository
        .acknowledge_operator_recovery(restart_at)
        .await
        .expect("externally revoked grants resolve ambiguous guardian custody");
    assert_eq!(recovery.accounts_marked_reauthorization_required, 2);
    assert_eq!(recovery.legacy_accounts_finalized, 0);
    assert!(readiness.check().await);

    test_database.destroy().await;
}

#[tokio::test]
#[ignore = "requires DAYWEAVE_TEST_DATABASE_URL; run with --include-ignored"]
#[allow(clippy::too_many_lines)]
async fn google_oauth_migration_quarantines_until_verified_operator_recovery() {
    let database_url = std::env::var("DAYWEAVE_TEST_DATABASE_URL")
        .expect("DAYWEAVE_TEST_DATABASE_URL is required for this ignored integration test");
    let test_database = TestDatabase::create(&database_url).await;
    let pool = &test_database.pool;
    for migration in [
        include_str!("../migrations/0001_identity_and_items.sql"),
        include_str!("../migrations/0002_schedule_sync_and_audit.sql"),
        include_str!("../migrations/0003_proposals_mcp_idempotency_outbox.sql"),
        include_str!("../migrations/0004_item_delta_sync.sql"),
        include_str!("../migrations/0005_execution_sessions.sql"),
    ] {
        pool.execute(migration).await.expect("legacy migration");
    }
    let scope = seed_scope(pool).await;
    let calendar_legacy_id = Uuid::new_v4();
    let tasks_legacy_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled) VALUES ($1, $2, $3, 'google_calendar', $4, 'Legacy Google', \
         $5, 1, 'active', true)",
    )
    .bind(calendar_legacy_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade")
    .bind(vec![1_u8; 64])
    .execute(pool)
    .await
    .expect("legacy Calendar account");
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled) VALUES ($1, $2, $3, 'google_tasks', $4, 'Legacy Google Tasks', \
         $5, 1, 'reauthorization_required', false)",
    )
    .bind(tasks_legacy_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade")
    .bind(vec![2_u8; 64])
    .execute(pool)
    .await
    .expect("legacy Tasks account");

    pool.execute(include_str!("../migrations/0006_google_oauth.sql"))
        .await
        .expect("Google OAuth migration");
    pool.execute(include_str!("../migrations/0007_google_sync.sql"))
        .await
        .expect("Google sync migration");
    let quarantined_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_accounts WHERE id = ANY($1) \
         AND provider IN ('google_calendar', 'google_tasks') \
         AND status = 'operator_recovery_required' AND NOT sync_enabled AND NOT is_default \
         AND encrypted_credentials IS NOT NULL AND credential_key_version IS NOT NULL \
         AND disconnected_at IS NULL",
    )
    .bind(vec![calendar_legacy_id, tasks_legacy_id])
    .fetch_one(pool)
    .await
    .expect("legacy rows remain non-terminal until verified recovery");
    assert_eq!(quarantined_count, 2);
    let preserved_legacy: Vec<(Uuid, String, Vec<u8>, i32)> = sqlx::query_as(
        "SELECT source_account_id, legacy_provider, encrypted_credentials, \
         credential_key_version FROM google_oauth_legacy_credential_quarantine \
         WHERE workspace_id = $1 AND user_id = $2 ORDER BY legacy_provider",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_all(pool)
    .await
    .expect("legacy opaque envelopes preserved in quarantine");
    assert_eq!(preserved_legacy.len(), 2);
    assert!(preserved_legacy.contains(&(
        calendar_legacy_id,
        "google_calendar".to_owned(),
        vec![1_u8; 64],
        1,
    )));
    assert!(preserved_legacy.contains(&(
        tasks_legacy_id,
        "google_tasks".to_owned(),
        vec![2_u8; 64],
        1,
    )));

    let repository = PostgresGoogleOAuthRepository::new(pool.clone(), scope);
    let readiness = Readiness::with_database(pool.clone(), scope.workspace_id, scope.user_id);
    readiness.set_ready(true);
    assert!(
        !readiness.check().await,
        "legacy credential custody must make readiness fail"
    );
    let cleanup = repository
        .cleanup_status()
        .await
        .expect("legacy recovery is visible");
    assert_eq!(cleanup.legacy_recovery_required, 2);
    assert!(cleanup.operator_recovery_required);
    assert!(cleanup.revocation_fenced);
    assert!(matches!(
        repository
            .create_session(
                google_session(Uuid::new_v4(), 110, 111, Utc::now()),
                google_idempotency("google_oauth_start", 110, 110, Utc::now()),
                Utc::now() - ChronoDuration::minutes(2),
            )
            .await,
        Err(GoogleOAuthRepositoryError::RevocationInProgress)
    ));

    let recovery = repository
        .acknowledge_operator_recovery(Utc::now())
        .await
        .expect("operator-confirmed external grant revocation finalizes quarantine");
    assert_eq!(recovery.accounts_marked_reauthorization_required, 0);
    assert_eq!(recovery.legacy_accounts_finalized, 2);
    let finalized_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_accounts WHERE id = ANY($1) AND provider = 'google' \
         AND status = 'revoked' AND NOT sync_enabled AND encrypted_credentials IS NULL \
         AND credential_key_version IS NULL AND cardinality(granted_scopes) = 0 \
         AND token_expires_at IS NULL AND disconnected_at IS NOT NULL",
    )
    .bind(vec![calendar_legacy_id, tasks_legacy_id])
    .fetch_one(pool)
    .await
    .expect("confirmed legacy credentials are scrubbed");
    assert_eq!(finalized_count, 2);
    let confirmed_quarantine: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM google_oauth_legacy_credential_quarantine \
         WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NOT NULL \
         AND encrypted_credentials IS NULL AND credential_key_version IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(pool)
    .await
    .expect("quarantine confirmation is durable");
    assert_eq!(confirmed_quarantine, 2);
    let cleanup = repository
        .cleanup_status()
        .await
        .expect("legacy recovery clears");
    assert_eq!(cleanup.legacy_recovery_required, 0);
    assert!(!cleanup.operator_recovery_required);
    assert!(!cleanup.revocation_fenced);
    assert!(
        readiness.check().await,
        "readiness recovers only after durable operator acknowledgement"
    );

    let reconnected_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, \
         'Reconnected Google', $5, 1, 'active', true, true)",
    )
    .bind(reconnected_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade")
    .bind(vec![3_u8; 64])
    .execute(pool)
    .await
    .expect("same external identity can reconnect after revocation");

    let second_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, \
         external_account_id, display_label, encrypted_credentials, credential_key_version, \
         status, sync_enabled, is_default) VALUES ($1, $2, $3, 'google', $4, \
         'Second Google', $5, 1, 'active', true, false)",
    )
    .bind(second_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind("google-user-upgrade-two")
    .bind(vec![4_u8; 64])
    .execute(pool)
    .await
    .expect("two active Google identities coexist after upgrade");
    let active_accounts: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_default) FROM provider_accounts \
         WHERE workspace_id = $1 AND user_id = $2 AND provider = 'google' \
         AND status <> 'revoked'",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(pool)
    .await
    .expect("multi-account migration state");
    assert_eq!(active_accounts, (2, 1));
    let duplicate_default = sqlx::query(
        "UPDATE provider_accounts SET is_default = true WHERE workspace_id = $1 AND id = $2",
    )
    .bind(scope.workspace_id)
    .bind(second_id)
    .execute(pool)
    .await;
    assert!(duplicate_default.is_err());

    test_database.destroy().await;
}

async fn insert_mcp_proposal(
    pool: &PgPool,
    scope: DatabaseScope,
    proposal_id: Uuid,
    created_at: chrono::DateTime<Utc>,
    payload: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO proposals (id, workspace_id, revision, submitted_by_subject, source, kind, \
         status, title, payload, created_at, updated_at, expires_at) VALUES ($1, $2, 1, \
         'migration-proof-test', 'external_mcp', 'create_item', 'pending', \
         'Migration proof proposal', $3, $4, $4, $5)",
    )
    .bind(proposal_id)
    .bind(scope.workspace_id)
    .bind(payload)
    .bind(created_at)
    .bind(created_at + ChronoDuration::days(1))
    .execute(pool)
    .await
    .expect("MCP proposal fixture");
}

#[allow(clippy::too_many_arguments)]
async fn insert_simulation_proof_receipt(
    pool: &PgPool,
    scope: DatabaseScope,
    proposal_id: Uuid,
    simulation_id: Uuid,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
    simulation_subject_hash: &[u8],
    request_digest: &[u8],
    request_hash: &[u8],
    revision_id: Uuid,
    evidence_hash: &[u8],
    compiled_payload_hash: &[u8],
    marker: u8,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO mcp_proposal_submissions (workspace_id, user_id, subject_hash, key_hash, \
         request_fingerprint, proposal_id, completed_at, simulation_id, simulation_subject_hash, \
         simulation_request_digest, simulation_request_hash, simulation_base_revision_id, \
         simulation_created_at, simulation_expires_at, simulation_evidence_schema, \
         simulation_evidence_hash, compilation_outcome, compiled_payload_hash, \
         proposal_payload_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         $12, $13, $14, 1, $15, 'actionable', $16, $16)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![marker; 32])
    .bind(vec![marker.wrapping_add(1); 32])
    .bind(vec![marker.wrapping_add(2); 32])
    .bind(proposal_id)
    .bind(created_at)
    .bind(simulation_id)
    .bind(simulation_subject_hash)
    .bind(request_digest)
    .bind(request_hash)
    .bind(revision_id)
    .bind(created_at)
    .bind(expires_at)
    .bind(evidence_hash)
    .bind(compiled_payload_hash)
    .execute(pool)
    .await
    .map(|_| ())
}

fn postgres_error_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned)
}

async fn insert_pre_v20_deferred_session<'e, E>(
    executor: E,
    scope: DatabaseScope,
    session_id: Uuid,
    item_id: Uuid,
    session_index: i32,
    terminal_at: chrono::DateTime<Utc>,
    move_start: chrono::DateTime<Utc>,
) where
    E: Executor<'e, Database = Postgres>,
{
    let move_end = move_start + ChronoDuration::minutes(30);
    let started_at = terminal_at - ChronoDuration::seconds(5);
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, observed_running_since, \
         paused_at, pause_until, pause_reason, move_start, move_end, ended_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, NULL, $4, NULL, $5, 'deferred', 1, 5, 5, $6, NULL, NULL, \
         NULL, NULL, NULL, $7, $8, $9, $6, $9)",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(session_index)
    .bind(Uuid::new_v4())
    .bind(started_at)
    .bind(move_start)
    .bind(move_end)
    .bind(terminal_at)
    .execute(executor)
    .await
    .expect("seed pre-v20 deferred session");
}

async fn insert_pre_v20_completed_session(
    pool: &PgPool,
    scope: DatabaseScope,
    session_id: Uuid,
    item_id: Uuid,
    session_index: i32,
    terminal_at: chrono::DateTime<Utc>,
) {
    let started_at = terminal_at - ChronoDuration::seconds(5);
    sqlx::query(
        "INSERT INTO execution_sessions (id, workspace_id, item_id, item_revision, \
         occurrence_id, session_index, planned_block_id, source_device_id, state, revision, \
         accumulated_seconds, actual_seconds, started_at, running_since, observed_running_since, \
         paused_at, pause_until, pause_reason, move_start, move_end, ended_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, NULL, $4, NULL, $5, 'completed', 1, 5, 5, $6, NULL, NULL, \
         NULL, NULL, NULL, NULL, NULL, $7, $6, $7)",
    )
    .bind(session_id)
    .bind(scope.workspace_id)
    .bind(item_id)
    .bind(session_index)
    .bind(Uuid::new_v4())
    .bind(started_at)
    .bind(terminal_at)
    .execute(pool)
    .await
    .expect("seed pre-v20 completed session");
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

async fn wait_for_postgres_blocker(pool: &PgPool, blocker_pid: i32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity activity \
                 WHERE $1 = ANY(pg_blocking_pids(activity.pid)))",
            )
            .bind(blocker_pid)
            .fetch_one(pool)
            .await
            .expect("inspect PostgreSQL blocking graph");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("competing PostgreSQL sensitivity mutation reached the intended lock");
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
