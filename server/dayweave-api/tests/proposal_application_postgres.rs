use std::{
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    items::{
        IdempotencyKey, Item, ItemKind, ItemService, ItemStatus, NewItem, ReplaceItem, SplitPolicy,
    },
    persistence::{
        DatabaseScope, MIGRATOR, PostgresItemRepository, PostgresProposalApplicationRepository,
        PostgresProposalRepository, ProposalApplicationError,
    },
    proposals::{
        Clock, NewProposal, Proposal, ProposalApplicationStatus, ProposalApplyRequest,
        ProposalChangeSet, ProposalCommand, ProposalConflictCode, ProposalImplicitChangeReason,
        ProposalItemField, ProposalKind, ProposalPreviewMember, ProposalPreviewRequest,
        ProposalRepository, ProposalSource, ProposalStatus, ProposalUndoRequest, SystemClock,
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
    let commands = vec![
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: item(parent_id, ItemKind::Goal, "Private launch", true, None),
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
    assert_eq!(preview.command_ids.len(), 2);
    assert_eq!(preview.diffs.len(), 2);
    assert!(preview.requires_explicit_approval);
    let parent_preview = preview
        .diffs
        .iter()
        .find(|diff| diff.item_id == parent_id)
        .and_then(|diff| diff.after.as_ref())
        .expect("parent has a final preview snapshot");
    assert_eq!(parent_preview.revision, 2);
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
    assert_eq!(first.application.affected_item_ids.len(), 2);
    assert_eq!(first.application.command_ids.len(), 2);
    assert_eq!(
        applications
            .get_for_proposal(proposal.id)
            .await
            .expect("application is discoverable after a lost response"),
        first.application
    );

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
        parent_revision, 2,
        "child creation refreshes its parent fence"
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
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM items WHERE workspace_id=$1 AND id = ANY($2) AND trashed_at IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(vec![parent_id, child_id])
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
    let parent = fixture
        .items
        .create(
            item(
                Uuid::new_v4(),
                ItemKind::Goal,
                "Existing parent",
                false,
                None,
            ),
            item_key("implicit-parent-create", 41),
        )
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

fn replacement(item: &Item, status: ItemStatus) -> ReplaceItem {
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
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
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
        duration_seconds: Some(1_800),
        deadline_at: None,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: json!({}),
        split_policy: SplitPolicy::Indivisible,
        importance: 70,
        urgency: 40,
        parent_id,
        sibling_order: 0,
    }
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
