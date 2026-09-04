use std::{
    str::FromStr,
    sync::{Arc, OnceLock},
};

use chrono::{Duration, TimeZone as _, Utc};
use dayweave_api::{
    items::{Item, ItemKind, ItemStatus, NewItem, SplitPolicy},
    persistence::{DatabaseScope, MIGRATOR, PostgresProposalApplicationRepository},
    proposals::{ProposalApplicationStatus, ProposalUndoRequest},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{
    AssertSqlSafe, ConnectOptions as _, Executor as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end upgrade keeps evidence, migration, and real undo together.
async fn pre_dependency_graph_application_keeps_exact_snapshot_and_remains_undoable() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal dependency upgrade test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let historical = seed_historical_application(&database.pool, scope, false, false).await;
    let original_snapshot: Value = sqlx::query_scalar(
        "SELECT before_snapshot FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 AND ordinal=0",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(historical.application)
    .fetch_one(&database.pool)
    .await
    .expect("pre-0025 dependency-bearing undo snapshot");

    apply_dependency_graph_migration(&database.pool)
        .await
        .expect("safe historical undo survives dependency cutover");

    let (review_ordinal, migrated_snapshot): (i16, Value) = sqlx::query_as(
        "SELECT review_ordinal,before_snapshot FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 AND ordinal=0",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(historical.application)
    .fetch_one(&database.pool)
    .await
    .expect("migrated historical proposal evidence");
    assert_eq!(
        review_ordinal, 0,
        "historical reviewed order equals its old execution order"
    );
    assert_eq!(
        migrated_snapshot, original_snapshot,
        "migration must not rewrite hash-bound undo evidence"
    );
    let reordered = sqlx::query(
        "UPDATE proposal_application_effects SET review_ordinal=1 \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 AND ordinal=0",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(historical.application)
    .execute(&database.pool)
    .await
    .expect_err("backfilled reviewed order remains immutable evidence");
    assert_eq!(
        reordered
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );

    let applications = PostgresProposalApplicationRepository::new(database.pool.clone(), scope);
    let undone = applications
        .undo(
            historical.application,
            ProposalUndoRequest {
                expected_application_revision: 1,
            },
            "pre-0025-dependency-undo-0001",
            None,
        )
        .await
        .expect("post-upgrade undo restores the historical dependency graph");
    assert_eq!(undone.application.status, ProposalApplicationStatus::Undone);
    assert_eq!(undone.application.application_revision, 2);

    let restored_edge: (String, i32, String, Option<i32>, i32) = sqlx::query_as(
        "SELECT dependency_kind,lag_seconds,dependency_strength, \
                dependency_soft_weight,projection_ordinal \
         FROM item_dependencies WHERE workspace_id=$1 \
           AND predecessor_item_id=$2 AND successor_item_id=$3",
    )
    .bind(scope.workspace_id)
    .bind(historical.predecessor)
    .bind(historical.target)
    .fetch_one(&database.pool)
    .await
    .expect("undo restored dependency into normalized authority");
    assert_eq!(
        restored_edge,
        (
            "finish_to_start".to_owned(),
            900,
            "hard".to_owned(),
            None,
            0,
        )
    );
    let restored: (String, i64, bool) = sqlx::query_as(
        "SELECT title,revision, \
                scheduling_constraints #> '{constraints,dependencies}' IS NULL \
         FROM items WHERE workspace_id=$1 AND id=$2",
    )
    .bind(scope.workspace_id)
    .bind(historical.target)
    .fetch_one(&database.pool)
    .await
    .expect("restored item row");
    assert_eq!(restored, ("Before dependency edit".to_owned(), 3, true));

    database.destroy().await;
}

#[tokio::test]
async fn dependency_cutover_rejects_an_actionable_undo_whose_old_graph_now_cycles() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!(
            "DAYWEAVE_TEST_DATABASE_URL is unset; proposal dependency preflight test skipped"
        );
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let historical = seed_historical_application(&database.pool, scope, true, false).await;

    let error = apply_dependency_graph_migration(&database.pool)
        .await
        .expect_err("cutover must not strand a currently actionable undo behind a graph cycle");
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint),
        Some("item_dependencies_acyclic")
    );
    assert!(
        error.as_database_error().is_some_and(|database| database
            .message()
            .contains(&historical.application.to_string())),
        "preflight identifies the application that needs repair: {error}"
    );

    let migration_rolled_back: (bool, bool) = sqlx::query_as(
        "SELECT \
             NOT EXISTS (SELECT 1 FROM information_schema.columns \
                         WHERE table_schema=current_schema() \
                           AND table_name='proposal_application_effects' \
                           AND column_name='review_ordinal'), \
             scheduling_constraints #> '{constraints,dependencies}' IS NOT NULL \
         FROM items WHERE workspace_id=$1 AND id=$2",
    )
    .bind(scope.workspace_id)
    .bind(historical.predecessor)
    .fetch_one(&database.pool)
    .await
    .expect("failed cutover retains the pre-0025 authority and schema");
    assert_eq!(migration_rolled_back, (true, true));

    database.destroy().await;
}

#[tokio::test]
async fn provider_managed_historical_application_is_still_dependency_preflighted() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; provider-managed upgrade test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let historical = seed_historical_application(&database.pool, scope, true, false).await;
    mark_provider_managed(&database.pool, scope, historical.target).await;

    let error = apply_dependency_graph_migration(&database.pool)
        .await
        .expect_err("mutable provider ownership must not bypass legacy undo preflight");
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint),
        Some("item_dependencies_acyclic")
    );
    assert!(
        error.as_database_error().is_some_and(|database| database
            .message()
            .contains(&historical.application.to_string())),
        "preflight identifies the provider-managed application: {error}"
    );

    database.destroy().await;
}

#[tokio::test]
async fn missing_inverse_parent_historical_application_is_still_dependency_preflighted() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; inverse parent upgrade test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let historical = seed_historical_application(&database.pool, scope, true, true).await;

    let error = apply_dependency_graph_migration(&database.pool)
        .await
        .expect_err("mutable inverse-parent validity must not bypass legacy undo preflight");
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint),
        Some("item_dependencies_acyclic")
    );
    assert!(
        error.as_database_error().is_some_and(|database| database
            .message()
            .contains(&historical.application.to_string())),
        "preflight identifies the missing-parent application: {error}"
    );

    database.destroy().await;
}

#[tokio::test]
async fn dependency_cutover_rejects_a_legacy_undo_above_atomic_payload_bound() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; undo payload upgrade test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let application_id = seed_oversized_historical_application(&database.pool, scope, false).await;

    let error = apply_dependency_graph_migration(&database.pool)
        .await
        .expect_err("an oversized historical atomic undo must stop before cutover");
    let message = error
        .as_database_error()
        .map(sqlx::error::DatabaseError::message)
        .unwrap_or_default();
    assert!(message.contains(&application_id.to_string()), "{error}");
    assert!(
        message.contains("8 MiB atomic delta payload bound"),
        "{error}"
    );
    let review_column_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema=current_schema() AND table_name='proposal_application_effects' \
           AND column_name='review_ordinal')",
    )
    .fetch_one(&database.pool)
    .await
    .expect("inspect rolled-back schema");
    assert!(!review_column_exists);

    database.destroy().await;
}

#[tokio::test]
async fn dependency_cutover_counts_parent_dependency_projections_in_payload_bound() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; parent payload upgrade test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let application_id = seed_oversized_historical_application(&database.pool, scope, true).await;

    let error = apply_dependency_graph_migration(&database.pool)
        .await
        .expect_err("large normalized parent projections must stop before cutover");
    let message = error
        .as_database_error()
        .map(sqlx::error::DatabaseError::message)
        .unwrap_or_default();
    assert!(message.contains(&application_id.to_string()), "{error}");
    assert!(
        message.contains("8 MiB atomic delta payload bound"),
        "{error}"
    );

    database.destroy().await;
}

#[tokio::test]
async fn dependency_cutover_rejects_nil_predecessor_even_when_nil_item_exists() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; nil dependency upgrade test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let created_at = Utc
        .timestamp_micros(1_788_514_400_000_000)
        .single()
        .expect("fixed microsecond timestamp");
    let nil_item = canonical_item(Uuid::nil(), "Legacy nil identity", created_at);
    insert_item_row(&database.pool, scope, &nil_item, json!({})).await;
    let successor = canonical_item(Uuid::new_v4(), "Nil dependency successor", created_at);
    insert_item_row(
        &database.pool,
        scope,
        &successor,
        dependency_metadata(Uuid::nil()),
    )
    .await;

    let error = apply_dependency_graph_migration(&database.pool)
        .await
        .expect_err("nil is never a valid dependency identity");
    assert!(
        error.as_database_error().is_some_and(|database| database
            .message()
            .contains("legacy constraints.dependencies contains an invalid edge")),
        "{error}"
    );

    database.destroy().await;
}

#[tokio::test]
async fn dependency_cutover_preserves_legacy_ungrouped_history_but_rejects_new_null_groups() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; item-change cutover test skipped");
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    apply_pre_dependency_graph_migrations(&database.pool).await;
    let scope = seed_scope(&database.pool).await;
    let created_at = Utc
        .timestamp_micros(1_788_514_400_000_000)
        .single()
        .expect("fixed microsecond timestamp");
    let item = canonical_item(Uuid::new_v4(), "Legacy delta history", created_at);
    insert_item_row(&database.pool, scope, &item, json!({})).await;
    let payload = serde_json::to_value(&item).expect("serialize legacy delta payload");
    let legacy_sequence: i64 = sqlx::query_scalar(
        "INSERT INTO item_changes (workspace_id,item_id,item_revision,change_kind,payload,changed_at) \
         VALUES ($1,$2,1,'upsert',$3,$4) RETURNING sequence",
    )
    .bind(scope.workspace_id)
    .bind(item.id)
    .bind(&payload)
    .bind(created_at)
    .fetch_one(&database.pool)
    .await
    .expect("pre-0025 ungrouped history");

    apply_dependency_graph_migration(&database.pool)
        .await
        .expect("dependency cutover with legacy ungrouped history");

    let legacy: (Value, Option<Uuid>) =
        sqlx::query_as("SELECT payload,change_group_id FROM item_changes WHERE sequence=$1")
            .bind(legacy_sequence)
            .fetch_one(&database.pool)
            .await
            .expect("legacy ungrouped history remains readable");
    assert_eq!(legacy, (payload.clone(), None));

    let rejected = sqlx::query(
        "INSERT INTO item_changes (workspace_id,item_id,item_revision,change_kind,payload,changed_at) \
         VALUES ($1,$2,2,'upsert',$3,$4)",
    )
    .bind(scope.workspace_id)
    .bind(item.id)
    .bind(&payload)
    .bind(created_at + Duration::seconds(1))
    .execute(&database.pool)
    .await
    .expect_err("post-cutover item changes must not omit their delivery group");
    assert_eq!(
        rejected
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint),
        Some("item_changes_group_required")
    );

    let group_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO item_changes (workspace_id,item_id,item_revision,change_kind,payload,changed_at,change_group_id) \
         VALUES ($1,$2,2,'upsert',$3,$4,$5)",
    )
    .bind(scope.workspace_id)
    .bind(item.id)
    .bind(payload)
    .bind(created_at + Duration::seconds(1))
    .bind(group_id)
    .execute(&database.pool)
    .await
    .expect("post-cutover grouped item change");

    database.destroy().await;
}

struct HistoricalApplication {
    application: Uuid,
    predecessor: Uuid,
    target: Uuid,
}

async fn apply_pre_dependency_graph_migrations(pool: &PgPool) {
    for migration in MIGRATOR.iter().filter(|migration| migration.version < 25) {
        pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
            .await
            .expect("pre-0025 migration applies");
    }
}

async fn apply_dependency_graph_migration(pool: &PgPool) -> Result<(), sqlx::Error> {
    let migration = MIGRATOR
        .iter()
        .find(|migration| migration.version == 25)
        .expect("dependency graph migration is embedded");
    pool.execute(AssertSqlSafe(migration.sql.as_str().to_owned()))
        .await
        .map(|_| ())
}

#[allow(clippy::too_many_lines)]
async fn seed_historical_application(
    pool: &PgPool,
    scope: DatabaseScope,
    add_current_reverse_edge: bool,
    missing_inverse_parent: bool,
) -> HistoricalApplication {
    let created_at = Utc
        .timestamp_micros(1_788_514_400_000_000)
        .single()
        .expect("fixed microsecond timestamp");
    let applied_at = created_at + Duration::seconds(2);
    let predecessor_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let predecessor = canonical_item(predecessor_id, "Dependency predecessor", created_at);
    let mut before = canonical_item(target_id, "Before dependency edit", created_at);
    before.flexible_constraints = dependency_metadata(predecessor_id);
    if missing_inverse_parent {
        before.parent_id = Some(Uuid::new_v4());
    }
    let mut after = before.clone();
    after.title = String::from("After dependency edit");
    after.flexible_constraints = json!({});
    after.revision = 2;
    after.updated_at = applied_at;

    insert_item_row(
        pool,
        scope,
        &predecessor,
        if add_current_reverse_edge {
            dependency_metadata(target_id)
        } else {
            json!({})
        },
    )
    .await;
    insert_item_row(pool, scope, &after, json!({})).await;

    let proposal_id = Uuid::new_v4();
    let preview_id = Uuid::new_v4();
    let application_id = Uuid::new_v4();
    let action_id = Uuid::new_v4();
    let audit_id = Uuid::new_v4();
    let preview_hash = vec![0x31_u8; 32];
    let proposal_payload_hash = vec![0x32_u8; 32];
    let before_snapshot = serde_json::to_value(&before).expect("serialize before snapshot");
    let after_snapshot = serde_json::to_value(&after).expect("serialize after snapshot");

    sqlx::query(
        "INSERT INTO proposals (id,workspace_id,revision,submitted_by_user_id, \
             submitted_by_subject,source,kind,status,title,payload,created_at,updated_at,expires_at) \
         VALUES ($1,$2,1,$3,'device:pre-0025','codex','update_item','pending', \
                 'Historical dependency edit','{}'::jsonb,$4,$4,$5)",
    )
    .bind(proposal_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(created_at - Duration::minutes(1))
    .bind(created_at + Duration::days(8))
    .execute(pool)
    .await
    .expect("historical proposal");

    let mut preview = pool.begin().await.expect("preview transaction");
    sqlx::query(
        "INSERT INTO proposal_apply_previews (id,workspace_id,user_id,proposal_count, \
             command_count,commands_hash,canonical_hash,review_content_hash,preview_hash, \
             can_apply,created_at,expires_at) \
         VALUES ($1,$2,$3,1,1,$4,$5,$6,$7,true,$8,$9)",
    )
    .bind(preview_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![0x33_u8; 32])
    .bind(vec![0x34_u8; 32])
    .bind(vec![0x35_u8; 32])
    .bind(&preview_hash)
    .bind(created_at)
    .bind(created_at + Duration::minutes(10))
    .execute(&mut *preview)
    .await
    .expect("historical preview header");
    sqlx::query(
        "INSERT INTO proposal_apply_preview_members (workspace_id,user_id,preview_id, \
             ordinal,proposal_id,proposal_revision,proposal_payload_hash) \
         VALUES ($1,$2,$3,0,$4,1,$5)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .bind(proposal_id)
    .bind(&proposal_payload_hash)
    .execute(&mut *preview)
    .await
    .expect("historical preview member");
    preview.commit().await.expect("complete historical preview");

    let mut application = pool.begin().await.expect("application transaction");
    sqlx::query(
        "INSERT INTO audit_operations (id,workspace_id,actor_user_id,operation_type, \
             entity_type,entity_id,result_revision,outcome) \
         VALUES ($1,$2,$3,'proposal.application.applied','proposal_application',$4,1,'succeeded')",
    )
    .bind(audit_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .execute(&mut *application)
    .await
    .expect("historical application audit");
    sqlx::query(
        "INSERT INTO proposal_applications (id,workspace_id,user_id,preview_id,preview_hash, \
             status,revision,effect_count,fence_count,apply_audit_id,applied_at,undo_expires_at) \
         VALUES ($1,$2,$3,$4,$5,'applied',1,1,1,$6,$7,$8)",
    )
    .bind(application_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .bind(&preview_hash)
    .bind(audit_id)
    .bind(applied_at)
    .bind(applied_at + Duration::days(7))
    .execute(&mut *application)
    .await
    .expect("historical application header");
    sqlx::query(
        "INSERT INTO proposal_application_members (workspace_id,user_id,application_id, \
             ordinal,proposal_id) VALUES ($1,$2,$3,0,$4)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .bind(proposal_id)
    .execute(&mut *application)
    .await
    .expect("historical application member");
    sqlx::query(
        "INSERT INTO proposal_application_effects (workspace_id,user_id,application_id,ordinal, \
             action_id,operation,command_hash,item_id,expected_revision,before_revision, \
             after_revision,before_deleted,after_deleted,before_snapshot_hash, \
             after_snapshot_hash,before_snapshot,after_snapshot,created_at) \
         VALUES ($1,$2,$3,0,$4,'replace_item',$5,$6,1,1,2,false,false,$7,$8,$9,$10,$11)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .bind(action_id)
    .bind(vec![0x36_u8; 32])
    .bind(target_id)
    .bind(item_snapshot_hash(&before))
    .bind(item_snapshot_hash(&after))
    .bind(&before_snapshot)
    .bind(&after_snapshot)
    .bind(applied_at)
    .execute(&mut *application)
    .await
    .expect("historical application effect");
    sqlx::query(
        "INSERT INTO proposal_application_fences (workspace_id,user_id,application_id,item_id, \
             applied_revision,applied_deleted) VALUES ($1,$2,$3,$4,2,false)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .bind(target_id)
    .execute(&mut *application)
    .await
    .expect("historical application fence");
    sqlx::query(
        "INSERT INTO proposal_application_requests (workspace_id,user_id,operation,key_hash, \
             request_hash,application_id,completed_at) \
         VALUES ($1,$2,'apply',$3,$4,$5,$6)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![0x37_u8; 32])
    .bind(vec![0x38_u8; 32])
    .bind(application_id)
    .bind(applied_at)
    .execute(&mut *application)
    .await
    .expect("historical apply receipt");
    sqlx::query(
        "UPDATE proposals SET revision=2,status='accepted',updated_at=$3,decided_at=$3 \
         WHERE workspace_id=$1 AND id=$2",
    )
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .bind(applied_at)
    .execute(&mut *application)
    .await
    .expect("historical accepted proposal");
    application
        .commit()
        .await
        .expect("complete historical application evidence");

    HistoricalApplication {
        application: application_id,
        predecessor: predecessor_id,
        target: target_id,
    }
}

#[allow(clippy::too_many_lines)]
async fn seed_oversized_historical_application(
    pool: &PgPool,
    scope: DatabaseScope,
    with_parent_dependency_refreshes: bool,
) -> Uuid {
    let effect_count = if with_parent_dependency_refreshes {
        72_i16
    } else {
        84_i16
    };
    let created_at = Utc
        .timestamp_micros(1_788_514_400_000_000)
        .single()
        .expect("fixed microsecond timestamp");
    let applied_at = created_at + Duration::seconds(2);
    let proposal_id = Uuid::new_v4();
    let preview_id = Uuid::new_v4();
    let application_id = Uuid::new_v4();
    let audit_id = Uuid::new_v4();
    let preview_hash = vec![0x61_u8; 32];
    let parent_ids = if with_parent_dependency_refreshes {
        let mut predecessor_ids = Vec::with_capacity(258);
        for index in 0..258 {
            let predecessor_id = Uuid::new_v4();
            let predecessor = canonical_item(
                predecessor_id,
                &format!("Large parent predecessor {index}"),
                created_at,
            );
            insert_item_row(pool, scope, &predecessor, json!({})).await;
            predecessor_ids.push(predecessor_id);
        }
        let parent_dependencies = dependency_metadata_many(&predecessor_ids);
        let dependency_bytes = serde_json::to_vec(&parent_dependencies)
            .expect("serialize large parent dependency projection")
            .len();
        assert!(
            (30_000..=32 * 1_024).contains(&dependency_bytes),
            "fixture must approach the canonical scheduling metadata bound: {dependency_bytes}"
        );
        let before_parent_id = Uuid::new_v4();
        let current_parent_id = Uuid::new_v4();
        let parent_revision = u64::try_from(effect_count).expect("positive effect count") + 1;
        for (parent_id, title) in [
            (before_parent_id, "Large old parent"),
            (current_parent_id, "Large current parent"),
        ] {
            let mut parent = canonical_item(parent_id, title, created_at);
            parent.flexible_constraints = parent_dependencies.clone();
            parent.revision = parent_revision;
            parent.updated_at = applied_at;
            insert_item_row(pool, scope, &parent, parent.flexible_constraints.clone()).await;
        }
        Some((before_parent_id, current_parent_id))
    } else {
        None
    };
    let fence_count = i32::from(effect_count) + if parent_ids.is_some() { 2 } else { 0 };

    sqlx::query(
        "INSERT INTO proposals (id,workspace_id,revision,submitted_by_user_id, \
             submitted_by_subject,source,kind,status,title,payload,created_at,updated_at,expires_at) \
         VALUES ($1,$2,1,$3,'device:pre-0025-large','codex','update_item','pending', \
                 'Historical large batch','{}'::jsonb,$4,$4,$5)",
    )
    .bind(proposal_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(created_at - Duration::minutes(1))
    .bind(created_at + Duration::days(8))
    .execute(pool)
    .await
    .expect("large historical proposal");

    let mut preview = pool.begin().await.expect("large preview transaction");
    sqlx::query(
        "INSERT INTO proposal_apply_previews (id,workspace_id,user_id,proposal_count, \
             command_count,commands_hash,canonical_hash,review_content_hash,preview_hash, \
             can_apply,created_at,expires_at) \
         VALUES ($1,$2,$3,1,$4,$5,$6,$7,$8,true,$9,$10)",
    )
    .bind(preview_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(effect_count)
    .bind(vec![0x62_u8; 32])
    .bind(vec![0x63_u8; 32])
    .bind(vec![0x64_u8; 32])
    .bind(&preview_hash)
    .bind(created_at)
    .bind(created_at + Duration::minutes(10))
    .execute(&mut *preview)
    .await
    .expect("large historical preview header");
    sqlx::query(
        "INSERT INTO proposal_apply_preview_members (workspace_id,user_id,preview_id, \
             ordinal,proposal_id,proposal_revision,proposal_payload_hash) \
         VALUES ($1,$2,$3,0,$4,1,$5)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .bind(proposal_id)
    .bind(vec![0x65_u8; 32])
    .execute(&mut *preview)
    .await
    .expect("large historical preview member");
    preview
        .commit()
        .await
        .expect("complete large historical preview");

    let mut application = pool.begin().await.expect("large application transaction");
    sqlx::query(
        "INSERT INTO audit_operations (id,workspace_id,actor_user_id,operation_type, \
             entity_type,entity_id,result_revision,outcome) \
         VALUES ($1,$2,$3,'proposal.application.applied','proposal_application',$4,1,'succeeded')",
    )
    .bind(audit_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .execute(&mut *application)
    .await
    .expect("large historical application audit");
    sqlx::query(
        "INSERT INTO proposal_applications (id,workspace_id,user_id,preview_id,preview_hash, \
             status,revision,effect_count,fence_count,apply_audit_id,applied_at,undo_expires_at) \
         VALUES ($1,$2,$3,$4,$5,'applied',1,$6,$7,$8,$9,$10)",
    )
    .bind(application_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .bind(&preview_hash)
    .bind(effect_count)
    .bind(fence_count)
    .bind(audit_id)
    .bind(applied_at)
    .bind(applied_at + Duration::days(7))
    .execute(&mut *application)
    .await
    .expect("large historical application header");
    sqlx::query(
        "INSERT INTO proposal_application_members (workspace_id,user_id,application_id, \
             ordinal,proposal_id) VALUES ($1,$2,$3,0,$4)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .bind(proposal_id)
    .execute(&mut *application)
    .await
    .expect("large historical application member");

    let notes = "x".repeat(if parent_ids.is_some() {
        60_000
    } else {
        100_000
    });
    for ordinal in 0..effect_count {
        let item_id = Uuid::new_v4();
        let mut before = canonical_item(
            item_id,
            &format!("Large historical item {ordinal}"),
            created_at,
        );
        before.notes = Some(notes.clone());
        if let Some((before_parent_id, _)) = parent_ids {
            before.parent_id = Some(before_parent_id);
            before.sibling_order = u32::try_from(ordinal).expect("non-negative ordinal");
        }
        let mut after = before.clone();
        after.title = format!("Changed large historical item {ordinal}");
        if let Some((_, current_parent_id)) = parent_ids {
            after.parent_id = Some(current_parent_id);
        }
        after.revision = 2;
        after.updated_at = applied_at;
        insert_item_row(pool, scope, &after, json!({})).await;
        if let Some((_, current_parent_id)) = parent_ids {
            sqlx::query(
                "INSERT INTO item_hierarchy (workspace_id,parent_item_id,child_item_id,position) \
                 VALUES ($1,$2,$3,$4)",
            )
            .bind(scope.workspace_id)
            .bind(current_parent_id)
            .bind(item_id)
            .bind(i32::from(ordinal))
            .execute(pool)
            .await
            .expect("current large historical hierarchy");
        }
        let before_snapshot =
            serde_json::to_value(&before).expect("serialize large before snapshot");
        let after_snapshot = serde_json::to_value(&after).expect("serialize large after snapshot");
        sqlx::query(
            "INSERT INTO proposal_application_effects (workspace_id,user_id,application_id, \
                 ordinal,action_id,operation,command_hash,item_id,expected_revision,before_revision, \
                 after_revision,before_deleted,after_deleted,before_snapshot_hash, \
                 after_snapshot_hash,before_snapshot,after_snapshot,created_at) \
             VALUES ($1,$2,$3,$4,$5,'replace_item',$6,$7,1,1,2,false,false,$8,$9,$10,$11,$12)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(application_id)
        .bind(ordinal)
        .bind(Uuid::new_v4())
        .bind(vec![0x66_u8; 32])
        .bind(item_id)
        .bind(item_snapshot_hash(&before))
        .bind(item_snapshot_hash(&after))
        .bind(before_snapshot)
        .bind(after_snapshot)
        .bind(applied_at)
        .execute(&mut *application)
        .await
        .expect("large historical effect");
        sqlx::query(
            "INSERT INTO proposal_application_fences (workspace_id,user_id,application_id, \
                 item_id,applied_revision,applied_deleted) VALUES ($1,$2,$3,$4,2,false)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(application_id)
        .bind(item_id)
        .execute(&mut *application)
        .await
        .expect("large historical fence");
    }

    if let Some((before_parent_id, current_parent_id)) = parent_ids {
        let parent_revision = i64::from(effect_count) + 1;
        for parent_id in [before_parent_id, current_parent_id] {
            sqlx::query(
                "INSERT INTO proposal_application_fences (workspace_id,user_id,application_id, \
                     item_id,applied_revision,applied_deleted) VALUES ($1,$2,$3,$4,$5,false)",
            )
            .bind(scope.workspace_id)
            .bind(scope.user_id)
            .bind(application_id)
            .bind(parent_id)
            .bind(parent_revision)
            .execute(&mut *application)
            .await
            .expect("large historical parent fence");
        }
    }

    sqlx::query(
        "INSERT INTO proposal_application_requests (workspace_id,user_id,operation,key_hash, \
             request_hash,application_id,completed_at) \
         VALUES ($1,$2,'apply',$3,$4,$5,$6)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(vec![0x67_u8; 32])
    .bind(vec![0x68_u8; 32])
    .bind(application_id)
    .bind(applied_at)
    .execute(&mut *application)
    .await
    .expect("large historical apply receipt");
    sqlx::query(
        "UPDATE proposals SET revision=2,status='accepted',updated_at=$3,decided_at=$3 \
         WHERE workspace_id=$1 AND id=$2",
    )
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .bind(applied_at)
    .execute(&mut *application)
    .await
    .expect("large historical accepted proposal");
    application
        .commit()
        .await
        .expect("complete large historical application evidence");
    application_id
}

fn canonical_item(id: Uuid, title: &str, now: chrono::DateTime<Utc>) -> Item {
    Item::new(
        NewItem {
            id,
            is_sensitive: false,
            kind: ItemKind::Task,
            status: ItemStatus::Inbox,
            title: title.to_owned(),
            notes: None,
            timezone_name: "Europe/Paris".to_owned(),
            duration_kind: None,
            duration_seconds: Some(1_800),
            duration_min_seconds: None,
            duration_max_seconds: None,
            duration_source: None,
            deadline_kind: None,
            deadline_date: None,
            deadline_at: None,
            deadline_strength: None,
            deadline_soft_weight: None,
            earliest_start_at: None,
            recurrence: None,
            flexible_constraints: json!({}),
            has_own_effort: None,
            split_policy: SplitPolicy::Indivisible,
            importance: 70,
            urgency: 40,
            parent_id: None,
            sibling_order: 0,
            blocked_reason_kind: None,
            blocked_by_item_id: None,
            blocked_reason: None,
        },
        now,
    )
    .expect("canonical historical item")
}

async fn insert_item_row(
    pool: &PgPool,
    scope: DatabaseScope,
    item: &Item,
    scheduling_constraints: Value,
) {
    sqlx::query(
        "INSERT INTO items (id,workspace_id,created_by_user_id,is_sensitive,kind,status,title, \
             notes,timezone_name,duration_seconds,scheduling_constraints,split_allowed,importance, \
             urgency,revision,created_at,updated_at,sibling_order) \
         VALUES ($1,$2,$3,false,'task','inbox',$4,$5,'Europe/Paris',1800,$6,false,70,40,$7,$8,$9,0)",
    )
    .bind(item.id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(&item.title)
    .bind(&item.notes)
    .bind(scheduling_constraints)
    .bind(i64::try_from(item.revision).expect("item revision fits PostgreSQL"))
    .bind(item.created_at)
    .bind(item.updated_at)
    .execute(pool)
    .await
    .expect("historical item row");
}

fn dependency_metadata(predecessor_id: Uuid) -> Value {
    json!({
        "constraints": {
            "dependencies": [{
                "item_id": predecessor_id,
                "relation": "finish_to_start",
                "minimum_lag": 15,
                "strength": {"level": "hard"}
            }]
        }
    })
}

fn dependency_metadata_many(predecessor_ids: &[Uuid]) -> Value {
    json!({
        "constraints": {
            "dependencies": predecessor_ids.iter().map(|predecessor_id| json!({
                "item_id": predecessor_id,
                "relation": "finish_to_start",
                "minimum_lag": 15,
                "strength": {"level": "hard"}
            })).collect::<Vec<_>>()
        }
    })
}

fn item_snapshot_hash(item: &Item) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(b"dayweave.proposal.item-snapshot.v1\0");
    digest.update(serde_json::to_vec(item).expect("serialize hash-bound Item"));
    digest.finalize().to_vec()
}

async fn mark_provider_managed(pool: &PgPool, scope: DatabaseScope, item_id: Uuid) {
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id,workspace_id,user_id,provider,external_account_id, \
             display_label,encrypted_credentials,credential_key_version,status,sync_enabled,is_default) \
         VALUES ($1,$2,$3,'google',$4,'Historical provider account',$5,1,'active',true,false)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("historical-provider-{account_id}"))
    .bind(vec![0x51_u8; 64])
    .execute(pool)
    .await
    .expect("historical provider account");
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id,workspace_id,provider_account_id,entity_kind, \
             local_entity_id,remote_resource_id,local_revision,sync_state,ownership) \
         VALUES ($1,$2,$3,'item',$4,$5,2,'synced','external')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(account_id)
    .bind(item_id)
    .bind(format!("historical-provider-item-{item_id}"))
    .execute(pool)
    .await
    .expect("provider-managed historical target");
}

async fn seed_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id,auth_subject,display_name,timezone_name) \
         VALUES ($1,$2,'Proposal migration owner','Europe/Paris')",
    )
    .bind(scope.user_id)
    .bind(format!(
        "proposal-migration-owner-{}",
        scope.user_id.simple()
    ))
    .execute(pool)
    .await
    .expect("migration owner");
    sqlx::query(
        "INSERT INTO workspaces (id,owner_user_id,slug,name,timezone_name) \
         VALUES ($1,$2,$3,'Personal','Europe/Paris')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!(
        "proposal-migration-{}",
        scope.workspace_id.simple()
    ))
    .execute(pool)
    .await
    .expect("migration workspace");
    sqlx::query("INSERT INTO workspace_members (workspace_id,user_id,role) VALUES ($1,$2,'owner')")
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .execute(pool)
        .await
        .expect("migration owner membership");
    scope
}

struct TestDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    _migration_permit: OwnedSemaphorePermit,
}

impl TestDatabase {
    async fn create(database_url: &str) -> Self {
        // Each test applies the complete historical migration chain in its own
        // schema. Running many chains concurrently can exhaust PostgreSQL's
        // finite relation-lock table even though their data is isolated, so
        // keep this schema-heavy target deterministic on production-shaped
        // instances with the default `max_locks_per_transaction` setting.
        let migration_permit = migration_test_semaphore()
            .clone()
            .acquire_owned()
            .await
            .expect("migration test semaphore remains open");
        let options = PgConnectOptions::from_str(database_url)
            .expect("valid DAYWEAVE_TEST_DATABASE_URL")
            .disable_statement_logging();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect test PostgreSQL");
        let schema = format!(
            "dayweave_proposal_dependency_migration_{}",
            Uuid::new_v4().simple()
        );
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
            _migration_permit: migration_permit,
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

fn migration_test_semaphore() -> &'static Arc<Semaphore> {
    static SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEMAPHORE.get_or_init(|| Arc::new(Semaphore::new(1)))
}
