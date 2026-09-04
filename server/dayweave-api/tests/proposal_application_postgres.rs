use std::{
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    execution::{
        ExecutionCommand, ExecutionIdempotencyKey, ExecutionService, FinishExecution,
        StartExecution,
    },
    items::{
        DeltaChange, IdempotencyKey, Item, ItemKind, ItemRepository, ItemService, ItemStatus,
        NewItem, ReplaceItem, SplitPolicy,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresExecutionRepository, PostgresItemRepository,
        PostgresProposalApplicationRepository, PostgresProposalRepository,
        ProposalApplicationError,
    },
    proposals::{
        Clock, NewProposal, Proposal, ProposalApplicationStatus, ProposalApplyRequest,
        ProposalChangeSet, ProposalCommand, ProposalConflictCode, ProposalImplicitChangeReason,
        ProposalItemField, ProposalKind, ProposalPreviewMember, ProposalPreviewRequest,
        ProposalRepository, ProposalRiskCode, ProposalSource, ProposalStatus, ProposalUndoRequest,
        SystemClock,
    },
};
use serde_json::{Value, json};
use sqlx::{
    AssertSqlSafe, ConnectOptions, Executor, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn grouped_application_is_atomic_idempotent_and_revision_fenced_for_undo() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal application test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let proposals = PostgresProposalRepository::new(test_database.pool.clone(), scope);
    let applications =
        PostgresProposalApplicationRepository::new(test_database.pool.clone(), scope);

    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let dependent_child_id = Uuid::new_v4();
    let mut dependent_child = item(
        dependent_child_id,
        ItemKind::Task,
        "Review launch notes",
        true,
        Some(parent_id),
    );
    dependent_child.flexible_constraints = dependency_metadata(child_id);
    let commands = vec![
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: dependent_child,
        },
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                child_id,
                ItemKind::Task,
                "Prepare launch notes",
                true,
                Some(parent_id),
            ),
        },
        // The parent intentionally appears last. Batch execution must stage all
        // identities, finalize parents before children, and retain submitted
        // order in review while persisting actual execution order for undo.
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(parent_id, ItemKind::Goal, "Private launch", true, None),
        },
    ];
    let payload = serde_json::to_value(ProposalChangeSet::new(commands.clone()).unwrap()).unwrap();
    let now = Utc::now();
    let proposal = Proposal::new(
        NewProposal {
            submitted_by: "device:test-owner".to_owned(),
            source: ProposalSource::Codex,
            source_reference: Some("synthetic-thread".to_owned()),
            kind: ProposalKind::GoalBreakdown,
            title: "Break down launch".to_owned(),
            explanation: Some("Create a private goal and its first task.".to_owned()),
            payload,
            expires_at: now + Duration::days(7),
        },
        now,
    )
    .unwrap();
    let proposal = proposals.insert(proposal).await.unwrap();

    let preview = applications
        .preview(ProposalPreviewRequest {
            proposals: vec![ProposalPreviewMember {
                proposal_id: proposal.id,
                expected_revision: proposal.revision,
            }],
        })
        .await
        .expect("typed proposal previews");
    assert!(preview.can_apply);
    assert_eq!(preview.command_ids.len(), 3);
    assert_eq!(preview.diffs.len(), 3);
    assert!(preview.requires_explicit_approval);
    let parent_preview = preview
        .diffs
        .iter()
        .find(|diff| diff.item_id == parent_id)
        .and_then(|diff| diff.after.as_ref())
        .expect("parent has a final preview snapshot");
    assert_eq!(parent_preview.revision, 3);
    assert!(!parent_preview.is_executable);

    let apply_request = ProposalApplyRequest {
        expected_review_hash: preview.review_hash.clone(),
    };
    let first = applications
        .apply(
            preview.preview_id,
            apply_request.clone(),
            "proposal-apply-key-0001",
            None,
        )
        .await
        .expect("application commits");
    assert!(!first.replayed);
    assert_eq!(first.application.status, ProposalApplicationStatus::Applied);
    assert_eq!(first.application.proposals[0].applied_revision, 2);
    assert_eq!(first.application.affected_item_ids.len(), 3);
    assert_eq!(first.application.command_ids.len(), 3);
    assert_eq!(
        first.application.command_ids,
        commands
            .iter()
            .map(ProposalCommand::command_id)
            .collect::<Vec<_>>(),
        "receipts preserve the reviewed order even when execution is reordered"
    );
    assert_eq!(
        applications
            .get_for_proposal(proposal.id)
            .await
            .expect("application is discoverable after a lost response"),
        first.application
    );
    let item_repository = PostgresItemRepository::new(test_database.pool.clone(), scope);
    let apply_delta = item_repository
        .delta(0, 1)
        .await
        .expect("application delta is readable atomically");
    assert_eq!(
        apply_delta.changes.len(),
        5,
        "three direct creates and two parent refreshes stay in one expanded page"
    );
    assert!(!apply_delta.has_more);
    let apply_group_ids: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT change_group_id FROM item_changes \
         WHERE workspace_id=$1 AND sequence <= $2 ORDER BY sequence",
    )
    .bind(scope.workspace_id)
    .bind(i64::try_from(apply_delta.watermark).unwrap())
    .fetch_all(&test_database.pool)
    .await
    .unwrap();
    let apply_group_id = apply_group_ids[0].expect("application changes are grouped");
    assert_eq!(apply_group_ids, vec![Some(apply_group_id); 5]);

    let replay = applications
        .apply(
            preview.preview_id,
            apply_request.clone(),
            "proposal-apply-key-0001",
            None,
        )
        .await
        .expect("exact apply replays");
    assert!(replay.replayed);
    assert_eq!(replay.application, first.application);

    let uppercase_review_hash = format!(
        "sha256:{}",
        apply_request
            .expected_review_hash
            .strip_prefix("sha256:")
            .expect("preview hashes use the sha256 prefix")
            .to_ascii_uppercase()
    );
    let mixed_case_replay = applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: uppercase_review_hash,
            },
            "proposal-apply-key-0001",
            None,
        )
        .await
        .expect("equivalent mixed-case review hash replays");
    assert!(mixed_case_replay.replayed);
    assert_eq!(mixed_case_replay.application, first.application);

    let wrong_hash = ProposalApplyRequest {
        expected_review_hash: format!("sha256:{}", "0".repeat(64)),
    };
    assert!(matches!(
        applications
            .apply(
                preview.preview_id,
                wrong_hash,
                "proposal-apply-key-0001",
                None,
            )
            .await,
        Err(ProposalApplicationError::IdempotencyConflict)
    ));

    let stored_proposal = proposals.get(proposal.id).await.unwrap();
    assert_eq!(stored_proposal.status, ProposalStatus::Accepted);
    assert_eq!(stored_proposal.revision, 2);
    let parent_revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM items WHERE workspace_id=$1 AND id=$2 AND trashed_at IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(parent_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(
        parent_revision, 3,
        "both child creations refresh their parent fence"
    );

    let undone = applications
        .undo(
            first.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: 1,
            },
            "proposal-undo-key-0001",
            None,
        )
        .await
        .expect("fenced undo commits");
    assert!(!undone.replayed);
    assert_eq!(undone.application.status, ProposalApplicationStatus::Undone);
    assert_eq!(undone.application.application_revision, 2);
    let undo_delta = item_repository
        .delta(apply_delta.watermark, 1)
        .await
        .expect("undo delta is readable atomically");
    assert_eq!(
        undo_delta.changes.len(),
        5,
        "inverse commands and their parent refreshes stay in one expanded page"
    );
    assert!(!undo_delta.has_more);
    let undo_group_ids: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT change_group_id FROM item_changes \
         WHERE workspace_id=$1 AND sequence > $2 ORDER BY sequence",
    )
    .bind(scope.workspace_id)
    .bind(i64::try_from(apply_delta.watermark).unwrap())
    .fetch_all(&test_database.pool)
    .await
    .unwrap();
    let undo_group_id = undo_group_ids[0].expect("undo changes are grouped");
    assert_ne!(undo_group_id, apply_group_id);
    assert_eq!(undo_group_ids, vec![Some(undo_group_id); 5]);
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM items WHERE workspace_id=$1 AND id = ANY($2) AND trashed_at IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(vec![parent_id, child_id, dependent_child_id])
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert_eq!(active_count, 0);

    let undo_replay = applications
        .undo(
            first.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: 1,
            },
            "proposal-undo-key-0001",
            None,
        )
        .await
        .expect("exact undo replays");
    assert!(undo_replay.replayed);
    assert_eq!(undo_replay.application, undone.application);

    let audit_canary: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM audit_operations WHERE workspace_id=$1 \
         AND metadata::text LIKE '%Private launch%')",
    )
    .bind(scope.workspace_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    let outbox_canary: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM outbox_messages WHERE workspace_id=$1 \
         AND payload::text LIKE '%Private launch%')",
    )
    .bind(scope.workspace_id)
    .fetch_one(&test_database.pool)
    .await
    .unwrap();
    assert!(!audit_canary && !outbox_canary);

    test_database.destroy().await;
}

#[tokio::test]
async fn preview_rejects_an_atomic_delta_group_too_large_for_native_delivery() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal payload bound test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let large_notes = "x".repeat(100_000);
    let commands = (0..90)
        .map(|index| {
            let mut proposed = item(
                Uuid::new_v4(),
                ItemKind::Task,
                &format!("Large payload item {index}"),
                false,
                None,
            );
            proposed.notes = Some(large_notes.clone());
            ProposalCommand::CreateItem {
                command_id: Uuid::new_v4(),
                item: proposed,
            }
        })
        .collect();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::GoalBreakdown,
        "Reject an undeliverable atomic payload",
        commands,
    )
    .await;

    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("oversized delivery remains a reviewable conflict");
    assert!(!preview.can_apply);
    assert!(preview.conflicts.iter().any(|conflict| {
        conflict.code == ProposalConflictCode::InvalidItem
            && conflict.summary.contains("Split this proposal")
            && conflict.summary.contains("safe device-delivery limit")
    }));
    let leaked_changes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM item_changes WHERE workspace_id=$1")
            .bind(fixture.scope.workspace_id)
            .fetch_one(&fixture.database.pool)
            .await
            .unwrap();
    assert_eq!(
        leaked_changes, 0,
        "preview payload measurement is rolled back with the simulation"
    );

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Tunes a real PostgreSQL JSONB group exactly across the delivery boundary.
async fn preview_reserves_space_for_later_timestamp_serialization_growth() {
    const COMMAND_COUNT: usize = 84;
    const MAX_GROUP_PAYLOAD_BYTES: i64 = 8 * 1024 * 1024;
    const MAX_NOTES_CHARS: usize = 100_000;
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal payload reserve test skipped");
        return;
    };
    let preview_now = "2026-09-04T12:00:00Z"
        .parse::<DateTime<Utc>>()
        .expect("fixed whole-second preview time");
    let later_now = "2026-09-04T12:00:00.123456Z"
        .parse::<DateTime<Utc>>()
        .expect("fixed microsecond apply time");
    let clock = Arc::new(MutableClock::new(preview_now));
    let fixture = ApplicationFixture::create_with_clock(&database_url, clock.clone()).await;
    let mut proposed_items = (0..COMMAND_COUNT)
        .map(|index| {
            let mut proposed = item(
                Uuid::new_v4(),
                ItemKind::Task,
                &format!("Near-limit payload item {index}"),
                false,
                None,
            );
            proposed.notes = Some("x".repeat(96_000));
            proposed
        })
        .collect::<Vec<_>>();

    let base_payload_bytes =
        serialized_create_delta_payload_bytes(&fixture.database.pool, &proposed_items, preview_now)
            .await;
    let target_payload_bytes = MAX_GROUP_PAYLOAD_BYTES - 1;
    assert!(
        base_payload_bytes <= target_payload_bytes,
        "base fixture must start below the exact group limit: {base_payload_bytes}"
    );
    let mut remaining = usize::try_from(target_payload_bytes - base_payload_bytes)
        .expect("positive payload adjustment fits usize");
    for proposed in &mut proposed_items {
        let notes = proposed.notes.as_mut().expect("near-limit notes");
        let addition = remaining.min(MAX_NOTES_CHARS - notes.len());
        notes.push_str(&"x".repeat(addition));
        remaining -= addition;
    }
    assert_eq!(remaining, 0, "fixture has enough bounded note capacity");

    let preview_payload_bytes =
        serialized_create_delta_payload_bytes(&fixture.database.pool, &proposed_items, preview_now)
            .await;
    assert_eq!(preview_payload_bytes, target_payload_bytes);
    let later_payload_bytes =
        serialized_create_delta_payload_bytes(&fixture.database.pool, &proposed_items, later_now)
            .await;
    assert!(
        later_payload_bytes > MAX_GROUP_PAYLOAD_BYTES,
        "a wider microsecond timestamp would exceed the exact later bound: {later_payload_bytes}"
    );

    let commands = proposed_items
        .into_iter()
        .map(|item| ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item,
        })
        .collect();
    let proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::GoalBreakdown,
        "Reserve later timestamp growth",
        commands,
        preview_now,
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("near-limit delivery remains a reviewable conflict");
    assert!(!preview.can_apply);
    assert!(preview.conflicts.iter().any(|conflict| {
        conflict.code == ProposalConflictCode::InvalidItem
            && conflict.summary.contains("safe device-delivery limit")
    }));

    clock.set(later_now);
    assert!(matches!(
        fixture
            .applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "proposal-payload-reserve-apply",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewNotApplicable
        ))
    ));
    let leaked_changes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM item_changes WHERE workspace_id=$1")
            .bind(fixture.scope.workspace_id)
            .fetch_one(&fixture.database.pool)
            .await
            .expect("inspect near-limit preview rollback");
    assert_eq!(leaked_changes, 0);

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn dependency_change_is_reviewed_applied_and_restored_by_undo() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; dependency proposal test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let predecessor = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Finish the prerequisite",
                false,
                None,
            ),
            item_key("dependency-proposal-predecessor", 91),
        )
        .await
        .expect("predecessor created")
        .item;
    let successor = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Start after the prerequisite",
                false,
                None,
            ),
            item_key("dependency-proposal-successor", 92),
        )
        .await
        .expect("successor created")
        .item;
    let mut proposed = replacement(&successor, successor.status);
    proposed.flexible_constraints = dependency_metadata(predecessor.id);
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::ConstraintChange,
        "Add a scheduling dependency",
        vec![ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: successor.id,
            expected_revision: successor.revision,
            item: proposed,
        }],
    )
    .await;

    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("dependency change previews");
    assert!(preview.can_apply);
    let diff = preview.diffs.first().expect("one dependency diff");
    assert!(
        diff.changed_fields
            .contains(&ProposalItemField::Dependencies)
    );
    assert!(diff.changed_fields.contains(&ProposalItemField::Revision));
    assert!(
        !diff
            .changed_fields
            .contains(&ProposalItemField::FlexibleConstraints),
        "dependency edges must not be hidden inside the generic metadata diff"
    );
    let risk = preview
        .risks
        .iter()
        .find(|risk| risk.code == ProposalRiskCode::ChangesDependencies)
        .expect("dependency change has a dedicated risk");
    assert!(risk.requires_explicit_approval);
    assert_eq!(risk.item_id, Some(successor.id));

    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "dependency-proposal-apply",
            None,
        )
        .await
        .expect("dependency change applies");
    let current = fixture
        .items
        .get(successor.id)
        .await
        .expect("successor reads");
    assert_eq!(
        current.flexible_constraints,
        dependency_metadata(predecessor.id)
    );
    let stored_edge: (String, i32, String, Option<i32>) = sqlx::query_as(
        "SELECT dependency_kind, lag_seconds, dependency_strength, dependency_soft_weight \
         FROM item_dependencies WHERE workspace_id = $1 AND predecessor_item_id = $2 \
         AND successor_item_id = $3",
    )
    .bind(fixture.scope.workspace_id)
    .bind(predecessor.id)
    .bind(successor.id)
    .fetch_one(&fixture.database.pool)
    .await
    .expect("normalized dependency edge exists");
    assert_eq!(
        stored_edge,
        ("finish_to_start".to_owned(), 900, "hard".to_owned(), None)
    );

    let undone = fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "dependency-proposal-undo",
            None,
        )
        .await
        .expect("dependency change undo restores the prior graph");
    assert_eq!(undone.application.status, ProposalApplicationStatus::Undone);
    let restored = fixture
        .items
        .get(successor.id)
        .await
        .expect("restored successor reads");
    assert_eq!(restored.flexible_constraints, json!({}));
    assert!(restored.revision > current.revision);
    let remaining_edges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM item_dependencies WHERE workspace_id = $1 \
         AND successor_item_id = $2",
    )
    .bind(fixture.scope.workspace_id)
    .bind(successor.id)
    .fetch_one(&fixture.database.pool)
    .await
    .expect("dependency graph inspected after undo");
    assert_eq!(remaining_edges, 0);

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn dependency_rewire_uses_final_batch_graph_for_apply_and_undo() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; dependency rewire test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let first = fixture
        .items
        .create(
            item(Uuid::new_v4(), ItemKind::Task, "First task", false, None),
            item_key("dependency-rewire-first", 93),
        )
        .await
        .expect("first item created")
        .item;
    let second = fixture
        .items
        .create(
            item(Uuid::new_v4(), ItemKind::Task, "Second task", false, None),
            item_key("dependency-rewire-second", 94),
        )
        .await
        .expect("second item created")
        .item;
    let mut second_with_dependency = replacement(&second, second.status);
    second_with_dependency.flexible_constraints = dependency_metadata(first.id);
    let second = fixture
        .items
        .replace(
            second.id,
            second.revision,
            second_with_dependency,
            item_key("dependency-rewire-seed", 95),
        )
        .await
        .expect("initial first-to-second edge created")
        .item;
    let baseline_delta = fixture
        .items
        .delta(None, 200)
        .await
        .expect("baseline item delta");
    assert!(!baseline_delta.has_more);

    let mut first_replacement = replacement(&first, first.status);
    first_replacement.flexible_constraints = dependency_metadata(second.id);
    let mut second_replacement = replacement(&second, second.status);
    second_replacement.flexible_constraints = json!({});
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::ConstraintChange,
        "Reverse a dependency without a transient cycle",
        vec![
            // This addition deliberately precedes removal of the reverse edge.
            // A per-command graph validator would reject it even though the
            // reviewed final graph is acyclic.
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: first.id,
                expected_revision: first.revision,
                item: first_replacement,
            },
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: second.id,
                expected_revision: second.revision,
                item: second_replacement,
            },
        ],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("final acyclic rewire previews");
    assert!(preview.can_apply);
    assert_eq!(
        preview
            .risks
            .iter()
            .filter(|risk| risk.code == ProposalRiskCode::ChangesDependencies)
            .count(),
        2
    );
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "dependency-rewire-apply",
            None,
        )
        .await
        .expect("final acyclic rewire applies");
    let applied_edges: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT predecessor_item_id, successor_item_id FROM item_dependencies \
         WHERE workspace_id = $1 ORDER BY predecessor_item_id, successor_item_id",
    )
    .bind(fixture.scope.workspace_id)
    .fetch_all(&fixture.database.pool)
    .await
    .expect("rewired graph reads");
    assert_eq!(applied_edges, vec![(second.id, first.id)]);

    let first_rewire_page = fixture
        .items
        .delta(Some(&baseline_delta.next_cursor), 1)
        .await
        .expect("atomic rewire delta page");
    assert_eq!(first_rewire_page.changes.len(), 2);
    assert!(!first_rewire_page.has_more);
    let changed_items = first_rewire_page
        .changes
        .iter()
        .map(|change| match change {
            DeltaChange::Upsert { item } => item.as_ref(),
            DeltaChange::Tombstone { .. } => panic!("rewire changes must be item upserts"),
        })
        .collect::<Vec<_>>();
    let removed_edge_item = changed_items
        .iter()
        .find(|item| item.id == second.id)
        .expect("rewire removal is delivered in the atomic group");
    assert_eq!(removed_edge_item.flexible_constraints, json!({}));
    let added_edge_item = changed_items
        .iter()
        .find(|item| item.id == first.id)
        .expect("rewire addition is delivered in the atomic group");
    assert_eq!(
        added_edge_item.flexible_constraints,
        dependency_metadata(second.id),
        "one atomic page moves the client directly between acyclic graphs"
    );

    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "dependency-rewire-undo",
            None,
        )
        .await
        .expect("rewire undo uses its final graph");
    let restored_edges: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT predecessor_item_id, successor_item_id FROM item_dependencies \
         WHERE workspace_id = $1 ORDER BY predecessor_item_id, successor_item_id",
    )
    .bind(fixture.scope.workspace_id)
    .fetch_all(&fixture.database.pool)
    .await
    .expect("restored graph reads");
    assert_eq!(restored_edges, vec![(first.id, second.id)]);

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn failed_late_command_rolls_back_every_canonical_and_application_write() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal rollback test skipped");
        return;
    };
    let test_database = TestDatabase::create(&database_url).await;
    MIGRATOR
        .run(&test_database.pool)
        .await
        .expect("migrations apply");
    let scope = seed_scope(&test_database.pool).await;
    let clock = Arc::new(SystemClock);
    let proposals = PostgresProposalRepository::new(test_database.pool.clone(), scope);
    let items = ItemService::new(
        Arc::new(PostgresItemRepository::new(
            test_database.pool.clone(),
            scope,
        )),
        clock.clone(),
    );
    let applications =
        PostgresProposalApplicationRepository::new(test_database.pool.clone(), scope);

    let duplicate_id = Uuid::new_v4();
    items
        .create(
            item(duplicate_id, ItemKind::Task, "Already exists", false, None),
            IdempotencyKey {
                key: "existing-item-key-0001".to_owned(),
                fingerprint: [7; 32],
            },
        )
        .await
        .unwrap();
    let would_leak_id = Uuid::new_v4();
    let commands = vec![
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(would_leak_id, ItemKind::Task, "Must roll back", true, None),
        },
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                duplicate_id,
                ItemKind::Task,
                "Duplicate target",
                false,
                None,
            ),
        },
    ];
    let now = Utc::now();
    let proposal = proposals
        .insert(
            Proposal::new(
                NewProposal {
                    submitted_by: "device:test-owner".to_owned(),
                    source: ProposalSource::Codex,
                    source_reference: None,
                    kind: ProposalKind::GoalBreakdown,
                    title: "Atomic rollback".to_owned(),
                    explanation: None,
                    payload: serde_json::to_value(ProposalChangeSet::new(commands).unwrap())
                        .unwrap(),
                    expires_at: now + Duration::days(1),
                },
                now,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let preview = applications
        .preview(ProposalPreviewRequest {
            proposals: vec![ProposalPreviewMember {
                proposal_id: proposal.id,
                expected_revision: 1,
            }],
        })
        .await
        .unwrap();
    assert!(!preview.can_apply);

    assert!(matches!(
        applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "proposal-failing-key-0001",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(_))
    ));
    let leaked_item: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id=$1 AND id=$2)")
            .bind(scope.workspace_id)
            .bind(would_leak_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert!(!leaked_item);
    let application_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM proposal_applications WHERE workspace_id=$1")
            .bind(scope.workspace_id)
            .fetch_one(&test_database.pool)
            .await
            .unwrap();
    assert_eq!(application_count, 0);
    assert_eq!(
        proposals.get(proposal.id).await.unwrap().status,
        ProposalStatus::Pending
    );

    test_database.destroy().await;
}

#[tokio::test]
async fn implicit_provider_managed_parent_blocks_preview_and_apply() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; provider-parent test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Imported parent",
                false,
                None,
            ),
            item_key("provider-parent-create", 11),
        )
        .await
        .expect("parent created")
        .item;
    mark_provider_managed(&fixture.database.pool, fixture.scope, &parent).await;

    let child_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Attach child to imported parent",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                child_id,
                ItemKind::Task,
                "Must remain unapplied",
                false,
                Some(parent.id),
            ),
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("unsafe proposal still returns a reviewable blocked preview");
    assert!(!preview.can_apply);
    assert!(
        preview
            .conflicts
            .iter()
            .any(|conflict| conflict.code == ProposalConflictCode::ProviderManagedItem)
    );

    assert!(matches!(
        fixture
            .applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "provider-parent-apply",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewNotApplicable
        ))
    ));
    assert!(!item_exists(&fixture.database.pool, fixture.scope, child_id).await);
    assert_eq!(
        fixture.proposals.get(proposal.id).await.unwrap().status,
        ProposalStatus::Pending
    );

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn undo_restores_completion_and_deletion_timestamps_while_advancing_revisions() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; exact undo test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;

    let mut completed_input = item(
        Uuid::new_v4(),
        ItemKind::Task,
        "Originally completed",
        false,
        None,
    );
    completed_input.status = ItemStatus::Completed;
    let completed = fixture
        .items
        .create(completed_input, item_key("undo-completed-create", 21))
        .await
        .expect("completed item created")
        .item;
    let original_completed_at = completed.completed_at.expect("completion timestamp");

    let deleted = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Originally deleted",
                false,
                None,
            ),
            item_key("undo-deleted-create", 22),
        )
        .await
        .expect("delete fixture created")
        .item;
    let deleted = fixture
        .items
        .trash(
            deleted.id,
            deleted.revision,
            item_key("undo-deleted-trash", 23),
        )
        .await
        .expect("delete fixture trashed")
        .item;
    let original_deleted_at = deleted.deleted_at.expect("deletion timestamp");

    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Exercise exact snapshot undo",
        vec![
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: completed.id,
                expected_revision: completed.revision,
                item: replacement(&completed, ItemStatus::Inbox),
            },
            ProposalCommand::RestoreItem {
                command_id: Uuid::new_v4(),
                item_id: deleted.id,
                expected_revision: deleted.revision,
            },
        ],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("snapshot changes preview");
    assert!(preview.can_apply);
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "snapshot-apply-key",
            None,
        )
        .await
        .expect("snapshot changes apply");
    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "snapshot-undo-key",
            None,
        )
        .await
        .expect("snapshot changes undo");

    let completed_state =
        item_timestamps(&fixture.database.pool, fixture.scope, completed.id).await;
    assert_eq!(completed_state.0, completed.revision + 2);
    assert_eq!(completed_state.1, Some(original_completed_at));
    assert_eq!(completed_state.2, None);

    let deleted_state = item_timestamps(&fixture.database.pool, fixture.scope, deleted.id).await;
    assert_eq!(deleted_state.0, deleted.revision + 2);
    assert_eq!(deleted_state.1, deleted.completed_at);
    assert_eq!(deleted_state.2, Some(original_deleted_at));

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers both proposal apply and snapshot undo terminal guards.
async fn proposal_terminal_apply_and_undo_wait_for_execution_to_close() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal execution guard test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let execution_clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let execution_items = Arc::new(ItemService::new(
        Arc::new(PostgresItemRepository::new(
            fixture.database.pool.clone(),
            fixture.scope,
        )),
        execution_clock.clone(),
    ));
    let execution = ExecutionService::new(
        Arc::new(PostgresExecutionRepository::new(
            fixture.database.pool.clone(),
            fixture.scope,
        )),
        execution_items,
        execution_clock,
    );

    let projected = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Proposal completion waits for execution",
                false,
                None,
            ),
            item_key("proposal-execution-create", 51),
        )
        .await
        .expect("projection fixture created")
        .item;
    let projection_proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Complete the active item",
        vec![ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: projected.id,
            expected_revision: projected.revision,
            item: replacement(&projected, ItemStatus::Completed),
        }],
    )
    .await;
    let projection_preview = fixture
        .applications
        .preview(preview_request(&projection_proposal))
        .await
        .expect("terminal proposal previews before execution starts");
    assert!(projection_preview.can_apply);

    let projection_session = Uuid::new_v4();
    execution
        .command(
            0,
            ExecutionCommand::Start(StartExecution {
                session_id: projection_session,
                item_id: projected.id,
                item_revision: projected.revision,
                occurrence_id: None,
                session_index: 0,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_key("proposal-execution-start", 52),
        )
        .await
        .expect("projection execution starts");
    let blocked_apply_effects_before =
        application_side_effect_counts(&fixture.database.pool, fixture.scope).await;
    let blocked_apply = fixture
        .applications
        .apply(
            projection_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: projection_preview.review_hash.clone(),
            },
            "proposal-execution-apply",
            None,
        )
        .await;
    assert!(matches!(
        blocked_apply,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::InvalidItem
        ))
    ));
    assert_eq!(
        fixture.items.get(projected.id).await.unwrap().status,
        projected.status
    );
    assert_eq!(
        application_side_effect_counts(&fixture.database.pool, fixture.scope).await,
        blocked_apply_effects_before,
        "failed apply must roll back receipts, fences, item deltas, outbox, and audit",
    );

    execution
        .command(
            1,
            ExecutionCommand::Complete(FinishExecution {
                session_id: projection_session,
                actual_seconds: Some(0),
            }),
            execution_key("proposal-execution-close", 53),
        )
        .await
        .expect("projection execution closes");
    let applied = fixture
        .applications
        .apply(
            projection_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: projection_preview.review_hash.clone(),
            },
            "proposal-execution-apply",
            None,
        )
        .await
        .expect("same failed apply key succeeds after close");
    assert!(!applied.replayed);
    assert_eq!(
        fixture.items.get(projected.id).await.unwrap().status,
        ItemStatus::Completed
    );
    let terminal_projection = fixture.items.get(projected.id).await.unwrap();
    let reopened_projection = fixture
        .items
        .replace(
            terminal_projection.id,
            terminal_projection.revision,
            replacement(&terminal_projection, ItemStatus::Planned),
            item_key("proposal-execution-reopen", 57),
        )
        .await
        .expect("reopen applied item before exact replay")
        .item;
    let projection_replay_session = Uuid::new_v4();
    execution
        .command(
            2,
            ExecutionCommand::Start(StartExecution {
                session_id: projection_replay_session,
                item_id: reopened_projection.id,
                item_revision: reopened_projection.revision,
                occurrence_id: None,
                session_index: 1,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_key("proposal-execution-replay-start", 58),
        )
        .await
        .expect("later lease opens before exact apply replay");
    let replayed_apply = fixture
        .applications
        .apply(
            projection_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: projection_preview.review_hash,
            },
            "proposal-execution-apply",
            None,
        )
        .await
        .expect("exact apply response replays during later lease");
    assert!(replayed_apply.replayed);
    assert_eq!(
        replayed_apply.application.application_id,
        applied.application.application_id
    );
    assert_eq!(
        fixture.items.get(projected.id).await.unwrap(),
        reopened_projection
    );
    execution
        .command(
            3,
            ExecutionCommand::Complete(FinishExecution {
                session_id: projection_replay_session,
                actual_seconds: Some(0),
            }),
            execution_key("proposal-execution-replay-close", 59),
        )
        .await
        .expect("close later apply-replay lease");

    let mut originally_completed = item(
        Uuid::new_v4(),
        ItemKind::Task,
        "Undo completion waits for execution",
        false,
        None,
    );
    originally_completed.status = ItemStatus::Completed;
    let originally_completed = fixture
        .items
        .create(originally_completed, item_key("undo-execution-create", 54))
        .await
        .expect("undo fixture created")
        .item;
    let undo_proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Reopen a completed item",
        vec![ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: originally_completed.id,
            expected_revision: originally_completed.revision,
            item: replacement(&originally_completed, ItemStatus::Planned),
        }],
    )
    .await;
    let undo_preview = fixture
        .applications
        .preview(preview_request(&undo_proposal))
        .await
        .expect("undo fixture previews");
    let undo_application = fixture
        .applications
        .apply(
            undo_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: undo_preview.review_hash,
            },
            "undo-execution-apply",
            None,
        )
        .await
        .expect("undo fixture applies");
    let reopened = fixture
        .items
        .get(originally_completed.id)
        .await
        .expect("completed item reopened");
    assert_eq!(reopened.status, ItemStatus::Planned);

    let undo_session = Uuid::new_v4();
    execution
        .command(
            4,
            ExecutionCommand::Start(StartExecution {
                session_id: undo_session,
                item_id: reopened.id,
                item_revision: reopened.revision,
                occurrence_id: None,
                session_index: 1,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_key("undo-execution-start", 55),
        )
        .await
        .expect("undo fixture execution starts");
    let blocked_undo_effects_before =
        application_side_effect_counts(&fixture.database.pool, fixture.scope).await;
    let blocked_undo = fixture
        .applications
        .undo(
            undo_application.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: undo_application.application.application_revision,
            },
            "undo-execution-undo",
            None,
        )
        .await;
    assert!(matches!(
        blocked_undo,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::InvalidItem
        ))
    ));
    assert_eq!(
        application_side_effect_counts(&fixture.database.pool, fixture.scope).await,
        blocked_undo_effects_before,
        "failed undo must roll back receipts, fences, item deltas, outbox, and audit",
    );

    execution
        .command(
            5,
            ExecutionCommand::Complete(FinishExecution {
                session_id: undo_session,
                actual_seconds: Some(0),
            }),
            execution_key("undo-execution-close", 56),
        )
        .await
        .expect("undo fixture execution closes");
    let undone = fixture
        .applications
        .undo(
            undo_application.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: undo_application.application.application_revision,
            },
            "undo-execution-undo",
            None,
        )
        .await
        .expect("same failed undo key succeeds after close");
    assert!(!undone.replayed);
    assert_eq!(
        fixture
            .items
            .get(originally_completed.id)
            .await
            .unwrap()
            .status,
        ItemStatus::Completed
    );
    let restored_terminal = fixture
        .items
        .get(originally_completed.id)
        .await
        .expect("terminal snapshot restored");
    let reopened_after_undo = fixture
        .items
        .replace(
            restored_terminal.id,
            restored_terminal.revision,
            replacement(&restored_terminal, ItemStatus::Planned),
            item_key("undo-execution-reopen-replay", 60),
        )
        .await
        .expect("reopen restored item before exact undo replay")
        .item;
    let undo_replay_session = Uuid::new_v4();
    execution
        .command(
            6,
            ExecutionCommand::Start(StartExecution {
                session_id: undo_replay_session,
                item_id: reopened_after_undo.id,
                item_revision: reopened_after_undo.revision,
                occurrence_id: None,
                session_index: 2,
                planned_block_id: None,
                device_id: Uuid::new_v4(),
            }),
            execution_key("undo-execution-replay-start", 61),
        )
        .await
        .expect("later lease opens before exact undo replay");
    let replayed_undo = fixture
        .applications
        .undo(
            undo_application.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: undo_application.application.application_revision,
            },
            "undo-execution-undo",
            None,
        )
        .await
        .expect("exact undo response replays during later lease");
    assert!(replayed_undo.replayed);
    assert_eq!(
        replayed_undo.application.application_id,
        undone.application.application_id
    );
    assert_eq!(
        fixture.items.get(originally_completed.id).await.unwrap(),
        reopened_after_undo
    );

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Covers apply, a post-apply provider boundary, and exact undo in one scope.
async fn undoing_child_field_replace_does_not_touch_unchanged_provider_managed_parent() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; child replace undo test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Unchanged parent",
                false,
                None,
            ),
            item_key("unchanged-parent-create", 71),
        )
        .await
        .expect("parent created")
        .item;
    let child = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Original child title",
                false,
                Some(parent.id),
            ),
            item_key("unchanged-parent-child-create", 72),
        )
        .await
        .expect("child created")
        .item;
    let parent_before = fixture
        .items
        .get(parent.id)
        .await
        .expect("parent refreshed after child creation");
    let mut changed = replacement(&child, child.status);
    changed.title = "Temporarily replaced child title".to_owned();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Replace only a child field",
        vec![ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: child.id,
            expected_revision: child.revision,
            item: changed,
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("child field replacement previews");
    assert!(preview.can_apply);
    assert!(preview.implicit_diffs.is_empty());
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "child-field-replace-apply",
            None,
        )
        .await
        .expect("child field replacement applies");
    assert_eq!(
        fixture.items.get(parent.id).await.unwrap().revision,
        parent_before.revision
    );

    mark_provider_managed(&fixture.database.pool, fixture.scope, &parent_before).await;
    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "child-field-replace-undo",
            None,
        )
        .await
        .expect("undo does not cross the unchanged provider parent boundary");

    let restored_child = fixture.items.get(child.id).await.expect("child restored");
    assert_eq!(restored_child.title, child.title);
    assert_eq!(restored_child.revision, child.revision + 2);
    let parent_after = fixture
        .items
        .get(parent.id)
        .await
        .expect("parent remains active");
    assert_eq!(parent_after.revision, parent_before.revision);
    assert_eq!(parent_after.updated_at, parent_before.updated_at);
    assert_eq!(parent_after.is_executable, parent_before.is_executable);

    fixture.database.destroy().await;
}

#[tokio::test]
async fn restore_under_an_executing_parent_is_rejected() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; executing-parent test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent = fixture
        .items
        .create(
            item(Uuid::new_v4(), ItemKind::Goal, "Parent", false, None),
            item_key("executing-parent-create", 31),
        )
        .await
        .expect("parent created")
        .item;
    let child = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Child",
                false,
                Some(parent.id),
            ),
            item_key("executing-child-create", 32),
        )
        .await
        .expect("child created")
        .item;
    let child = fixture
        .items
        .trash(
            child.id,
            child.revision,
            item_key("executing-child-trash", 33),
        )
        .await
        .expect("child trashed")
        .item;
    let refreshed_parent = fixture
        .items
        .get(parent.id)
        .await
        .expect("parent refreshed");
    fixture
        .items
        .replace(
            parent.id,
            refreshed_parent.revision,
            replacement(&refreshed_parent, ItemStatus::InProgress),
            item_key("executing-parent-start", 34),
        )
        .await
        .expect("leaf parent can start");

    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Invalid child restore",
        vec![ProposalCommand::RestoreItem {
            command_id: Uuid::new_v4(),
            item_id: child.id,
            expected_revision: child.revision,
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("invalid restore returns a blocked preview");
    assert!(!preview.can_apply);
    assert!(
        preview
            .conflicts
            .iter()
            .any(|conflict| conflict.code == ProposalConflictCode::InvalidParentState)
    );
    assert!(matches!(
        fixture
            .applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "executing-parent-apply",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewNotApplicable
        ))
    ));
    assert!(
        item_timestamps(&fixture.database.pool, fixture.scope, child.id)
            .await
            .2
            .is_some()
    );

    fixture.database.destroy().await;
}

#[tokio::test]
async fn preview_includes_implicit_existing_parent_diff() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; implicit diff test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let mut existing_parent = item(
        Uuid::new_v4(),
        ItemKind::Goal,
        "Existing parent",
        false,
        None,
    );
    existing_parent.flexible_constraints = json!({"has_own_effort": true});
    existing_parent.has_own_effort = Some(true);
    let parent = fixture
        .items
        .create(existing_parent, item_key("implicit-parent-create", 41))
        .await
        .expect("parent created")
        .item;
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Show parent side effect",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "New child",
                false,
                Some(parent.id),
            ),
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("hierarchy change previews");
    assert!(preview.can_apply);
    let implicit = preview
        .implicit_diffs
        .iter()
        .find(|diff| diff.item_id == parent.id)
        .expect("existing parent is shown as an implicit diff");
    assert_eq!(
        implicit.reason,
        ProposalImplicitChangeReason::HierarchyRefresh
    );
    assert!(implicit.before.is_executable);
    assert!(!implicit.after.is_executable);
    assert_eq!(implicit.before.revision, parent.revision);
    assert_eq!(implicit.after.revision, parent.revision + 1);
    assert!(
        implicit
            .changed_fields
            .contains(&ProposalItemField::IsExecutable)
    );
    assert!(
        implicit
            .changed_fields
            .contains(&ProposalItemField::Revision)
    );

    fixture.database.destroy().await;
}

#[tokio::test]
async fn conflicted_preview_cannot_apply_after_its_missing_parent_appears() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; conflicted-preview test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let missing_parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Initially invalid hierarchy",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                child_id,
                ItemKind::Task,
                "Blocked child",
                false,
                Some(missing_parent_id),
            ),
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("conflict is reviewable");
    assert!(!preview.can_apply);
    assert!(
        preview
            .conflicts
            .iter()
            .any(|conflict| conflict.code == ProposalConflictCode::ParentNotFound)
    );

    fixture
        .items
        .create(
            item(
                missing_parent_id,
                ItemKind::Goal,
                "Parent created later",
                false,
                None,
            ),
            item_key("late-parent-create", 51),
        )
        .await
        .expect("missing parent later appears");
    assert!(matches!(
        fixture
            .applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "conflicted-preview-apply",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewNotApplicable
        ))
    ));
    assert!(!item_exists(&fixture.database.pool, fixture.scope, child_id).await);
    assert_eq!(
        fixture.proposals.get(proposal.id).await.unwrap().status,
        ProposalStatus::Pending
    );

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercises the complete apply, retention, evidence, and undo lifecycle.
async fn expired_effect_snapshot_scrubbing_preserves_hash_evidence_and_blocks_undo() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; snapshot scrubbing test skipped");
        return;
    };
    let old_now = Utc::now() - Duration::days(2);
    let fixture =
        ApplicationFixture::create_with_clock(&database_url, Arc::new(FixedClock(old_now))).await;
    let source_id = Uuid::new_v4();
    let source_proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Create snapshot retention source",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                source_id,
                ItemKind::Task,
                "Snapshot retention source",
                false,
                None,
            ),
        }],
        old_now,
    )
    .await;
    let source_preview = fixture
        .applications
        .preview(preview_request(&source_proposal))
        .await
        .expect("source preview succeeds");
    fixture
        .applications
        .apply(
            source_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: source_preview.review_hash,
            },
            "snapshot-retention-source-apply",
            None,
        )
        .await
        .expect("source application succeeds");
    let original = fixture
        .items
        .get(source_id)
        .await
        .expect("applied source item exists");
    let mut changed = replacement(&original, ItemStatus::Planned);
    changed.notes = Some("Retained only until undo expires".to_owned());
    let proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Create expiring undo evidence",
        vec![ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: original.id,
            expected_revision: original.revision,
            item: changed,
        }],
        old_now,
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("old-time preview succeeds");
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "snapshot-retention-apply",
            None,
        )
        .await
        .expect("old-time application succeeds");
    let before = effect_snapshot_evidence(
        &fixture.database.pool,
        fixture.scope,
        applied.application.application_id,
    )
    .await;
    assert!(before.before_snapshot.is_some());
    assert!(before.after_snapshot.is_some());
    assert!(before.snapshots_scrubbed_at.is_none());
    assert_eq!(before.command_hash.len(), 32);
    assert_eq!(
        before
            .before_snapshot_hash
            .as_ref()
            .expect("replace effects retain a before hash")
            .len(),
        32
    );
    assert_eq!(before.after_snapshot_hash.len(), 32);

    let undo_deadline = applied.application.undo_expires_at;
    let current_applications = PostgresProposalApplicationRepository::new_with_test_clock(
        fixture.database.pool.clone(),
        fixture.scope,
        Arc::new(FixedClock(undo_deadline)),
    );
    let trigger_proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Trigger retention cleanup",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Current proposal",
                false,
                None,
            ),
        }],
        undo_deadline,
    )
    .await;
    current_applications
        .preview(preview_request(&trigger_proposal))
        .await
        .expect("a current preview triggers retention cleanup");

    let after = effect_snapshot_evidence(
        &fixture.database.pool,
        fixture.scope,
        applied.application.application_id,
    )
    .await;
    assert_eq!(after.command_hash, before.command_hash);
    assert_eq!(after.before_snapshot_hash, before.before_snapshot_hash);
    assert_eq!(after.after_snapshot_hash, before.after_snapshot_hash);
    assert!(after.before_snapshot.is_none());
    assert!(after.after_snapshot.is_none());
    assert!(after.snapshots_scrubbed_at.is_some());
    assert_eq!(
        current_applications
            .get(applied.application.application_id)
            .await
            .expect("hash-only application evidence remains readable"),
        applied.application
    );
    assert!(matches!(
        current_applications
            .undo(
                applied.application.application_id,
                ProposalUndoRequest {
                    expected_application_revision: applied.application.application_revision,
                },
                "expired-snapshot-undo",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::UndoExpired
        ))
    ));

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercises both sides of preview retention in one isolated scope.
async fn expired_unapplied_preview_is_pruned_but_applied_preview_evidence_remains() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; preview pruning test skipped");
        return;
    };
    let old_now = Utc::now() - Duration::days(2);
    let fixture =
        ApplicationFixture::create_with_clock(&database_url, Arc::new(FixedClock(old_now))).await;
    let applied_proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Applied preview retention",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Applied old proposal",
                false,
                None,
            ),
        }],
        old_now,
    )
    .await;
    let applied_preview = fixture
        .applications
        .preview(preview_request(&applied_proposal))
        .await
        .expect("applied preview created");
    let applied = fixture
        .applications
        .apply(
            applied_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: applied_preview.review_hash,
            },
            "applied-preview-retention",
            None,
        )
        .await
        .expect("preview applied");

    let unapplied_proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Disposable expired preview",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Unapplied old proposal",
                false,
                None,
            ),
        }],
        old_now,
    )
    .await;
    let unapplied_preview = fixture
        .applications
        .preview(preview_request(&unapplied_proposal))
        .await
        .expect("unapplied preview created");
    assert!(
        preview_exists(
            &fixture.database.pool,
            fixture.scope,
            unapplied_preview.preview_id,
        )
        .await
    );

    let current_applications =
        PostgresProposalApplicationRepository::new(fixture.database.pool.clone(), fixture.scope);
    let current_proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Current preview",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Current proposal item",
                false,
                None,
            ),
        }],
    )
    .await;
    current_applications
        .preview(preview_request(&current_proposal))
        .await
        .expect("current preview prunes expired unapplied evidence");

    assert!(
        !preview_exists(
            &fixture.database.pool,
            fixture.scope,
            unapplied_preview.preview_id,
        )
        .await
    );
    assert_eq!(
        preview_member_count(
            &fixture.database.pool,
            fixture.scope,
            unapplied_preview.preview_id,
        )
        .await,
        0
    );
    assert!(
        preview_exists(
            &fixture.database.pool,
            fixture.scope,
            applied_preview.preview_id,
        )
        .await
    );
    assert_eq!(
        preview_member_count(
            &fixture.database.pool,
            fixture.scope,
            applied_preview.preview_id,
        )
        .await,
        1
    );
    assert_eq!(
        current_applications
            .get(applied.application.application_id)
            .await
            .expect("applied evidence remains readable"),
        applied.application
    );

    fixture.database.destroy().await;
}

#[tokio::test]
async fn preview_member_delete_then_insert_cannot_replace_reviewed_evidence() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; preview immutability test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Immutable preview membership",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Immutable preview target",
                false,
                None,
            ),
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("preview created");
    let before =
        preview_member_evidence(&fixture.database.pool, fixture.scope, preview.preview_id).await;

    let mut replacement = fixture.database.pool.begin().await.unwrap();
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *replacement)
        .await
        .unwrap();
    let error = sqlx::query(
        "WITH removed AS ( \
             DELETE FROM proposal_apply_preview_members \
              WHERE workspace_id=$1 AND user_id=$2 AND preview_id=$3 \
              RETURNING workspace_id,user_id,preview_id,ordinal,proposal_id, \
                        proposal_revision,proposal_payload_hash \
         ) INSERT INTO proposal_apply_preview_members (workspace_id,user_id,preview_id,ordinal, \
             proposal_id,proposal_revision,proposal_payload_hash) \
             SELECT workspace_id,user_id,preview_id,ordinal,proposal_id,proposal_revision, \
                    proposal_payload_hash FROM removed",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(preview.preview_id)
    .execute(&mut *replacement)
    .await
    .expect_err("delete-and-reinsert cannot replace reviewed membership");
    assert_sqlstate(&error, "23514");
    replacement.rollback().await.unwrap();

    let after =
        preview_member_evidence(&fixture.database.pool, fixture.scope, preview.preview_id).await;
    assert_eq!(after, before);
    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Builds the smallest complete counterfeit header needed to reach the global claim.
async fn proposal_has_one_durable_application_claim_under_sql_fault_injection() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal claim test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Unique durable proposal claim",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Unique application target",
                false,
                None,
            ),
        }],
    )
    .await;
    let first_preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("first preview created");
    let competing_preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("competing preview created before acceptance");
    let applied = fixture
        .applications
        .apply(
            first_preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: first_preview.review_hash,
            },
            "unique-proposal-claim-apply",
            None,
        )
        .await
        .expect("first application claims proposal");
    let apply_audit_id: Uuid = sqlx::query_scalar(
        "SELECT apply_audit_id FROM proposal_applications \
         WHERE workspace_id=$1 AND user_id=$2 AND id=$3",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(applied.application.application_id)
    .fetch_one(&fixture.database.pool)
    .await
    .unwrap();

    let counterfeit_application_id = Uuid::new_v4();
    let mut fault = fixture.database.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO proposal_applications (id,workspace_id,user_id,preview_id,preview_hash,status, \
             revision,effect_count,fence_count,apply_audit_id,applied_at,undo_expires_at) \
         SELECT $1,preview.workspace_id,preview.user_id,preview.id,preview.preview_hash,'applied', \
                1,preview.command_count,1,$2,preview.created_at, \
                preview.created_at + interval '1 hour' \
           FROM proposal_apply_previews AS preview \
          WHERE preview.workspace_id=$3 AND preview.user_id=$4 AND preview.id=$5",
    )
    .bind(counterfeit_application_id)
    .bind(apply_audit_id)
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(competing_preview.preview_id)
    .execute(&mut *fault)
    .await
    .expect("counterfeit header reaches the member claim constraint");
    let error = sqlx::query(
        "INSERT INTO proposal_application_members \
             (workspace_id,user_id,application_id,ordinal,proposal_id) VALUES ($1,$2,$3,0,$4)",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(counterfeit_application_id)
    .bind(proposal.id)
    .execute(&mut *fault)
    .await
    .expect_err("one proposal cannot be claimed by two applications");
    assert_sqlstate(&error, "23505");
    fault.rollback().await.unwrap();

    let claim_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM proposal_application_members \
         WHERE workspace_id=$1 AND user_id=$2 AND proposal_id=$3",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(proposal.id)
    .fetch_one(&fixture.database.pool)
    .await
    .unwrap();
    let application_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM proposal_applications WHERE workspace_id=$1 AND user_id=$2",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .fetch_one(&fixture.database.pool)
    .await
    .unwrap();
    assert_eq!(claim_count, 1);
    assert_eq!(application_count, 1);
    fixture.database.destroy().await;
}

#[tokio::test]
async fn apply_does_not_deadlock_with_proposal_first_audit_writer() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; proposal deadlock test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Proposal lock-order regression",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Deadlock-free application target",
                false,
                None,
            ),
        }],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("preview created");

    let mut proposal_writer = fixture.database.pool.begin().await.unwrap();
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *proposal_writer)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM proposals WHERE workspace_id=$1 AND id=$2 FOR UPDATE")
        .bind(fixture.scope.workspace_id)
        .bind(proposal.id)
        .execute(&mut *proposal_writer)
        .await
        .unwrap();

    let applications = fixture.applications.clone();
    let apply = tokio::spawn(async move {
        applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "proposal-lock-order-apply",
                None,
            )
            .await
    });
    wait_for_postgres_blocker(&fixture.database.pool, writer_pid).await;

    tokio::time::timeout(
        StdDuration::from_secs(5),
        sqlx::query(
            "INSERT INTO audit_operations (id,workspace_id,actor_user_id,operation_type, \
                 entity_type,entity_id,outcome) \
             VALUES ($1,$2,$3,'proposal.lock_order_probe','proposal',$4,'succeeded')",
        )
        .bind(Uuid::new_v4())
        .bind(fixture.scope.workspace_id)
        .bind(fixture.scope.user_id)
        .bind(proposal.id)
        .execute(&mut *proposal_writer),
    )
    .await
    .expect("audit FK does not wait on the application owner lock")
    .expect("audit probe succeeds without SQLSTATE 40P01");
    proposal_writer.commit().await.unwrap();

    let applied = tokio::time::timeout(StdDuration::from_secs(5), apply)
        .await
        .expect("application unblocks after proposal writer commits")
        .expect("application task completes")
        .expect("application succeeds without deadlock");
    assert!(!applied.replayed);
    assert_eq!(
        fixture.proposals.get(proposal.id).await.unwrap().status,
        ProposalStatus::Accepted
    );
    fixture.database.destroy().await;
}

#[tokio::test]
async fn apply_rechecks_time_after_lock_wait_and_rejects_exact_preview_expiry() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; preview expiry boundary test skipped");
        return;
    };
    let base = Utc::now();
    let clock = Arc::new(MutableClock::new(base));
    let fixture = ApplicationFixture::create_with_clock(&database_url, clock.clone()).await;
    let target_item_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal_at(
        &fixture.proposals,
        ProposalKind::CreateItem,
        "Exact preview expiry boundary",
        vec![ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(
                target_item_id,
                ItemKind::Task,
                "Must not apply at expiry",
                false,
                None,
            ),
        }],
        base,
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("preview created");

    let mut proposal_blocker = fixture.database.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *proposal_blocker)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM proposals WHERE workspace_id=$1 AND id=$2 FOR UPDATE")
        .bind(fixture.scope.workspace_id)
        .bind(proposal.id)
        .execute(&mut *proposal_blocker)
        .await
        .unwrap();

    let preview_expiry = preview.expires_at;
    let review_hash = preview.review_hash.clone();
    clock.set(preview_expiry - Duration::microseconds(1));
    let applications = fixture.applications.clone();
    let preview_id = preview.preview_id;
    let apply = tokio::spawn(async move {
        applications
            .apply(
                preview_id,
                ProposalApplyRequest {
                    expected_review_hash: review_hash,
                },
                "exact-preview-expiry-apply",
                None,
            )
            .await
    });
    wait_for_postgres_blocker(&fixture.database.pool, blocker_pid).await;
    clock.set(preview_expiry);
    proposal_blocker.commit().await.unwrap();

    let result = tokio::time::timeout(StdDuration::from_secs(5), apply)
        .await
        .expect("application completes after lock release")
        .expect("application task completes");
    assert!(matches!(
        result,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewExpired
        ))
    ));
    let stored = fixture.proposals.get(proposal.id).await.unwrap();
    assert_eq!(stored.status, ProposalStatus::Pending);
    assert_eq!(stored.revision, proposal.revision);
    assert!(!item_exists(&fixture.database.pool, fixture.scope, target_item_id).await);
    let evidence_counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT \
             (SELECT COUNT(*) FROM proposal_applications WHERE workspace_id=$1 AND user_id=$2), \
             (SELECT COUNT(*) FROM proposal_application_members WHERE workspace_id=$1 AND user_id=$2), \
             (SELECT COUNT(*) FROM proposal_application_requests WHERE workspace_id=$1 AND user_id=$2)",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .fetch_one(&fixture.database.pool)
    .await
    .unwrap();
    assert_eq!(evidence_counts, (0, 0, 0));
    assert_eq!(
        preview_member_count(&fixture.database.pool, fixture.scope, preview.preview_id,).await,
        1
    );
    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines, clippy::type_complexity)]
async fn hierarchy_batch_detaches_child_before_closing_parent_and_adapts_revision() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; hierarchy close test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Close after detaching",
                false,
                None,
            ),
            item_key("hierarchy-close-parent", 111),
        )
        .await
        .expect("parent created")
        .item;
    let child = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Detach before close",
                false,
                Some(parent.id),
            ),
            item_key("hierarchy-close-child", 112),
        )
        .await
        .expect("child created")
        .item;
    let parent = fixture
        .items
        .get(parent.id)
        .await
        .expect("parent refresh reads");

    let mut detached_child = replacement(&child, child.status);
    detached_child.parent_id = None;
    let parent_command_id = Uuid::new_v4();
    let child_command_id = Uuid::new_v4();
    let commands = vec![
        ProposalCommand::ReplaceItem {
            command_id: parent_command_id,
            item_id: parent.id,
            expected_revision: parent.revision,
            item: replacement(&parent, ItemStatus::Completed),
        },
        ProposalCommand::ReplaceItem {
            command_id: child_command_id,
            item_id: child.id,
            expected_revision: child.revision,
            item: detached_child,
        },
    ];
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Detach a child and close its parent",
        commands,
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("inter-target hierarchy batch previews");
    assert!(preview.can_apply);
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "hierarchy-close-apply",
            None,
        )
        .await
        .expect("child is detached before the parent closes");

    let closed_parent = fixture.items.get(parent.id).await.expect("closed parent");
    let detached_child = fixture.items.get(child.id).await.expect("detached child");
    assert_eq!(closed_parent.status, ItemStatus::Completed);
    assert_eq!(closed_parent.revision, parent.revision + 2);
    assert_eq!(detached_child.parent_id, None);
    let evidence: Vec<(Uuid, i16, i16, Option<i64>, Option<i64>, i64)> = sqlx::query_as(
        "SELECT action_id,ordinal,review_ordinal,expected_revision,before_revision,after_revision \
         FROM proposal_application_effects WHERE workspace_id=$1 AND user_id=$2 \
         AND application_id=$3 ORDER BY review_ordinal",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(applied.application.application_id)
    .fetch_all(&fixture.database.pool)
    .await
    .expect("effect ordering evidence");
    assert_eq!(
        evidence,
        vec![
            (
                parent_command_id,
                1,
                0,
                Some(i64::try_from(parent.revision).unwrap()),
                Some(i64::try_from(parent.revision).unwrap()),
                i64::try_from(parent.revision + 2).unwrap(),
            ),
            (
                child_command_id,
                0,
                1,
                Some(i64::try_from(child.revision).unwrap()),
                Some(i64::try_from(child.revision).unwrap()),
                i64::try_from(child.revision + 1).unwrap(),
            ),
        ]
    );
    let parent_audit_chain: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT operation_type,base_revision,result_revision FROM audit_operations \
         WHERE workspace_id=$1 AND entity_type='item' AND entity_id=$2 \
         AND base_revision >= $3 ORDER BY base_revision",
    )
    .bind(fixture.scope.workspace_id)
    .bind(parent.id)
    .bind(i64::try_from(parent.revision).unwrap())
    .fetch_all(&fixture.database.pool)
    .await
    .expect("parent audit chain");
    assert_eq!(
        parent_audit_chain,
        vec![
            (
                "item.hierarchy_changed".to_owned(),
                Some(i64::try_from(parent.revision).unwrap()),
                Some(i64::try_from(parent.revision + 1).unwrap()),
            ),
            (
                "item.updated".to_owned(),
                Some(i64::try_from(parent.revision + 1).unwrap()),
                Some(i64::try_from(parent.revision + 2).unwrap()),
            ),
        ]
    );

    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "hierarchy-close-undo",
            None,
        )
        .await
        .expect("undo reopens the parent before restoring its child");
    let restored_parent = fixture.items.get(parent.id).await.expect("restored parent");
    let restored_child = fixture.items.get(child.id).await.expect("restored child");
    assert_eq!(restored_parent.status, parent.status);
    assert_eq!(restored_child.parent_id, Some(parent.id));

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn hierarchy_batch_trashes_child_before_parent_and_undo_restores_parent_first() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; hierarchy trash test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Trash after child",
                false,
                None,
            ),
            item_key("hierarchy-trash-parent", 113),
        )
        .await
        .expect("parent created")
        .item;
    let child = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Trash before parent",
                false,
                Some(parent.id),
            ),
            item_key("hierarchy-trash-child", 114),
        )
        .await
        .expect("child created")
        .item;
    let parent = fixture
        .items
        .get(parent.id)
        .await
        .expect("refreshed parent");
    let parent_command_id = Uuid::new_v4();
    let child_command_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Trash a hierarchy atomically",
        vec![
            ProposalCommand::TrashItem {
                command_id: parent_command_id,
                item_id: parent.id,
                expected_revision: parent.revision,
            },
            ProposalCommand::TrashItem {
                command_id: child_command_id,
                item_id: child.id,
                expected_revision: child.revision,
            },
        ],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("parent-first review finds a safe execution order");
    assert!(preview.can_apply);
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "hierarchy-trash-apply",
            None,
        )
        .await
        .expect("child and parent trash atomically");
    assert!(
        item_timestamps(&fixture.database.pool, fixture.scope, child.id)
            .await
            .2
            .is_some()
    );
    assert!(
        item_timestamps(&fixture.database.pool, fixture.scope, parent.id)
            .await
            .2
            .is_some()
    );
    let ordinals: Vec<(Uuid, i16, i16)> = sqlx::query_as(
        "SELECT action_id,ordinal,review_ordinal FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 ORDER BY review_ordinal",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(applied.application.application_id)
    .fetch_all(&fixture.database.pool)
    .await
    .expect("trash execution evidence");
    assert_eq!(
        ordinals,
        vec![(parent_command_id, 1, 0), (child_command_id, 0, 1)]
    );

    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "hierarchy-trash-undo",
            None,
        )
        .await
        .expect("parent restores before child");
    assert_eq!(
        fixture
            .items
            .get(child.id)
            .await
            .expect("child restored")
            .parent_id,
        Some(parent.id)
    );
    assert!(fixture.items.get(parent.id).await.is_ok());

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn hierarchy_batch_reverses_parentage_without_a_transient_cycle() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; hierarchy reversal test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let old_parent = fixture
        .items
        .create(
            item(Uuid::new_v4(), ItemKind::Goal, "Old parent", false, None),
            item_key("hierarchy-reverse-parent", 115),
        )
        .await
        .expect("old parent created")
        .item;
    let old_child = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Old child",
                false,
                Some(old_parent.id),
            ),
            item_key("hierarchy-reverse-child", 116),
        )
        .await
        .expect("old child created")
        .item;
    let old_parent = fixture
        .items
        .get(old_parent.id)
        .await
        .expect("refreshed old parent");
    let mut parent_under_child = replacement(&old_parent, old_parent.status);
    parent_under_child.parent_id = Some(old_child.id);
    let mut child_at_root = replacement(&old_child, old_child.status);
    child_at_root.parent_id = None;
    let parent_command_id = Uuid::new_v4();
    let child_command_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Reverse a hierarchy",
        vec![
            ProposalCommand::ReplaceItem {
                command_id: parent_command_id,
                item_id: old_parent.id,
                expected_revision: old_parent.revision,
                item: parent_under_child,
            },
            ProposalCommand::ReplaceItem {
                command_id: child_command_id,
                item_id: old_child.id,
                expected_revision: old_child.revision,
                item: child_at_root,
            },
        ],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("hierarchy reversal previews");
    assert!(preview.can_apply);
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "hierarchy-reverse-apply",
            None,
        )
        .await
        .expect("hierarchy reversal applies");
    assert_eq!(
        fixture
            .items
            .get(old_parent.id)
            .await
            .expect("new child")
            .parent_id,
        Some(old_child.id)
    );
    assert_eq!(
        fixture
            .items
            .get(old_child.id)
            .await
            .expect("new parent")
            .parent_id,
        None
    );
    let ordinals: Vec<(Uuid, i16)> = sqlx::query_as(
        "SELECT action_id,ordinal FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 ORDER BY review_ordinal",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(applied.application.application_id)
    .fetch_all(&fixture.database.pool)
    .await
    .expect("reversal execution evidence");
    assert_eq!(
        ordinals,
        vec![(parent_command_id, 1), (child_command_id, 0)]
    );

    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "hierarchy-reverse-undo",
            None,
        )
        .await
        .expect("hierarchy reversal undoes");
    assert_eq!(
        fixture
            .items
            .get(old_child.id)
            .await
            .expect("original child restored")
            .parent_id,
        Some(old_parent.id)
    );
    assert_eq!(
        fixture
            .items
            .get(old_parent.id)
            .await
            .expect("original root restored")
            .parent_id,
        None
    );

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn created_parent_can_depend_on_its_new_child() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; staged identity test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let mut parent = item(parent_id, ItemKind::Goal, "Dependent parent", false, None);
    parent.flexible_constraints = dependency_metadata(child_id);
    let child_command_id = Uuid::new_v4();
    let parent_command_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::GoalBreakdown,
        "Create mutually referenced hierarchy identities",
        vec![
            ProposalCommand::CreateItem {
                command_id: child_command_id,
                item: item(
                    child_id,
                    ItemKind::Task,
                    "New dependency child",
                    false,
                    Some(parent_id),
                ),
            },
            ProposalCommand::CreateItem {
                command_id: parent_command_id,
                item: parent,
            },
        ],
    )
    .await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("neutral staged identities make the graph previewable");
    assert!(preview.can_apply);
    let applied = fixture
        .applications
        .apply(
            preview.preview_id,
            ProposalApplyRequest {
                expected_review_hash: preview.review_hash,
            },
            "staged-hierarchy-apply",
            None,
        )
        .await
        .expect("parent dependency and child hierarchy apply together");
    assert_eq!(
        fixture
            .items
            .get(child_id)
            .await
            .expect("child created")
            .parent_id,
        Some(parent_id)
    );
    assert_eq!(
        fixture
            .items
            .get(parent_id)
            .await
            .expect("parent created")
            .flexible_constraints,
        dependency_metadata(child_id)
    );
    let edge: (Uuid, Uuid) = sqlx::query_as(
        "SELECT predecessor_item_id,successor_item_id FROM item_dependencies \
         WHERE workspace_id=$1 AND predecessor_item_id=$2 AND successor_item_id=$3",
    )
    .bind(fixture.scope.workspace_id)
    .bind(child_id)
    .bind(parent_id)
    .fetch_one(&fixture.database.pool)
    .await
    .expect("parent dependency edge exists");
    assert_eq!(edge, (child_id, parent_id));
    let ordinals: Vec<(Uuid, i16)> = sqlx::query_as(
        "SELECT action_id,ordinal FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 ORDER BY review_ordinal",
    )
    .bind(fixture.scope.workspace_id)
    .bind(fixture.scope.user_id)
    .bind(applied.application.application_id)
    .fetch_all(&fixture.database.pool)
    .await
    .expect("staged create execution evidence");
    assert_eq!(
        ordinals,
        vec![(child_command_id, 1), (parent_command_id, 0)]
    );

    fixture
        .applications
        .undo(
            applied.application.application_id,
            ProposalUndoRequest {
                expected_application_revision: applied.application.application_revision,
            },
            "staged-hierarchy-undo",
            None,
        )
        .await
        .expect("created child trashes before its parent");
    assert!(
        item_timestamps(&fixture.database.pool, fixture.scope, child_id)
            .await
            .2
            .is_some()
    );
    assert!(
        item_timestamps(&fixture.database.pool, fixture.scope, parent_id)
            .await
            .2
            .is_some()
    );

    fixture.database.destroy().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn hierarchy_batch_rejects_initial_parent_revision_before_child_refresh() {
    let Ok(database_url) = std::env::var("DAYWEAVE_TEST_DATABASE_URL") else {
        eprintln!("DAYWEAVE_TEST_DATABASE_URL is unset; hierarchy pre-fence test skipped");
        return;
    };
    let fixture = ApplicationFixture::create(&database_url).await;
    let parent = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Revision-fenced parent",
                false,
                None,
            ),
            item_key("hierarchy-fence-parent", 117),
        )
        .await
        .expect("parent created")
        .item;
    let child = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Task,
                "Would refresh parent",
                false,
                Some(parent.id),
            ),
            item_key("hierarchy-fence-child", 118),
        )
        .await
        .expect("child created")
        .item;
    let parent = fixture
        .items
        .get(parent.id)
        .await
        .expect("refreshed parent");
    let mut detached_child = replacement(&child, child.status);
    detached_child.parent_id = None;
    let parent_command_id = Uuid::new_v4();
    let proposal = insert_change_set_proposal(
        &fixture.proposals,
        ProposalKind::UpdateItem,
        "Reject a revision that only a batch side effect could create",
        vec![
            ProposalCommand::ReplaceItem {
                command_id: parent_command_id,
                item_id: parent.id,
                // Detaching the child would advance the parent to this revision.
                // It must not make an initially invalid optimistic fence valid.
                expected_revision: parent.revision + 1,
                item: replacement(&parent, ItemStatus::Completed),
            },
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: child.id,
                expected_revision: child.revision,
                item: detached_child,
            },
        ],
    )
    .await;
    let side_effects_before =
        application_side_effect_counts(&fixture.database.pool, fixture.scope).await;
    let preview = fixture
        .applications
        .preview(preview_request(&proposal))
        .await
        .expect("revision mismatch is a blocked preview");
    assert!(!preview.can_apply);
    let conflict = preview
        .conflicts
        .iter()
        .find(|conflict| conflict.command_id == Some(parent_command_id))
        .expect("parent command owns the initial fence conflict");
    assert_eq!(conflict.code, ProposalConflictCode::ItemRevisionMismatch);
    assert_eq!(conflict.expected_revision, Some(parent.revision + 1));
    assert_eq!(conflict.actual_revision, Some(parent.revision));
    assert!(matches!(
        fixture
            .applications
            .apply(
                preview.preview_id,
                ProposalApplyRequest {
                    expected_review_hash: preview.review_hash,
                },
                "hierarchy-fence-apply",
                None,
            )
            .await,
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::PreviewNotApplicable
        ))
    ));
    assert_eq!(
        application_side_effect_counts(&fixture.database.pool, fixture.scope).await,
        side_effects_before,
        "pre-fence rejection leaks no item delta, audit, outbox, receipt, or request"
    );
    assert_eq!(
        fixture
            .items
            .get(parent.id)
            .await
            .expect("parent unchanged")
            .revision,
        parent.revision
    );
    assert_eq!(
        fixture
            .items
            .get(child.id)
            .await
            .expect("child unchanged")
            .parent_id,
        Some(parent.id)
    );

    fixture.database.destroy().await;
}

struct ApplicationFixture {
    database: TestDatabase,
    scope: DatabaseScope,
    proposals: PostgresProposalRepository,
    items: ItemService,
    applications: PostgresProposalApplicationRepository,
}

impl ApplicationFixture {
    async fn create(database_url: &str) -> Self {
        Self::create_with_clock(database_url, Arc::new(SystemClock)).await
    }

    async fn create_with_clock(database_url: &str, clock: Arc<dyn Clock>) -> Self {
        let database = TestDatabase::create(database_url).await;
        MIGRATOR
            .run(&database.pool)
            .await
            .expect("migrations apply");
        let scope = seed_scope(&database.pool).await;
        Self {
            proposals: PostgresProposalRepository::new(database.pool.clone(), scope),
            items: ItemService::new(
                Arc::new(PostgresItemRepository::new(database.pool.clone(), scope)),
                clock.clone(),
            ),
            applications: PostgresProposalApplicationRepository::new_with_test_clock(
                database.pool.clone(),
                scope,
                clock,
            ),
            database,
            scope,
        }
    }
}

async fn application_side_effect_counts(
    pool: &PgPool,
    scope: DatabaseScope,
) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT \
         (SELECT count(*) FROM proposal_applications WHERE workspace_id = $1 AND user_id = $2), \
         (SELECT count(*) FROM proposal_application_members WHERE workspace_id = $1 AND user_id = $2), \
         (SELECT count(*) FROM proposal_application_requests WHERE workspace_id = $1 AND user_id = $2), \
         (SELECT count(*) FROM item_changes WHERE workspace_id = $1), \
         (SELECT count(*) FROM outbox_messages WHERE workspace_id = $1), \
         (SELECT count(*) FROM audit_operations WHERE workspace_id = $1)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(pool)
    .await
    .expect("proposal application side-effect counts")
}

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

struct MutableClock(RwLock<DateTime<Utc>>);

impl MutableClock {
    fn new(now: DateTime<Utc>) -> Self {
        Self(RwLock::new(now))
    }

    fn set(&self, now: DateTime<Utc>) {
        *self.0.write().expect("test clock write lock") = now;
    }
}

impl Clock for MutableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.read().expect("test clock read lock")
    }
}

async fn insert_change_set_proposal(
    proposals: &PostgresProposalRepository,
    kind: ProposalKind,
    title: &str,
    commands: Vec<ProposalCommand>,
) -> Proposal {
    insert_change_set_proposal_at(proposals, kind, title, commands, Utc::now()).await
}

async fn insert_change_set_proposal_at(
    proposals: &PostgresProposalRepository,
    kind: ProposalKind,
    title: &str,
    commands: Vec<ProposalCommand>,
    now: DateTime<Utc>,
) -> Proposal {
    let proposal = Proposal::new(
        NewProposal {
            submitted_by: "device:test-owner".to_owned(),
            source: ProposalSource::Codex,
            source_reference: None,
            kind,
            title: title.to_owned(),
            explanation: None,
            payload: serde_json::to_value(ProposalChangeSet::new(commands).unwrap()).unwrap(),
            expires_at: now + Duration::days(1),
        },
        now,
    )
    .unwrap();
    proposals.insert(proposal).await.unwrap()
}

fn preview_request(proposal: &Proposal) -> ProposalPreviewRequest {
    ProposalPreviewRequest {
        proposals: vec![ProposalPreviewMember {
            proposal_id: proposal.id,
            expected_revision: proposal.revision,
        }],
    }
}

fn item_key(key: &str, marker: u8) -> IdempotencyKey {
    IdempotencyKey {
        key: key.to_owned(),
        fingerprint: [marker; 32],
    }
}

fn execution_key(key: &str, marker: u8) -> ExecutionIdempotencyKey {
    ExecutionIdempotencyKey {
        key: key.to_owned(),
        fingerprint: [marker; 32],
    }
}

fn replacement(item: &Item, status: ItemStatus) -> ReplaceItem {
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
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
        blocked_reason_kind: item.blocked_reason_kind,
        blocked_by_item_id: item.blocked_by_item_id,
        blocked_reason: item.blocked_reason.clone(),
    }
}

async fn mark_provider_managed(pool: &PgPool, scope: DatabaseScope, item: &Item) {
    let account_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO provider_accounts (id, workspace_id, user_id, provider, external_account_id, \
         display_label, encrypted_credentials, credential_key_version, status, sync_enabled, \
         is_default) VALUES ($1,$2,$3,'google',$4,'Synthetic provider parent',$5,1,'active',true,false)",
    )
    .bind(account_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("synthetic-provider-{account_id}"))
    .bind(vec![0xA5_u8; 64])
    .execute(pool)
    .await
    .expect("synthetic provider account");
    sqlx::query(
        "INSERT INTO provider_sync_mappings (id,workspace_id,provider_account_id,entity_kind, \
         local_entity_id,remote_resource_id,local_revision,sync_state,ownership,created_at,updated_at) \
         VALUES ($1,$2,$3,'item',$4,$5,$6,'synced','external',$7,$7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(account_id)
    .bind(item.id)
    .bind(format!("synthetic-remote-{}", item.id))
    .bind(i64::try_from(item.revision).unwrap())
    .bind(Utc::now())
    .execute(pool)
    .await
    .expect("synthetic provider mapping");
}

async fn item_exists(pool: &PgPool, scope: DatabaseScope, item_id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id=$1 AND id=$2)")
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn item_timestamps(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
) -> (u64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let (revision, completed_at, deleted_at): (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as(
            "SELECT revision,completed_at,trashed_at FROM items WHERE workspace_id=$1 AND id=$2",
        )
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_one(pool)
        .await
        .unwrap();
    (u64::try_from(revision).unwrap(), completed_at, deleted_at)
}

#[derive(sqlx::FromRow)]
struct EffectSnapshotEvidence {
    command_hash: Vec<u8>,
    before_snapshot_hash: Option<Vec<u8>>,
    after_snapshot_hash: Vec<u8>,
    before_snapshot: Option<Value>,
    after_snapshot: Option<Value>,
    snapshots_scrubbed_at: Option<DateTime<Utc>>,
}

async fn effect_snapshot_evidence(
    pool: &PgPool,
    scope: DatabaseScope,
    application_id: Uuid,
) -> EffectSnapshotEvidence {
    sqlx::query_as(
        "SELECT command_hash,before_snapshot_hash,after_snapshot_hash,before_snapshot, \
         after_snapshot,snapshots_scrubbed_at FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn preview_exists(pool: &PgPool, scope: DatabaseScope, preview_id: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proposal_apply_previews \
         WHERE workspace_id=$1 AND user_id=$2 AND id=$3)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn preview_member_count(pool: &PgPool, scope: DatabaseScope, preview_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM proposal_apply_preview_members \
         WHERE workspace_id=$1 AND user_id=$2 AND preview_id=$3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[derive(Debug, Eq, PartialEq, sqlx::FromRow)]
struct PreviewMemberEvidence {
    workspace_id: Uuid,
    user_id: Uuid,
    preview_id: Uuid,
    ordinal: i16,
    proposal_id: Uuid,
    proposal_revision: i64,
    proposal_payload_hash: Vec<u8>,
}

async fn preview_member_evidence(
    pool: &PgPool,
    scope: DatabaseScope,
    preview_id: Uuid,
) -> Vec<PreviewMemberEvidence> {
    sqlx::query_as(
        "SELECT workspace_id,user_id,preview_id,ordinal,proposal_id,proposal_revision, \
         proposal_payload_hash FROM proposal_apply_preview_members \
         WHERE workspace_id=$1 AND user_id=$2 AND preview_id=$3 ORDER BY ordinal",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn assert_sqlstate(error: &sqlx::Error, expected: &str) {
    let actual = error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    assert_eq!(actual.as_deref(), Some(expected));
}

async fn wait_for_postgres_blocker(pool: &PgPool, blocker_pid: i32) {
    tokio::time::timeout(StdDuration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity AS activity \
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
    .expect("competing PostgreSQL query reached the intended lock");
}

fn item(
    id: Uuid,
    kind: ItemKind,
    title: &str,
    is_sensitive: bool,
    parent_id: Option<Uuid>,
) -> NewItem {
    NewItem {
        id,
        is_sensitive,
        kind,
        status: ItemStatus::Inbox,
        title: title.to_owned(),
        notes: None,
        timezone_name: "Europe/Madrid".to_owned(),
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
        parent_id,
        sibling_order: 0,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    }
}

async fn serialized_create_delta_payload_bytes(
    pool: &PgPool,
    items: &[NewItem],
    now: DateTime<Utc>,
) -> i64 {
    let payloads = items
        .iter()
        .cloned()
        .map(|item| {
            serde_json::to_value(Item::new(item, now).expect("valid near-limit item"))
                .expect("serialize near-limit item")
        })
        .collect();
    sqlx::query_scalar(
        "SELECT COALESCE(sum(octet_length(element::text)), 0)::bigint \
         FROM jsonb_array_elements($1::jsonb) AS elements(element)",
    )
    .bind(Value::Array(payloads))
    .fetch_one(pool)
    .await
    .expect("measure PostgreSQL delta payload bytes")
}

fn dependency_metadata(predecessor_id: Uuid) -> Value {
    json!({
        "constraints": {
            "dependencies": [{
                "item_id": predecessor_id,
                "relation": "finish_to_start",
                "minimum_lag": 15,
                "strength": { "level": "hard" }
            }]
        }
    })
}

async fn seed_scope(pool: &PgPool) -> DatabaseScope {
    let scope = DatabaseScope {
        user_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
    };
    sqlx::query(
        "INSERT INTO users (id,auth_subject,display_name,timezone_name) \
         VALUES ($1,'proposal-application-owner','Owner','Europe/Madrid')",
    )
    .bind(scope.user_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspaces (id,owner_user_id,slug,name,timezone_name) \
         VALUES ($1,$2,$3,'Personal','Europe/Madrid')",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(format!("proposal-{}", scope.workspace_id.simple()))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_members (workspace_id,user_id,role) VALUES ($1,$2,'owner')")
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
        let schema = format!("dayweave_proposal_apply_test_{}", Uuid::new_v4().simple());
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
