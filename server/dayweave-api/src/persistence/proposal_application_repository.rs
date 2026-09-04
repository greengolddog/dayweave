use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    items::{DeadlineKind, DeadlineStrength, Item, ItemRepositoryError, validate_dependency_graph},
    proposals::{
        Clock, DecisionKind, MAX_PROPOSAL_COMMANDS, MAX_PROPOSALS_PER_PREVIEW, Proposal,
        ProposalApplicationReceipt, ProposalApplicationStatus, ProposalAppliedMember,
        ProposalApplyRequest, ProposalApplyResponse, ProposalChangeSet, ProposalChangeSetPreview,
        ProposalChangeSetSchema, ProposalCommand, ProposalConflict, ProposalConflictCode,
        ProposalImplicitChangeReason, ProposalImplicitItemDiff, ProposalItemDiff,
        ProposalItemField, ProposalKind, ProposalOperation, ProposalPreviewRequest, ProposalRisk,
        ProposalRiskCode, ProposalRiskLevel, ProposalStatus, ProposalUndoRequest,
        ProposalUndoResponse,
    },
};

use super::{
    DatabaseScope, TransactionalGraphMode, TransactionalItemCommand, TransactionalItemEffect,
    apply_item_command_tx, clear_dependency_edges_tx, fetch_item_batch_tx, list_item_batch_tx,
    lock_execution_item_batch_tx, lock_item_batch_tx, proposal_from_row, stage_item_create_tx,
    staged_item_shell, start_item_change_group_tx, validate_dependency_graph_batch_tx,
    validate_item_change_group_tx, validate_preview_item_change_group_tx,
};

const PREVIEW_TTL: StdDuration = StdDuration::from_mins(15);
const UNDO_TTL: StdDuration = StdDuration::from_hours(24);
const MAINTENANCE_INTERVAL: StdDuration = StdDuration::from_hours(1);
const MAX_REVIEW_HASH_LENGTH: usize = 80;
const MAX_ACTIVE_PREVIEWS: i64 = 100;

const LOCKED_PROPOSAL_SELECT: &str = "SELECT id, revision, submitted_by_subject, source, \
    source_reference, kind, status, title, explanation, payload, decision_note, created_at, \
    updated_at, expires_at, decided_at FROM proposals WHERE workspace_id=$1 AND id=$2 \
    AND trashed_at IS NULL FOR UPDATE";

#[derive(Clone)]
pub struct PostgresProposalApplicationRepository {
    pool: PgPool,
    scope: DatabaseScope,
    test_clock: Option<Arc<dyn Clock>>,
}

impl std::fmt::Debug for PostgresProposalApplicationRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresProposalApplicationRepository")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl PostgresProposalApplicationRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self {
            pool,
            scope,
            test_clock: None,
        }
    }

    /// Installs a deterministic time source for integration tests that need to
    /// exercise long retention windows without sleeping. Production callers
    /// must use [`Self::new`] so PostgreSQL remains the single clock authority.
    #[doc(hidden)]
    #[must_use]
    pub fn new_with_test_clock(pool: PgPool, scope: DatabaseScope, clock: Arc<dyn Clock>) -> Self {
        Self {
            pool,
            scope,
            test_clock: Some(clock),
        }
    }

    #[must_use]
    pub const fn scope(&self) -> DatabaseScope {
        self.scope
    }

    pub(crate) const fn uses_test_clock(&self) -> bool {
        self.test_clock.is_some()
    }

    /// Scrubs expired undo snapshots and prunes expired unapplied previews.
    ///
    /// # Errors
    ///
    /// Returns a storage or owner-scope error. The transaction is all-or-none.
    pub async fn maintain_retention(&self) -> Result<(), ProposalApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_item_batch_tx(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(map_item_error)?;
        lock_owner(&mut transaction, self.scope).await?;
        let now = self.authoritative_now(&mut transaction).await?;
        scrub_expired_effect_snapshots(&mut transaction, self.scope, now).await?;
        prune_expired_previews(&mut transaction, self.scope, now).await?;
        transaction.commit().await.map_err(internal)
    }

    pub(crate) fn spawn_maintenance_worker(self: &Arc<Self>) {
        let repository = Arc::clone(self);
        tokio::spawn(async move {
            let interval = tokio::time::Instant::now() + MAINTENANCE_INTERVAL;
            let mut interval = tokio::time::interval_at(interval, MAINTENANCE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = repository.maintain_retention().await {
                    tracing::warn!(%error, "proposal retention maintenance failed");
                }
            }
        });
    }

    /// Simulates a complete proposal group with ordinary item rules, rolls the
    /// canonical transaction back, and stores an immutable content-bound
    /// preview. Generic or legacy proposal payloads never cross this boundary.
    ///
    /// # Errors
    ///
    /// Returns a validation, stale-state, owner-scope, or storage error.
    #[allow(clippy::too_many_lines)] // Keeps simulation, review hashing, and persistence in one visible lock boundary.
    pub async fn preview(
        &self,
        request: ProposalPreviewRequest,
    ) -> Result<ProposalChangeSetPreview, ProposalApplicationError> {
        validate_preview_request(&request)?;
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_execution_item_batch_tx(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(map_item_error)?;
        lock_owner(&mut transaction, self.scope).await?;
        let maintenance_now = self.authoritative_now(&mut transaction).await?;
        scrub_expired_effect_snapshots(&mut transaction, self.scope, maintenance_now).await?;
        prune_and_limit_previews(&mut transaction, self.scope, maintenance_now).await?;
        let canonical_hash = canonical_item_hash(&mut transaction, self.scope.workspace_id).await?;

        let proposals = lock_requested_proposals(&mut transaction, self.scope, &request).await?;
        let now = self.authoritative_now(&mut transaction).await?;
        let prepared = prepare_change_set(&proposals, now)?;
        let before_items = list_item_batch_tx(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(map_item_error)?;
        sqlx::query("SAVEPOINT dayweave_proposal_preview_simulation")
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        // Recording inside the savepoint produces the exact payloads that apply
        // would publish, including every intermediate parent refresh. The
        // records, audits, and outbox rows are rolled back after both apply and
        // undo delivery bounds have been checked.
        let apply_group_id = start_item_change_group_tx(&mut transaction)
            .await
            .map_err(map_item_error)?;
        let simulated =
            simulate_commands(&mut transaction, self.scope, &prepared.commands, now, true).await;
        let (effects, implicit_diffs, mut conflicts) = match simulated {
            Ok(mut effects) => {
                let after_items = list_item_batch_tx(&mut transaction, self.scope.workspace_id)
                    .await
                    .map_err(map_item_error)?;
                let initial_by_id = before_items
                    .iter()
                    .map(|item| (item.id, item))
                    .collect::<HashMap<_, _>>();
                for effect in &mut effects {
                    effect.before = initial_by_id
                        .get(&effect.after.id)
                        .map(|item| (*item).clone());
                }
                let implicit_diffs =
                    implicit_item_diffs(&before_items, &after_items, &prepared.commands);
                let affected_item_ids =
                    affected_preview_item_ids(&prepared.commands, &effects, &implicit_diffs);
                let conflicts = provider_managed_conflicts(
                    &mut transaction,
                    self.scope,
                    &prepared.commands,
                    &affected_item_ids,
                )
                .await?;
                let mut conflicts = conflicts;
                if let Err(error) = validate_preview_item_change_group_tx(
                    &mut transaction,
                    self.scope.workspace_id,
                    apply_group_id,
                )
                .await
                {
                    conflicts.push(item_conflict(&prepared.commands[0], &error));
                }

                let undo_group_id = start_item_change_group_tx(&mut transaction)
                    .await
                    .map_err(map_item_error)?;
                match simulate_preview_undo_group(
                    &mut transaction,
                    self.scope,
                    &prepared.commands,
                    &effects,
                    now,
                )
                .await
                {
                    Ok(()) => {
                        if let Err(error) = validate_preview_item_change_group_tx(
                            &mut transaction,
                            self.scope.workspace_id,
                            undo_group_id,
                        )
                        .await
                        {
                            conflicts.push(item_conflict(&prepared.commands[0], &error));
                        }
                    }
                    Err(conflict) => conflicts.push(conflict),
                }
                (effects, implicit_diffs, conflicts)
            }
            Err(conflict) => (Vec::new(), Vec::new(), vec![conflict]),
        };
        sqlx::query("ROLLBACK TO SAVEPOINT dayweave_proposal_preview_simulation")
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        sqlx::query("RELEASE SAVEPOINT dayweave_proposal_preview_simulation")
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;

        let diffs = effects
            .iter()
            .zip(&prepared.commands)
            .map(|(effect, command)| item_diff(command, effect))
            .collect::<Vec<_>>();
        let risks = proposal_risks(&prepared.commands, &effects);
        let maximum_risk = risks
            .iter()
            .map(|risk| risk.level)
            .max()
            .unwrap_or(ProposalRiskLevel::Low);
        let requires_explicit_approval = risks.iter().any(|risk| risk.requires_explicit_approval);

        let preview_id = Uuid::new_v4();
        let preview_ttl = Duration::from_std(PREVIEW_TTL).map_err(|_| internal(()))?;
        let proposal_expiry = proposals
            .iter()
            .map(|proposal| proposal.expires_at)
            .min()
            .ok_or_else(|| validation("at least one proposal is required"))?;
        let expires_at = (now + preview_ttl).min(proposal_expiry);
        if expires_at <= now {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::ProposalExpired,
            ));
        }

        let commands_hash = hash_json(&prepared.commands)?;
        let members = proposals
            .iter()
            .map(|proposal| StoredPreviewMember {
                proposal_id: proposal.id,
                proposal_revision: proposal.revision,
                payload_hash: hash_json(&proposal.payload).unwrap_or([0; 32]),
            })
            .collect::<Vec<_>>();
        if members.iter().any(|member| member.payload_hash == [0; 32]) {
            return Err(ProposalApplicationError::Internal);
        }
        conflicts.sort_by_key(|conflict| (conflict.item_id, conflict.command_id));
        let can_apply = conflicts.is_empty();
        let review_content_hash = hash_domain_json(
            b"dayweave.proposal.review-content.v1\0",
            &json!({
                "change_set_schema": ProposalChangeSetSchema::V1,
                "command_ids": prepared.commands.iter().map(ProposalCommand::command_id).collect::<Vec<_>>(),
                "can_apply": can_apply,
                "maximum_risk": maximum_risk,
                "requires_explicit_approval": requires_explicit_approval,
                "diffs": &diffs,
                "implicit_diffs": &implicit_diffs,
                "risks": &risks,
                "conflicts": &conflicts,
            }),
        )?;
        let preview_hash = calculate_preview_hash(
            preview_id,
            self.scope,
            &members,
            prepared.commands.len(),
            commands_hash,
            canonical_hash,
            review_content_hash,
            can_apply,
            now,
            expires_at,
        );

        persist_preview(
            &mut transaction,
            self.scope,
            preview_id,
            &members,
            prepared.commands.len(),
            commands_hash,
            canonical_hash,
            review_content_hash,
            preview_hash,
            can_apply,
            now,
            expires_at,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(map_preview_insert_error)?;

        Ok(ProposalChangeSetPreview {
            preview_id,
            proposals: request.proposals,
            change_set_schema: ProposalChangeSetSchema::V1,
            command_ids: prepared
                .commands
                .iter()
                .map(ProposalCommand::command_id)
                .collect(),
            review_hash: encoded_hash(preview_hash),
            expires_at,
            can_apply,
            maximum_risk,
            requires_explicit_approval,
            diffs,
            implicit_diffs,
            risks,
            conflicts,
        })
    }

    /// Applies one exact live preview and records its inverse evidence.
    ///
    /// # Errors
    ///
    /// Returns a validation, conflict, idempotency, owner-scope, or storage error.
    #[allow(clippy::too_many_lines)] // The transaction is intentionally visible as one ordered safety boundary.
    pub async fn apply(
        &self,
        preview_id: Uuid,
        request: ProposalApplyRequest,
        idempotency_key: &str,
        actor_session_id: Option<Uuid>,
    ) -> Result<ProposalApplyResponse, ProposalApplicationError> {
        validate_idempotency_key(idempotency_key)?;
        let expected_hash = decode_hash(&request.expected_review_hash)?;
        let mut request_evidence = Vec::with_capacity(96);
        request_evidence.extend_from_slice(b"dayweave.proposal.apply.v1\0");
        request_evidence.extend_from_slice(preview_id.as_bytes());
        request_evidence.extend_from_slice(&expected_hash);
        let request_hash = hash_bytes(&request_evidence);
        let key_hash = hash_bytes(idempotency_key.as_bytes());
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_execution_item_batch_tx(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(map_item_error)?;
        lock_owner(&mut transaction, self.scope).await?;

        if let Some(application_id) = replay_request(
            &mut transaction,
            self.scope,
            "apply",
            key_hash,
            request_hash,
        )
        .await?
        {
            let application = load_receipt_tx(&mut transaction, self.scope, application_id).await?;
            transaction.commit().await.map_err(internal)?;
            return Ok(ProposalApplyResponse {
                application,
                replayed: true,
            });
        }

        let preview = lock_preview(&mut transaction, self.scope, preview_id).await?;
        if preview.preview_hash != expected_hash {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::PreviewMismatch,
            ));
        }
        if !preview.can_apply {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::PreviewNotApplicable,
            ));
        }
        let members = load_preview_members(&mut transaction, self.scope, preview_id).await?;
        let proposals = lock_preview_proposals(&mut transaction, self.scope, &members).await?;
        let now = self.authoritative_now(&mut transaction).await?;
        if preview.expires_at <= now {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::PreviewExpired,
            ));
        }
        let commands = prepare_change_set(&proposals, now)?.commands;
        validate_stored_preview(&preview, self.scope, &members, &proposals, &commands)?;
        if canonical_item_hash(&mut transaction, self.scope.workspace_id).await?
            != preview.canonical_hash
        {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::PreviewMismatch,
            ));
        }
        let watermark = item_change_watermark(&mut transaction, self.scope.workspace_id).await?;
        let change_group_id = start_item_change_group_tx(&mut transaction)
            .await
            .map_err(map_item_error)?;
        let effects = execute_commands(&mut transaction, self.scope, &commands, now, true).await?;
        validate_item_change_group_tx(&mut transaction, self.scope.workspace_id, change_group_id)
            .await
            .map_err(map_item_error)?;
        let affected_item_ids =
            changed_item_ids_since(&mut transaction, self.scope.workspace_id, watermark).await?;
        reject_provider_managed_items(&mut transaction, self.scope, &affected_item_ids).await?;
        let final_items = fetch_items(
            &mut transaction,
            self.scope.workspace_id,
            &affected_item_ids,
        )
        .await?;

        let application_id = Uuid::new_v4();
        let undo_ttl = Duration::from_std(UNDO_TTL).map_err(|_| internal(()))?;
        let undo_expires_at = now + undo_ttl;
        let apply_audit_id = Uuid::new_v4();
        insert_application_audit(
            &mut transaction,
            self.scope,
            actor_session_id,
            apply_audit_id,
            application_id,
            "proposal.application.applied",
            None,
            Some(1),
            commands.len(),
            affected_item_ids.len(),
        )
        .await?;
        insert_application_header(
            &mut transaction,
            self.scope,
            application_id,
            &preview,
            apply_audit_id,
            commands.len(),
            affected_item_ids.len(),
            now,
            undo_expires_at,
        )
        .await?;
        insert_application_members(&mut transaction, self.scope, application_id, &members).await?;
        insert_effects(
            &mut transaction,
            self.scope,
            application_id,
            &commands,
            &effects,
            &final_items,
            now,
        )
        .await?;
        insert_fences(&mut transaction, self.scope, application_id, &final_items).await?;
        accept_proposals(
            &mut transaction,
            self.scope,
            &proposals,
            actor_session_id,
            now,
        )
        .await?;
        insert_request_receipt(
            &mut transaction,
            self.scope,
            "apply",
            key_hash,
            request_hash,
            application_id,
            now,
        )
        .await?;
        insert_application_outbox(
            &mut transaction,
            self.scope,
            application_id,
            1,
            "proposal.application.applied",
        )
        .await?;
        let application = load_receipt_tx(&mut transaction, self.scope, application_id).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ProposalApplyResponse {
            application,
            replayed: false,
        })
    }

    /// Loads a content-free durable application receipt.
    ///
    /// # Errors
    ///
    /// Returns `NotFound`, owner-scope, or storage errors.
    pub async fn get(
        &self,
        application_id: Uuid,
    ) -> Result<ProposalApplicationReceipt, ProposalApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_owner(&mut transaction, self.scope).await?;
        lock_receipt_application(&mut transaction, self.scope, application_id).await?;
        let receipt = load_receipt_tx(&mut transaction, self.scope, application_id).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(receipt)
    }

    /// Finds the durable application linked to one accepted proposal. This is
    /// the cross-device reconciliation path after a lost apply response.
    ///
    /// # Errors
    ///
    /// Returns `NotFound`, owner-scope, or storage errors.
    pub async fn get_for_proposal(
        &self,
        proposal_id: Uuid,
    ) -> Result<ProposalApplicationReceipt, ProposalApplicationError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_owner(&mut transaction, self.scope).await?;
        let application_id: Uuid = sqlx::query_scalar(
            "SELECT application.id FROM proposal_applications AS application \
             JOIN proposal_application_members AS member \
               ON member.workspace_id=application.workspace_id \
              AND member.user_id=application.user_id \
              AND member.application_id=application.id \
             WHERE application.workspace_id=$1 AND application.user_id=$2 \
               AND member.proposal_id=$3 FOR SHARE OF application",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(proposal_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?
        .ok_or(ProposalApplicationError::NotFound)?;
        let receipt = load_receipt_tx(&mut transaction, self.scope, application_id).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(receipt)
    }

    /// Reverses a still-fenced application as one canonical transaction.
    ///
    /// # Errors
    ///
    /// Returns a validation, stale-state, idempotency, owner-scope, or storage error.
    #[allow(clippy::too_many_lines)] // The inverse lock/fence/commit ordering is intentionally kept together.
    pub async fn undo(
        &self,
        application_id: Uuid,
        request: ProposalUndoRequest,
        idempotency_key: &str,
        actor_session_id: Option<Uuid>,
    ) -> Result<ProposalUndoResponse, ProposalApplicationError> {
        validate_idempotency_key(idempotency_key)?;
        if request.expected_application_revision == 0 {
            return Err(validation("expected_application_revision must be positive"));
        }
        let request_hash = hash_bytes(
            format!(
                "dayweave.proposal.undo.v1:{application_id}:{}",
                request.expected_application_revision
            )
            .as_bytes(),
        );
        let key_hash = hash_bytes(idempotency_key.as_bytes());
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_execution_item_batch_tx(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(map_item_error)?;
        lock_owner(&mut transaction, self.scope).await?;

        if let Some(replayed_id) =
            replay_request(&mut transaction, self.scope, "undo", key_hash, request_hash).await?
        {
            let application = load_receipt_tx(&mut transaction, self.scope, replayed_id).await?;
            transaction.commit().await.map_err(internal)?;
            return Ok(ProposalUndoResponse {
                application,
                replayed: true,
            });
        }

        let application = lock_application(&mut transaction, self.scope, application_id).await?;
        let now = self.authoritative_now(&mut transaction).await?;
        if application.status != ProposalApplicationStatus::Applied {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::AlreadyApplied,
            ));
        }
        if application.revision != request.expected_application_revision {
            return Err(ProposalApplicationError::RevisionConflict {
                expected: request.expected_application_revision,
                actual: application.revision,
            });
        }
        if application.undo_expires_at <= now {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::UndoExpired,
            ));
        }
        let fences = lock_fences(&mut transaction, self.scope, application_id).await?;
        validate_fences(&mut transaction, self.scope, &fences).await?;
        reject_provider_managed_items(
            &mut transaction,
            self.scope,
            &fences.iter().map(|fence| fence.item_id).collect::<Vec<_>>(),
        )
        .await?;
        let effects = load_effects_reverse(&mut transaction, self.scope, application_id).await?;
        let watermark = item_change_watermark(&mut transaction, self.scope.workspace_id).await?;
        let change_group_id = start_item_change_group_tx(&mut transaction)
            .await
            .map_err(map_item_error)?;
        let replaced_dependency_targets = effects
            .iter()
            .filter_map(|effect| match effect.operation {
                ProposalOperation::CreateItem => None,
                ProposalOperation::ReplaceItem
                | ProposalOperation::TrashItem
                | ProposalOperation::RestoreItem => Some(effect.item_id),
            })
            .collect::<Vec<_>>();
        clear_dependency_edges_tx(
            &mut transaction,
            self.scope.workspace_id,
            &replaced_dependency_targets,
        )
        .await
        .map_err(map_item_error)?;
        let undo_graph_mode = if effects.len() == 1 {
            TransactionalGraphMode::Immediate
        } else {
            TransactionalGraphMode::Deferred
        };
        for effect in effects {
            let current = fetch_item_batch_tx(
                &mut transaction,
                self.scope.workspace_id,
                effect.item_id,
                true,
            )
            .await
            .map_err(map_item_error)?;
            let inverse = inverse_command(&effect, &current)?;
            apply_item_command_tx(
                &mut transaction,
                self.scope,
                inverse,
                now,
                true,
                undo_graph_mode,
            )
            .await
            .map_err(map_item_error)?;
        }
        validate_dependency_graph_batch_tx(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(map_item_error)?;
        validate_item_change_group_tx(&mut transaction, self.scope.workspace_id, change_group_id)
            .await
            .map_err(map_item_error)?;
        let undo_item_ids =
            changed_item_ids_since(&mut transaction, self.scope.workspace_id, watermark).await?;
        let undo_items =
            fetch_items(&mut transaction, self.scope.workspace_id, &undo_item_ids).await?;
        let undo_audit_id = Uuid::new_v4();
        insert_application_audit(
            &mut transaction,
            self.scope,
            actor_session_id,
            undo_audit_id,
            application_id,
            "proposal.application.undone",
            Some(1),
            Some(2),
            application.effect_count,
            application.fence_count,
        )
        .await?;
        update_fences_after_undo(
            &mut transaction,
            self.scope,
            application_id,
            &fences,
            &undo_items,
        )
        .await?;
        mark_application_undone(
            &mut transaction,
            self.scope,
            application_id,
            undo_audit_id,
            now,
        )
        .await?;
        insert_request_receipt(
            &mut transaction,
            self.scope,
            "undo",
            key_hash,
            request_hash,
            application_id,
            now,
        )
        .await?;
        insert_application_outbox(
            &mut transaction,
            self.scope,
            application_id,
            2,
            "proposal.application.undone",
        )
        .await?;
        let application = load_receipt_tx(&mut transaction, self.scope, application_id).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(ProposalUndoResponse {
            application,
            replayed: false,
        })
    }

    async fn authoritative_now(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<DateTime<Utc>, ProposalApplicationError> {
        if let Some(clock) = self.test_clock.as_ref() {
            return Ok(clock.now());
        }
        sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut **transaction)
            .await
            .map_err(internal)
    }
}

#[derive(Debug, Error)]
pub enum ProposalApplicationError {
    #[error("proposal application input is invalid: {0}")]
    Validation(String),
    #[error("proposal application resource was not found")]
    NotFound,
    #[error("proposal application owner scope is unavailable")]
    OwnerUnavailable,
    #[error("proposal application state is stale: {0:?}")]
    Stale(ProposalConflictCode),
    #[error("revision conflict: expected {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("idempotency key was used for different content")]
    IdempotencyConflict,
    #[error("proposal application failed")]
    Internal,
}

struct PreparedChangeSet {
    commands: Vec<ProposalCommand>,
}

#[derive(Clone)]
struct StoredPreviewMember {
    proposal_id: Uuid,
    proposal_revision: u64,
    payload_hash: [u8; 32],
}

struct StoredPreview {
    id: Uuid,
    proposal_count: usize,
    command_count: usize,
    commands_hash: [u8; 32],
    canonical_hash: [u8; 32],
    review_content_hash: [u8; 32],
    preview_hash: [u8; 32],
    can_apply: bool,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

struct StoredApplication {
    revision: u64,
    status: ProposalApplicationStatus,
    effect_count: usize,
    fence_count: usize,
    undo_expires_at: DateTime<Utc>,
}

struct StoredEffect {
    operation: ProposalOperation,
    item_id: Uuid,
    before: Option<Item>,
}

struct StoredFence {
    item_id: Uuid,
    applied_revision: u64,
    applied_deleted: bool,
}

fn validate_preview_request(
    request: &ProposalPreviewRequest,
) -> Result<(), ProposalApplicationError> {
    if request.proposals.is_empty() || request.proposals.len() > MAX_PROPOSALS_PER_PREVIEW {
        return Err(validation(format!(
            "proposals must contain between 1 and {MAX_PROPOSALS_PER_PREVIEW} members"
        )));
    }
    let mut ids = HashSet::with_capacity(request.proposals.len());
    for member in &request.proposals {
        if member.expected_revision == 0 {
            return Err(validation("expected_revision must be positive"));
        }
        if !ids.insert(member.proposal_id) {
            return Err(validation("proposal ids must be unique"));
        }
    }
    Ok(())
}

async fn lock_owner(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), ProposalApplicationError> {
    let active: Option<bool> = sqlx::query_scalar(
        "SELECT role = 'owner' AND removed_at IS NULL FROM workspace_members \
         WHERE workspace_id = $1 AND user_id = $2 FOR NO KEY UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    if active == Some(true) {
        Ok(())
    } else {
        Err(ProposalApplicationError::OwnerUnavailable)
    }
}

async fn lock_requested_proposals(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    request: &ProposalPreviewRequest,
) -> Result<Vec<Proposal>, ProposalApplicationError> {
    let mut sorted = request.proposals.clone();
    sorted.sort_by_key(|member| member.proposal_id);
    let mut by_id = std::collections::HashMap::with_capacity(sorted.len());
    for member in sorted {
        let row = sqlx::query(LOCKED_PROPOSAL_SELECT)
            .bind(scope.workspace_id)
            .bind(member.proposal_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(internal)?
            .ok_or(ProposalApplicationError::NotFound)?;
        let proposal = proposal_from_row(&row).map_err(|_| ProposalApplicationError::Internal)?;
        if proposal.revision != member.expected_revision {
            return Err(ProposalApplicationError::RevisionConflict {
                expected: member.expected_revision,
                actual: proposal.revision,
            });
        }
        by_id.insert(proposal.id, proposal);
    }
    request
        .proposals
        .iter()
        .map(|member| {
            by_id
                .remove(&member.proposal_id)
                .ok_or(ProposalApplicationError::Internal)
        })
        .collect()
}

fn prepare_change_set(
    proposals: &[Proposal],
    now: DateTime<Utc>,
) -> Result<PreparedChangeSet, ProposalApplicationError> {
    let mut commands = Vec::new();
    for proposal in proposals {
        if proposal.status != ProposalStatus::Pending {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::ProposalNotPending,
            ));
        }
        if proposal.expires_at <= now {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::ProposalExpired,
            ));
        }
        let change_set = ProposalChangeSet::from_payload(&proposal.payload)
            .map_err(|error| validation(error.to_string()))?;
        validate_kind_contract(proposal.kind, &change_set.commands)?;
        commands.extend(change_set.commands);
    }
    let combined =
        ProposalChangeSet::new(commands).map_err(|error| validation(error.to_string()))?;
    if combined.commands.len() > MAX_PROPOSAL_COMMANDS {
        return Err(validation("proposal group contains too many commands"));
    }
    Ok(PreparedChangeSet {
        commands: combined.commands,
    })
}

fn validate_kind_contract(
    kind: ProposalKind,
    commands: &[ProposalCommand],
) -> Result<(), ProposalApplicationError> {
    let valid = match kind {
        ProposalKind::CreateItem => {
            commands.len() == 1
                && matches!(commands.first(), Some(ProposalCommand::CreateItem { .. }))
        }
        ProposalKind::GoalBreakdown => commands
            .iter()
            .all(|command| matches!(command, ProposalCommand::CreateItem { .. })),
        ProposalKind::CalendarEvent => commands.iter().all(|command| {
            matches!(command, ProposalCommand::CreateItem { item, .. } if item.kind == crate::items::ItemKind::Event)
        }),
        ProposalKind::UpdateItem | ProposalKind::ConstraintChange => commands.iter().all(
            |command| {
                matches!(
                    command,
                    ProposalCommand::ReplaceItem { .. }
                        | ProposalCommand::TrashItem { .. }
                        | ProposalCommand::RestoreItem { .. }
                )
            },
        ),
        ProposalKind::SchedulePlan | ProposalKind::Recommendation => false,
    };
    if valid {
        Ok(())
    } else {
        Err(validation(
            "proposal kind is not compatible with its typed item commands",
        ))
    }
}

async fn simulate_commands(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    commands: &[ProposalCommand],
    now: DateTime<Utc>,
    record: bool,
) -> Result<Vec<TransactionalItemEffect>, ProposalConflict> {
    let initial_items = list_item_batch_tx(transaction, scope.workspace_id)
        .await
        .map_err(|error| item_conflict(&commands[0], &error))?;
    let initial_by_id = initial_items
        .into_iter()
        .map(|item| (item.id, item))
        .collect::<HashMap<_, _>>();
    validate_initial_command_fences(commands, &initial_by_id)
        .map_err(|(index, error)| item_conflict(&commands[index], &error))?;
    let execution_order = command_execution_order(commands, &initial_by_id, now)
        .map_err(|(index, error)| item_conflict(&commands[index], &error))?;

    // Stage every new identity before authoring any edge or blocked-by foreign
    // key. These rows remain transaction-local shells until their command is
    // finalized and are never visible without the matching item mutation.
    for command in commands {
        if let ProposalCommand::CreateItem { item, .. } = command {
            stage_item_create_tx(transaction, scope, item, now)
                .await
                .map_err(|error| item_conflict(command, &error))?;
        }
    }
    let replaced_dependency_targets = commands
        .iter()
        .filter_map(|command| match command {
            ProposalCommand::ReplaceItem { item_id, .. } => Some(*item_id),
            ProposalCommand::CreateItem { .. }
            | ProposalCommand::TrashItem { .. }
            | ProposalCommand::RestoreItem { .. } => None,
        })
        .collect::<Vec<_>>();
    clear_dependency_edges_tx(
        transaction,
        scope.workspace_id,
        &replaced_dependency_targets,
    )
    .await
    .map_err(|error| item_conflict(&commands[0], &error))?;

    let mut indexed_effects = Vec::with_capacity(commands.len());
    let graph_mode =
        if commands.len() == 1 && !matches!(commands[0], ProposalCommand::CreateItem { .. }) {
            TransactionalGraphMode::Immediate
        } else {
            TransactionalGraphMode::DeferredWithStagedCreates
        };
    for (execution_ordinal, index) in execution_order.into_iter().enumerate() {
        let command = &commands[index];
        let transactional =
            transactional_command_at_current_revision(transaction, scope.workspace_id, command)
                .await
                .map_err(|error| item_conflict(command, &error))?;
        match apply_item_command_tx(transaction, scope, transactional, now, record, graph_mode)
            .await
        {
            Ok(mut effect) => {
                effect.before = initial_by_id.get(&effect.after.id).cloned();
                effect.execution_ordinal = execution_ordinal;
                indexed_effects.push((index, effect));
            }
            Err(error) => return Err(item_conflict(command, &error)),
        }
    }
    validate_dependency_graph_batch_tx(transaction, scope.workspace_id)
        .await
        .map_err(|error| {
            let command = commands
                .iter()
                .find(|command| matches!(command, ProposalCommand::ReplaceItem { .. }))
                .unwrap_or(&commands[0]);
            item_conflict(command, &error)
        })?;
    indexed_effects.sort_by_key(|(index, _)| *index);
    let mut effects = indexed_effects
        .into_iter()
        .map(|(_, effect)| effect)
        .collect::<Vec<_>>();
    // A later hierarchy command can advance an earlier command target (for
    // example, creating a child refreshes its newly created goal). Review must
    // show the final post-batch state, exactly like durable effect evidence.
    for effect in &mut effects {
        effect.after =
            match fetch_item_batch_tx(transaction, scope.workspace_id, effect.after.id, true).await
            {
                Ok(item) => item,
                Err(error) => {
                    let command = commands
                        .iter()
                        .find(|command| command.target_item_id() == effect.after.id)
                        .expect("each effect has one validated command target");
                    return Err(item_conflict(command, &error));
                }
            };
    }
    Ok(effects)
}

fn validate_initial_command_fences(
    commands: &[ProposalCommand],
    initial_by_id: &HashMap<Uuid, Item>,
) -> Result<(), (usize, ItemRepositoryError)> {
    for (index, command) in commands.iter().enumerate() {
        let (item_id, expected_revision, must_be_deleted) = match command {
            ProposalCommand::CreateItem { item, .. } => {
                if initial_by_id.contains_key(&item.id) {
                    return Err((index, ItemRepositoryError::Duplicate(item.id)));
                }
                continue;
            }
            ProposalCommand::ReplaceItem {
                item_id,
                expected_revision,
                ..
            }
            | ProposalCommand::TrashItem {
                item_id,
                expected_revision,
                ..
            } => (*item_id, *expected_revision, false),
            ProposalCommand::RestoreItem {
                item_id,
                expected_revision,
                ..
            } => (*item_id, *expected_revision, true),
        };
        let Some(item) = initial_by_id
            .get(&item_id)
            .filter(|item| item.deleted_at.is_some() == must_be_deleted)
        else {
            return Err((index, ItemRepositoryError::NotFound(item_id)));
        };
        if item.revision != expected_revision {
            return Err((
                index,
                ItemRepositoryError::RevisionConflict {
                    expected: expected_revision,
                    actual: item.revision,
                },
            ));
        }
    }
    Ok(())
}

/// Replays the exact inverse that a future undo would publish while the preview
/// savepoint still contains the simulated applied state. This makes the undo
/// delivery-size guarantee part of review instead of discovering an
/// undrainable atomic group after approval.
async fn simulate_preview_undo_group(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    commands: &[ProposalCommand],
    effects: &[TransactionalItemEffect],
    now: DateTime<Utc>,
) -> Result<(), ProposalConflict> {
    let Some(first_command) = commands.first() else {
        return Err(ProposalConflict {
            code: ProposalConflictCode::InvalidItem,
            command_id: None,
            item_id: None,
            expected_revision: None,
            actual_revision: None,
            summary: "The proposal contains no item commands.".to_owned(),
        });
    };
    if commands.len() != effects.len() {
        return Err(item_conflict(first_command, &ItemRepositoryError::Internal));
    }
    let mut inverse_order = effects.iter().enumerate().collect::<Vec<_>>();
    inverse_order.sort_by_key(|(_, effect)| std::cmp::Reverse(effect.execution_ordinal));
    if inverse_order
        .iter()
        .enumerate()
        .any(|(ordinal, (_, effect))| effect.execution_ordinal != effects.len() - ordinal - 1)
    {
        return Err(item_conflict(first_command, &ItemRepositoryError::Internal));
    }

    let replaced_dependency_targets = inverse_order
        .iter()
        .filter_map(|(_, effect)| effect.before.as_ref().map(|_| effect.after.id))
        .collect::<Vec<_>>();
    clear_dependency_edges_tx(
        transaction,
        scope.workspace_id,
        &replaced_dependency_targets,
    )
    .await
    .map_err(|error| item_conflict(first_command, &error))?;
    let graph_mode = if effects.len() == 1 {
        TransactionalGraphMode::Immediate
    } else {
        TransactionalGraphMode::Deferred
    };
    for (index, effect) in inverse_order {
        let command = &commands[index];
        let current = fetch_item_batch_tx(transaction, scope.workspace_id, effect.after.id, true)
            .await
            .map_err(|error| item_conflict(command, &error))?;
        let stored = StoredEffect {
            operation: command.operation(),
            item_id: effect.after.id,
            before: effect.before.clone(),
        };
        let inverse = inverse_command(&stored, &current)
            .map_err(|_| item_conflict(command, &ItemRepositoryError::Internal))?;
        apply_item_command_tx(transaction, scope, inverse, now, true, graph_mode)
            .await
            .map_err(|error| item_conflict(command, &error))?;
    }
    validate_dependency_graph_batch_tx(transaction, scope.workspace_id)
        .await
        .map_err(|error| item_conflict(first_command, &error))
}

#[derive(Clone)]
struct ProjectedBatchState {
    items: HashMap<Uuid, Item>,
    pending_create_ids: HashSet<Uuid>,
}

const MAX_COMMAND_ORDER_SEARCH_STATES: usize = 50_000;

fn command_execution_order(
    commands: &[ProposalCommand],
    initial_by_id: &HashMap<Uuid, Item>,
    now: DateTime<Utc>,
) -> Result<Vec<usize>, (usize, ItemRepositoryError)> {
    // Only a newly created parent is an unconditional prerequisite. Existing
    // parents may instead need a child detached or trashed before their own
    // command; treating both the old and new parent as predecessors invents
    // cycles and makes otherwise valid final-state batches impossible.
    let created_parent_indices = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            ProposalCommand::CreateItem { item, .. } => Some((item.id, index)),
            ProposalCommand::ReplaceItem { .. }
            | ProposalCommand::TrashItem { .. }
            | ProposalCommand::RestoreItem { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    let mut successors = vec![Vec::<usize>::new(); commands.len()];
    let mut prerequisites = vec![Vec::<usize>::new(); commands.len()];
    let mut indegree = vec![0_usize; commands.len()];
    let mut edges = HashSet::new();
    for (child_index, command) in commands.iter().enumerate() {
        let parent_id = match command {
            ProposalCommand::CreateItem { item, .. } => item.parent_id,
            ProposalCommand::ReplaceItem { item, .. } => item.parent_id,
            ProposalCommand::TrashItem { .. } | ProposalCommand::RestoreItem { .. } => None,
        };
        let Some(parent_index) =
            parent_id.and_then(|parent_id| created_parent_indices.get(&parent_id).copied())
        else {
            continue;
        };
        if edges.insert((parent_index, child_index)) {
            successors[parent_index].push(child_index);
            prerequisites[child_index].push(parent_index);
            indegree[child_index] += 1;
        }
    }
    for command_prerequisites in &mut prerequisites {
        command_prerequisites.sort_unstable();
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let initial_projected = projected_batch_state(commands, initial_by_id, now)?;
    let mut projected = initial_projected.clone();
    let mut order = Vec::with_capacity(commands.len());
    while !ready.is_empty() {
        let mut selected = None;
        let mut first_order_error = None;
        for index in ready.iter().copied() {
            match project_command_for_ordering(&commands[index], &projected, now) {
                Ok(candidate) => {
                    selected = Some((index, candidate));
                    break;
                }
                Err(error) if order_error_can_be_resolved(&error) => {
                    if first_order_error.is_none() {
                        first_order_error = Some((index, error));
                    }
                }
                Err(error) => return Err((index, error)),
            }
        }
        let Some((index, candidate)) = selected else {
            let greedy_error = first_order_error.unwrap_or((
                *ready.first().unwrap_or(&0),
                ItemRepositoryError::HierarchyCycle,
            ));
            return match fallback_command_execution_order(
                commands,
                &initial_projected,
                &prerequisites,
                now,
            )? {
                Some(order) => Ok(order),
                None => Err(greedy_error),
            };
        };
        projected = candidate;
        ready.remove(&index);
        order.push(index);
        successors[index].sort_unstable();
        for successor in &successors[index] {
            indegree[*successor] -= 1;
            if indegree[*successor] == 0 {
                ready.insert(*successor);
            }
        }
    }
    if order.len() == commands.len() && projected.pending_create_ids.is_empty() {
        Ok(order)
    } else {
        Err((
            indegree.iter().position(|degree| *degree > 0).unwrap_or(0),
            ItemRepositoryError::HierarchyCycle,
        ))
    }
}

fn fallback_command_execution_order(
    commands: &[ProposalCommand],
    initial: &ProjectedBatchState,
    prerequisites: &[Vec<usize>],
    now: DateTime<Utc>,
) -> Result<Option<Vec<usize>>, (usize, ItemRepositoryError)> {
    let mut remaining_states = MAX_COMMAND_ORDER_SEARCH_STATES;
    let mut executed = vec![false; commands.len()];
    let mut order = Vec::with_capacity(commands.len());
    let mut exhausted_sets = HashSet::new();
    search_command_execution_order(
        commands,
        initial,
        prerequisites,
        now,
        &mut executed,
        &mut order,
        &mut exhausted_sets,
        &mut remaining_states,
    )
}

#[allow(clippy::too_many_arguments)]
fn search_command_execution_order(
    commands: &[ProposalCommand],
    projected: &ProjectedBatchState,
    prerequisites: &[Vec<usize>],
    now: DateTime<Utc>,
    executed: &mut [bool],
    order: &mut Vec<usize>,
    exhausted_sets: &mut HashSet<Vec<bool>>,
    remaining_states: &mut usize,
) -> Result<Option<Vec<usize>>, (usize, ItemRepositoryError)> {
    if order.len() == commands.len() {
        return Ok(Some(order.clone()));
    }
    let Some(next_remaining) = remaining_states.checked_sub(1) else {
        let index = executed.iter().position(|done| !done).unwrap_or(0);
        return Err((index, ItemRepositoryError::Internal));
    };
    *remaining_states = next_remaining;
    if !exhausted_sets.insert(executed.to_vec()) {
        return Ok(None);
    }

    for index in 0..commands.len() {
        if executed[index]
            || prerequisites[index]
                .iter()
                .any(|prerequisite| !executed[*prerequisite])
        {
            continue;
        }
        let candidate = match project_command_for_ordering(&commands[index], projected, now) {
            Ok(candidate) => candidate,
            Err(error) if order_error_can_be_resolved(&error) => continue,
            Err(error) => return Err((index, error)),
        };
        executed[index] = true;
        order.push(index);
        let result = search_command_execution_order(
            commands,
            &candidate,
            prerequisites,
            now,
            executed,
            order,
            exhausted_sets,
            remaining_states,
        );
        order.pop();
        executed[index] = false;
        if result.as_ref().is_ok_and(Option::is_some) {
            return result;
        }
        result?;
    }
    Ok(None)
}

fn projected_batch_state(
    commands: &[ProposalCommand],
    initial_by_id: &HashMap<Uuid, Item>,
    now: DateTime<Utc>,
) -> Result<ProjectedBatchState, (usize, ItemRepositoryError)> {
    let mut items = initial_by_id.clone();

    // SQL clears every replaced successor's incoming edge set before command
    // execution. Project the same neutral graph so ordering never rejects an
    // acyclic final rewire because of an edge that is already staged away.
    for (index, command) in commands.iter().enumerate() {
        let ProposalCommand::ReplaceItem { item_id, .. } = command else {
            continue;
        };
        let item = items
            .get_mut(item_id)
            .filter(|item| item.deleted_at.is_none())
            .ok_or((index, ItemRepositoryError::NotFound(*item_id)))?;
        item.project_dependencies(&[])
            .map_err(ItemRepositoryError::from)
            .map_err(|error| (index, error))?;
    }

    // Mirror the transaction-local rows inserted by `stage_item_create_tx`.
    // The identity exists for foreign keys, but its final hierarchy,
    // dependency, completion, and blocker state does not exist yet.
    let mut pending_create_ids = HashSet::new();
    for (index, command) in commands.iter().enumerate() {
        let ProposalCommand::CreateItem { item, .. } = command else {
            continue;
        };
        let shell = staged_item_shell(item, now).map_err(|error| (index, error))?;
        if items.insert(shell.id, shell).is_some() || !pending_create_ids.insert(item.id) {
            return Err((index, ItemRepositoryError::Duplicate(item.id)));
        }
    }
    Ok(ProjectedBatchState {
        items,
        pending_create_ids,
    })
}

fn project_command_for_ordering(
    command: &ProposalCommand,
    state: &ProjectedBatchState,
    now: DateTime<Utc>,
) -> Result<ProjectedBatchState, ItemRepositoryError> {
    let mut next = state.clone();
    let (mut item, parents_to_refresh) = match command {
        ProposalCommand::CreateItem { item, .. } => {
            if !next.pending_create_ids.remove(&item.id) {
                return Err(ItemRepositoryError::Duplicate(item.id));
            }
            let item = Item::new(item.clone(), now).map_err(ItemRepositoryError::from)?;
            validate_projected_parent(&next.items, item.id, item.parent_id)?;
            validate_projected_blocker(&next.items, item.blocked_by_item_id)?;
            let parent_id = item.parent_id;
            (item, vec![parent_id])
        }
        ProposalCommand::ReplaceItem { item_id, item, .. } => {
            let current = next
                .items
                .get(item_id)
                .filter(|item| item.deleted_at.is_none())
                .cloned()
                .ok_or(ItemRepositoryError::NotFound(*item_id))?;
            let previous_parent_id = current.parent_id;
            let previous_sibling_order = current.sibling_order;
            let item = current
                .replaced(item.clone(), now)
                .map_err(ItemRepositoryError::from)?;
            validate_projected_parent(&next.items, *item_id, item.parent_id)?;
            validate_projected_blocker(&next.items, item.blocked_by_item_id)?;
            if projected_has_active_children(*item_id, &next.items)
                && item.status.is_executing_state()
            {
                return Err(ItemRepositoryError::NonLeafExecutable);
            }
            let parents = if previous_parent_id != item.parent_id
                || previous_sibling_order != item.sibling_order
            {
                vec![previous_parent_id, item.parent_id]
            } else {
                Vec::new()
            };
            (item, parents)
        }
        ProposalCommand::TrashItem { item_id, .. } => {
            let current = next
                .items
                .get(item_id)
                .filter(|item| item.deleted_at.is_none())
                .cloned()
                .ok_or(ItemRepositoryError::NotFound(*item_id))?;
            if projected_has_active_children(*item_id, &next.items) {
                return Err(ItemRepositoryError::HasChildren);
            }
            let parent_id = current.parent_id;
            (
                current.trashed(now).map_err(ItemRepositoryError::from)?,
                vec![parent_id],
            )
        }
        ProposalCommand::RestoreItem { item_id, .. } => {
            let current = next
                .items
                .get(item_id)
                .filter(|item| item.deleted_at.is_some())
                .cloned()
                .ok_or(ItemRepositoryError::NotFound(*item_id))?;
            validate_projected_parent(&next.items, *item_id, current.parent_id).map_err(
                |error| match error {
                    ItemRepositoryError::ParentNotFound(_) => ItemRepositoryError::DeletedParent,
                    other => other,
                },
            )?;
            let item = current.restored(now).map_err(ItemRepositoryError::from)?;
            if projected_has_active_children(*item_id, &next.items)
                && item.status.is_executing_state()
            {
                return Err(ItemRepositoryError::NonLeafExecutable);
            }
            let parent_id = item.parent_id;
            (item, vec![parent_id])
        }
    };

    let item_id = item.id;
    next.items.insert(item_id, item);
    let has_active_children = projected_has_active_children(item_id, &next.items);
    item = next
        .items
        .get(&item_id)
        .cloned()
        .ok_or(ItemRepositoryError::Internal)?;
    item.is_executable = item.execution_is_allowed(has_active_children);
    next.items.insert(item_id, item);
    refresh_projected_parents(&mut next.items, parents_to_refresh, now)?;
    validate_dependency_graph(&next.items)?;
    Ok(next)
}

fn validate_projected_parent(
    items: &HashMap<Uuid, Item>,
    item_id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if parent_id == item_id {
        return Err(ItemRepositoryError::SelfParent);
    }
    let parent = items
        .get(&parent_id)
        .filter(|item| item.deleted_at.is_none())
        .ok_or(ItemRepositoryError::ParentNotFound(parent_id))?;
    if parent.status.is_executing_state() {
        return Err(ItemRepositoryError::InvalidParentState);
    }
    let mut visited = HashSet::new();
    let mut ancestor_id = Some(parent_id);
    while let Some(ancestor) = ancestor_id {
        if ancestor == item_id || !visited.insert(ancestor) {
            return Err(ItemRepositoryError::HierarchyCycle);
        }
        ancestor_id = items.get(&ancestor).and_then(|item| item.parent_id);
    }
    Ok(())
}

fn validate_projected_blocker(
    items: &HashMap<Uuid, Item>,
    blocked_by_item_id: Option<Uuid>,
) -> Result<(), ItemRepositoryError> {
    if let Some(blocked_by_item_id) = blocked_by_item_id
        && !items.contains_key(&blocked_by_item_id)
    {
        return Err(ItemRepositoryError::BlockedByItemNotFound(
            blocked_by_item_id,
        ));
    }
    Ok(())
}

fn projected_has_active_children(item_id: Uuid, items: &HashMap<Uuid, Item>) -> bool {
    items
        .values()
        .any(|item| item.parent_id == Some(item_id) && item.deleted_at.is_none())
}

fn refresh_projected_parents(
    items: &mut HashMap<Uuid, Item>,
    parent_ids: impl IntoIterator<Item = Option<Uuid>>,
    now: DateTime<Utc>,
) -> Result<(), ItemRepositoryError> {
    let mut parent_ids = parent_ids.into_iter().flatten().collect::<Vec<_>>();
    parent_ids.sort_unstable();
    parent_ids.dedup();
    for parent_id in parent_ids {
        let current = items
            .get(&parent_id)
            .filter(|item| item.deleted_at.is_none())
            .cloned()
            .ok_or(ItemRepositoryError::ParentNotFound(parent_id))?;
        let parent = current
            .refreshed_execution(projected_has_active_children(parent_id, items), now)
            .map_err(ItemRepositoryError::from)?;
        items.insert(parent_id, parent);
    }
    Ok(())
}

const fn order_error_can_be_resolved(error: &ItemRepositoryError) -> bool {
    matches!(
        error,
        ItemRepositoryError::ParentNotFound(_)
            | ItemRepositoryError::HierarchyCycle
            | ItemRepositoryError::DependencyNotFound(_)
            | ItemRepositoryError::CrossRecurringSubtreeDependency { .. }
            | ItemRepositoryError::InvalidParentState
            | ItemRepositoryError::NonLeafExecutable
            | ItemRepositoryError::HasChildren
            | ItemRepositoryError::DeletedParent
            | ItemRepositoryError::DependencyCycle
    )
}

async fn execute_commands(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    commands: &[ProposalCommand],
    now: DateTime<Utc>,
    record: bool,
) -> Result<Vec<TransactionalItemEffect>, ProposalApplicationError> {
    simulate_commands(transaction, scope, commands, now, record)
        .await
        .map_err(|conflict| ProposalApplicationError::Stale(conflict.code))
}

async fn transactional_command_at_current_revision(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    command: &ProposalCommand,
) -> Result<TransactionalItemCommand, ItemRepositoryError> {
    match command {
        ProposalCommand::CreateItem { item, .. } => {
            Ok(TransactionalItemCommand::Create(item.clone()))
        }
        ProposalCommand::ReplaceItem { item_id, item, .. } => {
            Ok(TransactionalItemCommand::Replace {
                item_id: *item_id,
                expected_revision: fetch_item_batch_tx(transaction, workspace_id, *item_id, true)
                    .await?
                    .revision,
                replacement: item.clone(),
            })
        }
        ProposalCommand::TrashItem { item_id, .. } => Ok(TransactionalItemCommand::Trash {
            item_id: *item_id,
            expected_revision: fetch_item_batch_tx(transaction, workspace_id, *item_id, true)
                .await?
                .revision,
        }),
        ProposalCommand::RestoreItem { item_id, .. } => Ok(TransactionalItemCommand::Restore {
            item_id: *item_id,
            expected_revision: fetch_item_batch_tx(transaction, workspace_id, *item_id, true)
                .await?
                .revision,
        }),
    }
}

fn item_conflict(command: &ProposalCommand, error: &ItemRepositoryError) -> ProposalConflict {
    let (code, expected, actual) = match error {
        ItemRepositoryError::Duplicate(_) => (ProposalConflictCode::ItemAlreadyExists, None, None),
        ItemRepositoryError::NotFound(_) => (ProposalConflictCode::ItemNotFound, None, None),
        ItemRepositoryError::RevisionConflict { expected, actual } => (
            ProposalConflictCode::ItemRevisionMismatch,
            Some(*expected),
            Some(*actual),
        ),
        ItemRepositoryError::ParentNotFound(_) => {
            (ProposalConflictCode::ParentNotFound, None, None)
        }
        ItemRepositoryError::HierarchyCycle | ItemRepositoryError::SelfParent => {
            (ProposalConflictCode::HierarchyCycle, None, None)
        }
        ItemRepositoryError::DependencyNotFound(_) => {
            (ProposalConflictCode::DependencyNotFound, None, None)
        }
        ItemRepositoryError::DependencyCycle => (ProposalConflictCode::DependencyCycle, None, None),
        ItemRepositoryError::InvalidParentState => {
            (ProposalConflictCode::InvalidParentState, None, None)
        }
        ItemRepositoryError::NonLeafExecutable => {
            (ProposalConflictCode::NonLeafExecutable, None, None)
        }
        ItemRepositoryError::ActiveExecutionConflict { .. }
        | ItemRepositoryError::CrossRecurringSubtreeDependency { .. } => {
            (ProposalConflictCode::InvalidItem, None, None)
        }
        ItemRepositoryError::HasChildren => (ProposalConflictCode::HasChildren, None, None),
        ItemRepositoryError::DeletedParent => (ProposalConflictCode::DeletedParent, None, None),
        _ => (ProposalConflictCode::InvalidItem, None, None),
    };
    let summary = match error {
        ItemRepositoryError::DependencyNotFound(_) => {
            "A dependency predecessor no longer exists in this workspace."
        }
        ItemRepositoryError::DependencyCycle => {
            "The proposed dependency change would create a cycle."
        }
        ItemRepositoryError::CrossRecurringSubtreeDependency { .. } => {
            "A dependency cannot point into a different materialized recurring subtree."
        }
        ItemRepositoryError::DeltaGroupTooLarge => {
            "Split this proposal into smaller batches because its atomic sync payload exceeds the safe device-delivery limit."
        }
        _ => "This change no longer satisfies the current item constraints.",
    };
    ProposalConflict {
        code,
        command_id: Some(command.command_id()),
        item_id: Some(command.target_item_id()),
        expected_revision: expected,
        actual_revision: actual,
        summary: summary.to_owned(),
    }
}

fn item_diff(command: &ProposalCommand, effect: &TransactionalItemEffect) -> ProposalItemDiff {
    ProposalItemDiff {
        command_id: command.command_id(),
        operation: command.operation(),
        item_id: command.target_item_id(),
        changed_fields: changed_fields(effect.before.as_ref(), Some(&effect.after)),
        before: effect.before.clone(),
        after: Some(effect.after.clone()),
    }
}

fn implicit_item_diffs(
    before_items: &[Item],
    after_items: &[Item],
    commands: &[ProposalCommand],
) -> Vec<ProposalImplicitItemDiff> {
    let direct_targets = commands
        .iter()
        .map(ProposalCommand::target_item_id)
        .collect::<HashSet<_>>();
    let after_by_id = after_items
        .iter()
        .map(|item| (item.id, item))
        .collect::<HashMap<_, _>>();
    before_items
        .iter()
        .filter(|before| !direct_targets.contains(&before.id))
        .filter_map(|before| {
            let after = after_by_id.get(&before.id)?;
            (before != *after).then(|| ProposalImplicitItemDiff {
                item_id: before.id,
                reason: ProposalImplicitChangeReason::HierarchyRefresh,
                changed_fields: changed_fields(Some(before), Some(after)),
                before: before.clone(),
                after: (*after).clone(),
            })
        })
        .collect()
}

fn affected_preview_item_ids(
    commands: &[ProposalCommand],
    effects: &[TransactionalItemEffect],
    implicit_diffs: &[ProposalImplicitItemDiff],
) -> Vec<Uuid> {
    let mut ids = commands
        .iter()
        .map(ProposalCommand::target_item_id)
        .chain(effects.iter().map(|effect| effect.after.id))
        .chain(implicit_diffs.iter().map(|diff| diff.item_id))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn changed_fields(before: Option<&Item>, after: Option<&Item>) -> Vec<ProposalItemField> {
    let Some(before) = before else {
        return vec![
            ProposalItemField::IsSensitive,
            ProposalItemField::Kind,
            ProposalItemField::Status,
            ProposalItemField::Title,
            ProposalItemField::Notes,
            ProposalItemField::TimezoneName,
            ProposalItemField::DurationKind,
            ProposalItemField::DurationSeconds,
            ProposalItemField::DurationMinSeconds,
            ProposalItemField::DurationMaxSeconds,
            ProposalItemField::DurationSource,
            ProposalItemField::DeadlineKind,
            ProposalItemField::DeadlineDate,
            ProposalItemField::DeadlineAt,
            ProposalItemField::DeadlineStrength,
            ProposalItemField::DeadlineSoftWeight,
            ProposalItemField::EarliestStartAt,
            ProposalItemField::Recurrence,
            ProposalItemField::FlexibleConstraints,
            ProposalItemField::Dependencies,
            ProposalItemField::HasOwnEffort,
            ProposalItemField::SplitPolicy,
            ProposalItemField::Importance,
            ProposalItemField::Urgency,
            ProposalItemField::ParentId,
            ProposalItemField::SiblingOrder,
            ProposalItemField::BlockedReasonKind,
            ProposalItemField::BlockedByItemId,
            ProposalItemField::BlockedReason,
            ProposalItemField::IsExecutable,
            ProposalItemField::Revision,
            ProposalItemField::CompletedAt,
            ProposalItemField::DeletedAt,
        ];
    };
    let Some(after) = after else {
        return vec![ProposalItemField::DeletedAt];
    };
    let mut fields = Vec::new();
    macro_rules! changed {
        ($field:ident, $variant:ident) => {
            if before.$field != after.$field {
                fields.push(ProposalItemField::$variant);
            }
        };
    }
    changed!(is_sensitive, IsSensitive);
    changed!(kind, Kind);
    changed!(status, Status);
    changed!(title, Title);
    changed!(notes, Notes);
    changed!(timezone_name, TimezoneName);
    changed!(duration_kind, DurationKind);
    changed!(duration_seconds, DurationSeconds);
    changed!(duration_min_seconds, DurationMinSeconds);
    changed!(duration_max_seconds, DurationMaxSeconds);
    changed!(duration_source, DurationSource);
    changed!(deadline_kind, DeadlineKind);
    changed!(deadline_date, DeadlineDate);
    changed!(deadline_at, DeadlineAt);
    changed!(deadline_strength, DeadlineStrength);
    changed!(deadline_soft_weight, DeadlineSoftWeight);
    changed!(earliest_start_at, EarliestStartAt);
    changed!(recurrence, Recurrence);
    if before.constraints_without_dependencies() != after.constraints_without_dependencies() {
        fields.push(ProposalItemField::FlexibleConstraints);
    }
    if before.dependencies() != after.dependencies() {
        fields.push(ProposalItemField::Dependencies);
    }
    changed!(has_own_effort, HasOwnEffort);
    changed!(split_policy, SplitPolicy);
    changed!(importance, Importance);
    changed!(urgency, Urgency);
    changed!(parent_id, ParentId);
    changed!(sibling_order, SiblingOrder);
    changed!(blocked_reason_kind, BlockedReasonKind);
    changed!(blocked_by_item_id, BlockedByItemId);
    changed!(blocked_reason, BlockedReason);
    changed!(is_executable, IsExecutable);
    changed!(revision, Revision);
    changed!(completed_at, CompletedAt);
    changed!(deleted_at, DeletedAt);
    fields
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeadlineRiskShape {
    kind: DeadlineKind,
    date: Option<NaiveDate>,
    instant: Option<DateTime<Utc>>,
    strength: Option<DeadlineStrength>,
    soft_weight: Option<u32>,
}

impl DeadlineRiskShape {
    fn new(
        kind: DeadlineKind,
        date: Option<NaiveDate>,
        instant: Option<DateTime<Utc>>,
        strength: Option<DeadlineStrength>,
        soft_weight: Option<u32>,
    ) -> Self {
        match kind {
            DeadlineKind::None => Self {
                kind,
                date: None,
                instant: None,
                strength: None,
                soft_weight: None,
            },
            DeadlineKind::Date => Self {
                kind,
                date,
                instant: None,
                strength,
                soft_weight: (strength == Some(DeadlineStrength::Soft))
                    .then_some(soft_weight)
                    .flatten(),
            },
            DeadlineKind::DateTime => Self {
                kind,
                date: None,
                instant,
                strength,
                soft_weight: (strength == Some(DeadlineStrength::Soft))
                    .then_some(soft_weight)
                    .flatten(),
            },
        }
    }

    fn from_item(item: &Item) -> Self {
        Self::new(
            item.deadline_kind,
            item.deadline_date,
            item.deadline_at,
            item.deadline_strength,
            item.deadline_soft_weight,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadlineRiskChange {
    Changed,
    Relaxed,
}

fn deadline_risk_change(before: &Item, after: &Item) -> Option<DeadlineRiskChange> {
    classify_deadline_risk_change(
        &DeadlineRiskShape::from_item(before),
        &DeadlineRiskShape::from_item(after),
    )
}

fn classify_deadline_risk_change(
    before: &DeadlineRiskShape,
    after: &DeadlineRiskShape,
) -> Option<DeadlineRiskChange> {
    if before == after {
        return None;
    }

    let mut relaxed = false;
    let mut tightened = false;
    match (before.kind, after.kind) {
        (DeadlineKind::None, DeadlineKind::Date | DeadlineKind::DateTime) => tightened = true,
        (DeadlineKind::Date | DeadlineKind::DateTime, DeadlineKind::None) => relaxed = true,
        (DeadlineKind::Date, DeadlineKind::Date) => {
            if let (Some(old), Some(new)) = (before.date, after.date) {
                relaxed |= new > old;
                tightened |= new < old;
            }
        }
        (DeadlineKind::DateTime, DeadlineKind::DateTime) => {
            if let (Some(old), Some(new)) = (before.instant, after.instant) {
                relaxed |= new > old;
                tightened |= new < old;
            }
        }
        _ => {}
    }

    if before.kind != DeadlineKind::None && after.kind != DeadlineKind::None {
        match (before.strength, after.strength) {
            (Some(DeadlineStrength::Hard), Some(DeadlineStrength::Soft)) => relaxed = true,
            (Some(DeadlineStrength::Soft), Some(DeadlineStrength::Hard)) => tightened = true,
            (Some(DeadlineStrength::Soft), Some(DeadlineStrength::Soft)) => {
                if let (Some(old), Some(new)) = (before.soft_weight, after.soft_weight) {
                    relaxed |= new < old;
                    tightened |= new > old;
                }
            }
            _ => {}
        }
    }

    Some(if relaxed && !tightened {
        DeadlineRiskChange::Relaxed
    } else {
        DeadlineRiskChange::Changed
    })
}

#[allow(clippy::too_many_lines)] // Each material field class is deliberately explicit in the approval model.
fn proposal_risks(
    commands: &[ProposalCommand],
    effects: &[TransactionalItemEffect],
) -> Vec<ProposalRisk> {
    let mut risks = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let (code, level, approval, summary) = match command {
                ProposalCommand::CreateItem { .. } => (
                    ProposalRiskCode::CreatesItem,
                    ProposalRiskLevel::Low,
                    false,
                    "Creates a reversible local item.",
                ),
                ProposalCommand::ReplaceItem { .. } => (
                    ProposalRiskCode::ReplacesItem,
                    ProposalRiskLevel::Medium,
                    true,
                    "Replaces the current local item fields.",
                ),
                ProposalCommand::TrashItem { .. } => (
                    ProposalRiskCode::TrashesItem,
                    ProposalRiskLevel::High,
                    true,
                    "Moves a local item to Trash.",
                ),
                ProposalCommand::RestoreItem { .. } => (
                    ProposalRiskCode::RestoresItem,
                    ProposalRiskLevel::Medium,
                    true,
                    "Restores a previously trashed local item.",
                ),
            };
            let sensitive = effects.get(index).is_some_and(|effect| {
                effect.after.is_sensitive
                    || effect
                        .before
                        .as_ref()
                        .is_some_and(|before| before.is_sensitive)
            });
            ProposalRisk {
                code: if sensitive {
                    ProposalRiskCode::SensitiveContent
                } else {
                    code
                },
                level: if sensitive {
                    level.max(ProposalRiskLevel::Medium)
                } else {
                    level
                },
                command_id: Some(command.command_id()),
                item_id: Some(command.target_item_id()),
                requires_explicit_approval: approval || sensitive,
                summary: if sensitive {
                    "Changes an item marked sensitive. Content remains inside the owner boundary."
                        .to_owned()
                } else {
                    summary.to_owned()
                },
            }
        })
        .collect::<Vec<_>>();
    for (command, effect) in commands.iter().zip(effects) {
        if matches!(command, ProposalCommand::CreateItem { item, .. } if item.parent_id.is_some()) {
            risks.push(ProposalRisk {
                code: ProposalRiskCode::ChangesHierarchy,
                level: ProposalRiskLevel::High,
                command_id: Some(command.command_id()),
                item_id: Some(command.target_item_id()),
                requires_explicit_approval: true,
                summary: "Adds an item beneath an existing or proposed hierarchy parent."
                    .to_owned(),
            });
        }
        let dependencies_changed = effect.before.as_ref().map_or_else(
            || {
                effect
                    .after
                    .dependencies()
                    .is_ok_and(|value| !value.is_empty())
            },
            |before| before.dependencies() != effect.after.dependencies(),
        );
        if dependencies_changed {
            risks.push(ProposalRisk {
                code: ProposalRiskCode::ChangesDependencies,
                level: ProposalRiskLevel::High,
                command_id: Some(command.command_id()),
                item_id: Some(command.target_item_id()),
                requires_explicit_approval: true,
                summary: "Changes which items constrain this item's schedule.".to_owned(),
            });
        }
        let Some(before) = effect.before.as_ref() else {
            continue;
        };
        let after = &effect.after;
        let mut material = Vec::new();
        if let Some(deadline_change) = deadline_risk_change(before, after) {
            let relaxed = deadline_change == DeadlineRiskChange::Relaxed;
            material.push((
                if relaxed {
                    ProposalRiskCode::RelaxesDeadline
                } else {
                    ProposalRiskCode::ChangesDeadline
                },
                ProposalRiskLevel::Medium,
                if relaxed {
                    "Relaxes or removes the current deadline."
                } else {
                    "Changes the current deadline."
                },
            ));
        }
        if before.parent_id != after.parent_id || before.sibling_order != after.sibling_order {
            material.push((
                ProposalRiskCode::ChangesHierarchy,
                ProposalRiskLevel::High,
                "Moves the item within the goal hierarchy.",
            ));
        }
        if before.recurrence != after.recurrence {
            material.push((
                ProposalRiskCode::ChangesRecurrence,
                ProposalRiskLevel::High,
                "Changes the recurrence definition.",
            ));
        }
        if before.status != after.status {
            material.push((
                ProposalRiskCode::ChangesExecutionState,
                ProposalRiskLevel::Medium,
                "Changes the item's execution state.",
            ));
        }
        if before.is_sensitive != after.is_sensitive {
            material.push((
                ProposalRiskCode::ChangesSensitivity,
                ProposalRiskLevel::High,
                "Changes the item's sensitivity boundary.",
            ));
        }
        risks.extend(
            material
                .into_iter()
                .map(|(code, level, summary)| ProposalRisk {
                    code,
                    level,
                    command_id: Some(command.command_id()),
                    item_id: Some(command.target_item_id()),
                    requires_explicit_approval: true,
                    summary: summary.to_owned(),
                }),
        );
    }
    if commands.len() > 1 {
        risks.push(ProposalRisk {
            code: ProposalRiskCode::BulkChange,
            level: ProposalRiskLevel::Medium,
            command_id: None,
            item_id: None,
            requires_explicit_approval: true,
            summary: format!("Applies {} changes as one atomic group.", commands.len()),
        });
    }
    risks
}

#[allow(clippy::too_many_arguments)]
async fn persist_preview(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    preview_id: Uuid,
    members: &[StoredPreviewMember],
    command_count: usize,
    commands_hash: [u8; 32],
    canonical_hash: [u8; 32],
    review_content_hash: [u8; 32],
    preview_hash: [u8; 32],
    can_apply: bool,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "INSERT INTO proposal_apply_previews (id, workspace_id, user_id, proposal_count, \
         command_count, commands_hash, canonical_hash, review_content_hash, preview_hash, \
         can_apply, created_at, expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(preview_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(i16::try_from(members.len()).map_err(|_| ProposalApplicationError::Internal)?)
    .bind(i16::try_from(command_count).map_err(|_| ProposalApplicationError::Internal)?)
    .bind(commands_hash.as_slice())
    .bind(canonical_hash.as_slice())
    .bind(review_content_hash.as_slice())
    .bind(preview_hash.as_slice())
    .bind(can_apply)
    .bind(created_at)
    .bind(expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(map_preview_insert_error)?;
    for (ordinal, member) in members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO proposal_apply_preview_members (workspace_id,user_id,preview_id,ordinal, \
             proposal_id,proposal_revision,proposal_payload_hash) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(preview_id)
        .bind(i16::try_from(ordinal).map_err(|_| ProposalApplicationError::Internal)?)
        .bind(member.proposal_id)
        .bind(revision_i64(member.proposal_revision)?)
        .bind(member.payload_hash.as_slice())
        .execute(&mut **transaction)
        .await
        .map_err(map_preview_insert_error)?;
    }
    Ok(())
}

async fn lock_preview(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    preview_id: Uuid,
) -> Result<StoredPreview, ProposalApplicationError> {
    let row = sqlx::query(
        "SELECT id, proposal_count, command_count, commands_hash, canonical_hash, \
         review_content_hash, preview_hash, can_apply, created_at, expires_at FROM proposal_apply_previews \
         WHERE workspace_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ProposalApplicationError::NotFound)?;
    stored_preview(&row)
}

fn stored_preview(row: &PgRow) -> Result<StoredPreview, ProposalApplicationError> {
    Ok(StoredPreview {
        id: row.try_get("id").map_err(internal)?,
        proposal_count: positive_usize(row.try_get::<i16, _>("proposal_count").map_err(internal)?)?,
        command_count: positive_usize(row.try_get::<i16, _>("command_count").map_err(internal)?)?,
        commands_hash: bytes32(row.try_get("commands_hash").map_err(internal)?)?,
        canonical_hash: bytes32(row.try_get("canonical_hash").map_err(internal)?)?,
        review_content_hash: bytes32(row.try_get("review_content_hash").map_err(internal)?)?,
        preview_hash: bytes32(row.try_get("preview_hash").map_err(internal)?)?,
        can_apply: row.try_get("can_apply").map_err(internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        expires_at: row.try_get("expires_at").map_err(internal)?,
    })
}

async fn load_preview_members(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    preview_id: Uuid,
) -> Result<Vec<StoredPreviewMember>, ProposalApplicationError> {
    let rows = sqlx::query(
        "SELECT proposal_id, proposal_revision, proposal_payload_hash \
         FROM proposal_apply_preview_members WHERE workspace_id=$1 AND user_id=$2 \
         AND preview_id=$3 ORDER BY ordinal",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    rows.iter()
        .map(|row| {
            Ok(StoredPreviewMember {
                proposal_id: row.try_get("proposal_id").map_err(internal)?,
                proposal_revision: revision_u64(
                    row.try_get::<i64, _>("proposal_revision")
                        .map_err(internal)?,
                )?,
                payload_hash: bytes32(row.try_get("proposal_payload_hash").map_err(internal)?)?,
            })
        })
        .collect()
}

async fn lock_preview_proposals(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    members: &[StoredPreviewMember],
) -> Result<Vec<Proposal>, ProposalApplicationError> {
    let mut sorted = members.to_vec();
    sorted.sort_by_key(|member| member.proposal_id);
    let mut by_id = std::collections::HashMap::with_capacity(sorted.len());
    for member in sorted {
        let row = sqlx::query(LOCKED_PROPOSAL_SELECT)
            .bind(scope.workspace_id)
            .bind(member.proposal_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(internal)?
            .ok_or(ProposalApplicationError::NotFound)?;
        let proposal = proposal_from_row(&row).map_err(|_| ProposalApplicationError::Internal)?;
        if proposal.status != ProposalStatus::Pending {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::ProposalNotPending,
            ));
        }
        if proposal.revision != member.proposal_revision {
            return Err(ProposalApplicationError::RevisionConflict {
                expected: member.proposal_revision,
                actual: proposal.revision,
            });
        }
        if hash_json(&proposal.payload)? != member.payload_hash {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::ProposalRevisionMismatch,
            ));
        }
        by_id.insert(proposal.id, proposal);
    }
    members
        .iter()
        .map(|member| {
            by_id
                .remove(&member.proposal_id)
                .ok_or(ProposalApplicationError::Internal)
        })
        .collect()
}

fn validate_stored_preview(
    preview: &StoredPreview,
    scope: DatabaseScope,
    members: &[StoredPreviewMember],
    proposals: &[Proposal],
    commands: &[ProposalCommand],
) -> Result<(), ProposalApplicationError> {
    if members.len() != preview.proposal_count
        || proposals.len() != preview.proposal_count
        || commands.len() != preview.command_count
    {
        return Err(ProposalApplicationError::Internal);
    }
    if hash_json(commands)? != preview.commands_hash {
        return Err(ProposalApplicationError::Internal);
    }
    let expected_preview_hash = calculate_preview_hash(
        preview.id,
        scope,
        members,
        preview.command_count,
        preview.commands_hash,
        preview.canonical_hash,
        preview.review_content_hash,
        preview.can_apply,
        preview.created_at,
        preview.expires_at,
    );
    if expected_preview_hash != preview.preview_hash {
        return Err(ProposalApplicationError::Internal);
    }
    for proposal in proposals {
        let change_set = ProposalChangeSet::from_payload(&proposal.payload)
            .map_err(|_| ProposalApplicationError::Internal)?;
        validate_kind_contract(proposal.kind, &change_set.commands)?;
    }
    Ok(())
}

async fn provider_managed_conflicts(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    commands: &[ProposalCommand],
    item_ids: &[Uuid],
) -> Result<Vec<ProposalConflict>, ProposalApplicationError> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let managed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT local_entity_id FROM provider_sync_mappings WHERE workspace_id=$1 \
         AND local_entity_id = ANY($2) AND tombstoned_at IS NULL \
         AND entity_kind IN ('item','calendar_occurrence') ORDER BY local_entity_id",
    )
    .bind(scope.workspace_id)
    .bind(item_ids)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(managed
        .into_iter()
        .map(|item_id| ProposalConflict {
            code: ProposalConflictCode::ProviderManagedItem,
            command_id: commands
                .iter()
                .find(|command| command.target_item_id() == item_id)
                .map(ProposalCommand::command_id),
            item_id: Some(item_id),
            expected_revision: None,
            actual_revision: None,
            summary: "This item is managed by an external calendar and cannot be changed by an AI proposal."
                .to_owned(),
        })
        .collect())
}

async fn reject_provider_managed_items(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_ids: &[Uuid],
) -> Result<(), ProposalApplicationError> {
    if item_ids.is_empty() {
        return Ok(());
    }
    if provider_managed_conflicts(transaction, scope, &[], item_ids)
        .await?
        .is_empty()
    {
        Ok(())
    } else {
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::ProviderManagedItem,
        ))
    }
}

#[allow(clippy::too_many_arguments)] // Every approval capability binding is explicit and domain-separated.
fn calculate_preview_hash(
    preview_id: Uuid,
    scope: DatabaseScope,
    members: &[StoredPreviewMember],
    command_count: usize,
    commands_hash: [u8; 32],
    canonical_hash: [u8; 32],
    review_content_hash: [u8; 32],
    can_apply: bool,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dayweave.proposal.preview.v2\0");
    digest.update(preview_id.as_bytes());
    digest.update(scope.workspace_id.as_bytes());
    digest.update(scope.user_id.as_bytes());
    digest.update(
        u64::try_from(command_count)
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(commands_hash);
    digest.update(canonical_hash);
    digest.update(review_content_hash);
    digest.update([u8::from(can_apply)]);
    digest.update(created_at.timestamp_micros().to_be_bytes());
    digest.update(expires_at.timestamp_micros().to_be_bytes());
    for member in members {
        digest.update(member.proposal_id.as_bytes());
        digest.update(member.proposal_revision.to_be_bytes());
        digest.update(member.payload_hash);
    }
    digest.finalize().into()
}

async fn canonical_item_hash(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<[u8; 32], ProposalApplicationError> {
    let rows = sqlx::query(
        "SELECT id,revision,trashed_at IS NOT NULL AS deleted FROM items \
         WHERE workspace_id=$1 ORDER BY id",
    )
    .bind(workspace_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    let mut digest = Sha256::new();
    digest.update(b"dayweave.proposal.canonical-items.v2\0items\0");
    digest.update(workspace_id.as_bytes());
    for row in rows {
        let item_id: Uuid = row.try_get("id").map_err(internal)?;
        let revision: i64 = row.try_get("revision").map_err(internal)?;
        let deleted: bool = row.try_get("deleted").map_err(internal)?;
        digest.update(item_id.as_bytes());
        digest.update(revision.to_be_bytes());
        digest.update([u8::from(deleted)]);
    }
    let managed_item_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT local_entity_id FROM provider_sync_mappings \
         WHERE workspace_id=$1 AND local_entity_id IS NOT NULL AND tombstoned_at IS NULL \
         AND entity_kind IN ('item','calendar_occurrence') ORDER BY local_entity_id",
    )
    .bind(workspace_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    digest.update(b"\0provider-managed-items\0");
    for item_id in managed_item_ids {
        digest.update(item_id.as_bytes());
    }
    Ok(digest.finalize().into())
}

fn hash_json<T: Serialize + ?Sized>(value: &T) -> Result<[u8; 32], ProposalApplicationError> {
    let encoded = serde_json::to_vec(value).map_err(|_| ProposalApplicationError::Internal)?;
    Ok(hash_bytes(&encoded))
}

fn hash_domain_json<T: Serialize + ?Sized>(
    domain: &[u8],
    value: &T,
) -> Result<[u8; 32], ProposalApplicationError> {
    let encoded = serde_json::to_vec(value).map_err(|_| ProposalApplicationError::Internal)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(encoded);
    Ok(digest.finalize().into())
}

fn hash_bytes(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn encoded_hash(hash: [u8; 32]) -> String {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_hash(value: &str) -> Result<[u8; 32], ProposalApplicationError> {
    if value.len() > MAX_REVIEW_HASH_LENGTH {
        return Err(validation("expected_review_hash is invalid"));
    }
    let encoded = value
        .strip_prefix("sha256:")
        .ok_or_else(|| validation("expected_review_hash is invalid"))?;
    if encoded.len() != 64 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(validation("expected_review_hash is invalid"));
    }
    let mut decoded = [0_u8; 32];
    for (index, output) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *output = u8::from_str_radix(&encoded[offset..offset + 2], 16)
            .map_err(|_| validation("expected_review_hash is invalid"))?;
    }
    Ok(decoded)
}

async fn replay_request(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    operation: &str,
    key_hash: [u8; 32],
    request_hash: [u8; 32],
) -> Result<Option<Uuid>, ProposalApplicationError> {
    let row = sqlx::query(
        "SELECT request_hash, application_id FROM proposal_application_requests \
         WHERE workspace_id=$1 AND user_id=$2 AND operation=$3 AND key_hash=$4 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(operation)
    .bind(key_hash.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_hash = bytes32(row.try_get("request_hash").map_err(internal)?)?;
    if stored_hash != request_hash {
        return Err(ProposalApplicationError::IdempotencyConflict);
    }
    Ok(Some(row.try_get("application_id").map_err(internal)?))
}

async fn item_change_watermark(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
) -> Result<i64, ProposalApplicationError> {
    sqlx::query_scalar("SELECT COALESCE(MAX(sequence),0) FROM item_changes WHERE workspace_id=$1")
        .bind(workspace_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(internal)
}

async fn scrub_expired_effect_snapshots(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "UPDATE proposal_application_effects AS effect \
         SET before_snapshot=NULL,after_snapshot=NULL,snapshots_scrubbed_at=$3 \
         FROM proposal_applications AS application \
         WHERE effect.workspace_id=$1 AND effect.user_id=$2 \
         AND application.workspace_id=effect.workspace_id \
         AND application.user_id=effect.user_id AND application.id=effect.application_id \
         AND application.undo_expires_at <= $3 AND effect.snapshots_scrubbed_at IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn prune_and_limit_previews(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    prune_expired_previews(transaction, scope, now).await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM proposal_apply_previews WHERE workspace_id=$1 \
         AND user_id=$2 AND expires_at > $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)?;
    if active >= MAX_ACTIVE_PREVIEWS {
        Err(validation(
            "too many active proposal previews; wait for an existing preview to expire",
        ))
    } else {
        Ok(())
    }
}

async fn prune_expired_previews(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "DELETE FROM proposal_apply_previews AS preview WHERE preview.workspace_id=$1 \
         AND preview.user_id=$2 AND preview.expires_at <= $3 AND NOT EXISTS ( \
             SELECT 1 FROM proposal_applications AS application \
             WHERE application.workspace_id=preview.workspace_id \
             AND application.user_id=preview.user_id AND application.preview_id=preview.id)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn changed_item_ids_since(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    watermark: i64,
) -> Result<Vec<Uuid>, ProposalApplicationError> {
    let ids = sqlx::query_scalar(
        "SELECT DISTINCT item_id FROM item_changes WHERE workspace_id=$1 AND sequence>$2 \
         ORDER BY item_id",
    )
    .bind(workspace_id)
    .bind(watermark)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    if ids.is_empty() {
        Err(ProposalApplicationError::Internal)
    } else {
        Ok(ids)
    }
}

async fn fetch_items(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    item_ids: &[Uuid],
) -> Result<Vec<Item>, ProposalApplicationError> {
    let mut items = Vec::with_capacity(item_ids.len());
    for item_id in item_ids {
        items.push(
            fetch_item_batch_tx(transaction, workspace_id, *item_id, true)
                .await
                .map_err(map_item_error)?,
        );
    }
    Ok(items)
}

#[allow(clippy::too_many_arguments)]
async fn insert_application_audit(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    actor_session_id: Option<Uuid>,
    audit_id: Uuid,
    application_id: Uuid,
    operation: &str,
    base_revision: Option<u64>,
    result_revision: Option<u64>,
    command_count: usize,
    affected_count: usize,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "INSERT INTO audit_operations (id,workspace_id,actor_user_id,actor_session_id, \
         operation_type,entity_type,entity_id,base_revision,result_revision,outcome,metadata) \
         VALUES ($1,$2,$3,$4,$5,'proposal_application',$6,$7,$8,'succeeded',$9)",
    )
    .bind(audit_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(actor_session_id)
    .bind(operation)
    .bind(application_id)
    .bind(base_revision.map(revision_i64).transpose()?)
    .bind(result_revision.map(revision_i64).transpose()?)
    .bind(json!({
        "command_count": command_count,
        "affected_item_count": affected_count,
    }))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_application_header(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    preview: &StoredPreview,
    audit_id: Uuid,
    effect_count: usize,
    fence_count: usize,
    applied_at: DateTime<Utc>,
    undo_expires_at: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "INSERT INTO proposal_applications (id,workspace_id,user_id,preview_id,preview_hash, \
         status,revision,effect_count,fence_count,apply_audit_id,applied_at,undo_expires_at) \
         VALUES ($1,$2,$3,$4,$5,'applied',1,$6,$7,$8,$9,$10)",
    )
    .bind(application_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview.id)
    .bind(preview.preview_hash.as_slice())
    .bind(i16::try_from(effect_count).map_err(|_| ProposalApplicationError::Internal)?)
    .bind(i32::try_from(fence_count).map_err(|_| ProposalApplicationError::Internal)?)
    .bind(audit_id)
    .bind(applied_at)
    .bind(undo_expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(map_application_insert_error)?;
    Ok(())
}

async fn insert_application_members(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    members: &[StoredPreviewMember],
) -> Result<(), ProposalApplicationError> {
    for (ordinal, member) in members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO proposal_application_members (workspace_id,user_id,application_id, \
             ordinal,proposal_id) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(application_id)
        .bind(i16::try_from(ordinal).map_err(|_| ProposalApplicationError::Internal)?)
        .bind(member.proposal_id)
        .execute(&mut **transaction)
        .await
        .map_err(map_application_insert_error)?;
    }
    Ok(())
}

async fn insert_effects(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    commands: &[ProposalCommand],
    effects: &[TransactionalItemEffect],
    final_items: &[Item],
    now: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    if commands.len() != effects.len() {
        return Err(ProposalApplicationError::Internal);
    }
    for (review_ordinal, (command, effect)) in commands.iter().zip(effects).enumerate() {
        let final_item = final_items
            .iter()
            .find(|item| item.id == command.target_item_id())
            .ok_or(ProposalApplicationError::Internal)?;
        let command_hash = hash_json(command)?;
        let before_snapshot = effect
            .before
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| ProposalApplicationError::Internal)?;
        let after_snapshot =
            serde_json::to_value(final_item).map_err(|_| ProposalApplicationError::Internal)?;
        let before_snapshot_hash = effect
            .before
            .as_ref()
            .map(|item| hash_domain_json(b"dayweave.proposal.item-snapshot.v1\0", item))
            .transpose()?;
        let after_snapshot_hash =
            hash_domain_json(b"dayweave.proposal.item-snapshot.v1\0", final_item)?;
        sqlx::query(
            "INSERT INTO proposal_application_effects (workspace_id,user_id,application_id,ordinal, \
             review_ordinal,action_id,operation,command_hash,item_id,expected_revision, \
             before_revision,after_revision,before_deleted,after_deleted,before_snapshot_hash, \
             after_snapshot_hash,before_snapshot,after_snapshot,created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(application_id)
        .bind(
            i16::try_from(effect.execution_ordinal)
                .map_err(|_| ProposalApplicationError::Internal)?,
        )
        .bind(i16::try_from(review_ordinal).map_err(|_| ProposalApplicationError::Internal)?)
        .bind(command.command_id())
        .bind(operation_name(command.operation()))
        .bind(command_hash.as_slice())
        .bind(command.target_item_id())
        .bind(command.expected_revision().map(revision_i64).transpose()?)
        .bind(effect.before.as_ref().map(|item| revision_i64(item.revision)).transpose()?)
        .bind(revision_i64(final_item.revision)?)
        .bind(effect.before.as_ref().map(|item| item.deleted_at.is_some()))
        .bind(final_item.deleted_at.is_some())
        .bind(before_snapshot_hash.map(|hash| hash.to_vec()))
        .bind(after_snapshot_hash.as_slice())
        .bind(before_snapshot)
        .bind(after_snapshot)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    }
    Ok(())
}

async fn insert_fences(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    items: &[Item],
) -> Result<(), ProposalApplicationError> {
    for item in items {
        sqlx::query(
            "INSERT INTO proposal_application_fences (workspace_id,user_id,application_id,item_id, \
             applied_revision,applied_deleted) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(application_id)
        .bind(item.id)
        .bind(revision_i64(item.revision)?)
        .bind(item.deleted_at.is_some())
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    }
    Ok(())
}

async fn accept_proposals(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    proposals: &[Proposal],
    actor_session_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    for proposal in proposals {
        let mut accepted = proposal.clone();
        accepted
            .decide(DecisionKind::Accept, None, now)
            .map_err(|_| ProposalApplicationError::Internal)?;
        let updated = sqlx::query(
            "UPDATE proposals SET status='accepted', revision=$3, decision_note=NULL, \
             updated_at=$4, decided_at=$4 WHERE workspace_id=$1 AND id=$2 \
             AND revision=$5 AND status='pending' AND trashed_at IS NULL",
        )
        .bind(scope.workspace_id)
        .bind(proposal.id)
        .bind(revision_i64(accepted.revision)?)
        .bind(now)
        .bind(revision_i64(proposal.revision)?)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated != 1 {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::ProposalRevisionMismatch,
            ));
        }
        sqlx::query(
            "INSERT INTO outbox_messages (id,workspace_id,aggregate_type,aggregate_id, \
             aggregate_revision,event_type,deduplication_key,payload) \
             VALUES ($1,$2,'proposal',$3,$4,'proposal.accepted',$5,$6)",
        )
        .bind(Uuid::new_v4())
        .bind(scope.workspace_id)
        .bind(proposal.id)
        .bind(revision_i64(accepted.revision)?)
        .bind(format!(
            "proposal.accepted:{}:{}",
            proposal.id, accepted.revision
        ))
        .bind(json!({
            "proposal_id": proposal.id,
            "revision": accepted.revision,
            "status": "accepted",
        }))
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
        sqlx::query(
            "INSERT INTO audit_operations (id,workspace_id,actor_user_id,actor_session_id, \
             operation_type,entity_type,entity_id,base_revision,result_revision,outcome) \
             VALUES ($1,$2,$3,$4,'proposal.accepted','proposal',$5,$6,$7,'succeeded')",
        )
        .bind(Uuid::new_v4())
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(actor_session_id)
        .bind(proposal.id)
        .bind(revision_i64(proposal.revision)?)
        .bind(revision_i64(accepted.revision)?)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_request_receipt(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    operation: &str,
    key_hash: [u8; 32],
    request_hash: [u8; 32],
    application_id: Uuid,
    completed_at: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "INSERT INTO proposal_application_requests (workspace_id,user_id,operation,key_hash, \
         request_hash,application_id,completed_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(operation)
    .bind(key_hash.as_slice())
    .bind(request_hash.as_slice())
    .bind(application_id)
    .bind(completed_at)
    .execute(&mut **transaction)
    .await
    .map_err(map_request_insert_error)?;
    Ok(())
}

async fn insert_application_outbox(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    revision: u64,
    event_type: &str,
) -> Result<(), ProposalApplicationError> {
    sqlx::query(
        "INSERT INTO outbox_messages (id,workspace_id,aggregate_type,aggregate_id, \
         aggregate_revision,event_type,deduplication_key,payload) \
         VALUES ($1,$2,'proposal_application',$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(application_id)
    .bind(revision_i64(revision)?)
    .bind(event_type)
    .bind(format!("{event_type}:{application_id}:{revision}"))
    .bind(json!({
        "application_id": application_id,
        "revision": revision,
    }))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn load_receipt_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
) -> Result<ProposalApplicationReceipt, ProposalApplicationError> {
    let row = sqlx::query(
        "SELECT preview_id,status,revision,applied_at,undo_expires_at,undone_at \
         FROM proposal_applications WHERE workspace_id=$1 AND user_id=$2 AND id=$3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ProposalApplicationError::NotFound)?;
    let preview_id: Uuid = row.try_get("preview_id").map_err(internal)?;
    let status = parse_application_status(&row.try_get::<String, _>("status").map_err(internal)?)?;
    let revision = revision_u64(row.try_get::<i16, _>("revision").map_err(internal)?)?;
    let member_rows = sqlx::query(
        "SELECT member.proposal_id,preview_member.proposal_revision \
         FROM proposal_application_members AS member \
         JOIN proposal_apply_preview_members AS preview_member \
           ON preview_member.workspace_id=member.workspace_id \
          AND preview_member.user_id=member.user_id \
          AND preview_member.preview_id=$3 \
          AND preview_member.ordinal=member.ordinal \
          AND preview_member.proposal_id=member.proposal_id \
         WHERE member.workspace_id=$1 AND member.user_id=$2 \
           AND member.application_id=$4 ORDER BY member.ordinal",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(preview_id)
    .bind(application_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    let proposals = member_rows
        .iter()
        .map(|member| {
            let base = revision_u64(
                member
                    .try_get::<i64, _>("proposal_revision")
                    .map_err(internal)?,
            )?;
            Ok(ProposalAppliedMember {
                proposal_id: member.try_get("proposal_id").map_err(internal)?,
                applied_revision: base
                    .checked_add(1)
                    .ok_or(ProposalApplicationError::Internal)?,
            })
        })
        .collect::<Result<Vec<_>, ProposalApplicationError>>()?;
    let command_ids = sqlx::query_scalar(
        "SELECT action_id FROM proposal_application_effects WHERE workspace_id=$1 AND user_id=$2 \
         AND application_id=$3 ORDER BY review_ordinal",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    let affected_item_ids = sqlx::query_scalar(
        "SELECT item_id FROM proposal_application_fences WHERE workspace_id=$1 AND user_id=$2 \
         AND application_id=$3 ORDER BY item_id",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(ProposalApplicationReceipt {
        application_id,
        proposals,
        application_revision: revision,
        status,
        command_ids,
        affected_item_ids,
        applied_at: row.try_get("applied_at").map_err(internal)?,
        undo_expires_at: row.try_get("undo_expires_at").map_err(internal)?,
        undone_at: row.try_get("undone_at").map_err(internal)?,
    })
}

async fn lock_application(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
) -> Result<StoredApplication, ProposalApplicationError> {
    let row = sqlx::query(
        "SELECT status,revision,effect_count,fence_count,undo_expires_at FROM proposal_applications \
         WHERE workspace_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ProposalApplicationError::NotFound)?;
    Ok(StoredApplication {
        revision: revision_u64(row.try_get::<i16, _>("revision").map_err(internal)?)?,
        status: parse_application_status(&row.try_get::<String, _>("status").map_err(internal)?)?,
        effect_count: positive_usize(row.try_get::<i16, _>("effect_count").map_err(internal)?)?,
        fence_count: positive_usize(row.try_get::<i32, _>("fence_count").map_err(internal)?)?,
        undo_expires_at: row.try_get("undo_expires_at").map_err(internal)?,
    })
}

async fn lock_receipt_application(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
) -> Result<(), ProposalApplicationError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM proposal_applications WHERE workspace_id=$1 AND user_id=$2 AND id=$3 \
         FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?
    .ok_or(ProposalApplicationError::NotFound)?;
    Ok(())
}

async fn lock_fences(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
) -> Result<Vec<StoredFence>, ProposalApplicationError> {
    let rows = sqlx::query(
        "SELECT item_id,applied_revision,applied_deleted FROM proposal_application_fences \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 ORDER BY item_id FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    rows.iter()
        .map(|row| {
            Ok(StoredFence {
                item_id: row.try_get("item_id").map_err(internal)?,
                applied_revision: revision_u64(
                    row.try_get::<i64, _>("applied_revision")
                        .map_err(internal)?,
                )?,
                applied_deleted: row.try_get("applied_deleted").map_err(internal)?,
            })
        })
        .collect()
}

async fn validate_fences(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    fences: &[StoredFence],
) -> Result<(), ProposalApplicationError> {
    for fence in fences {
        let item = fetch_item_batch_tx(transaction, scope.workspace_id, fence.item_id, true)
            .await
            .map_err(map_item_error)?;
        if item.revision != fence.applied_revision
            || item.deleted_at.is_some() != fence.applied_deleted
        {
            return Err(ProposalApplicationError::Stale(
                ProposalConflictCode::UndoDiverged,
            ));
        }
    }
    Ok(())
}

async fn load_effects_reverse(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
) -> Result<Vec<StoredEffect>, ProposalApplicationError> {
    let rows = sqlx::query(
        "SELECT operation,item_id,before_snapshot,before_snapshot_hash FROM proposal_application_effects \
         WHERE workspace_id=$1 AND user_id=$2 AND application_id=$3 ORDER BY ordinal DESC",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(internal)?;
    rows.iter()
        .map(|row| {
            let before_value: Option<Value> = row.try_get("before_snapshot").map_err(internal)?;
            let before = before_value
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| ProposalApplicationError::Internal)?;
            let before_hash: Option<Vec<u8>> =
                row.try_get("before_snapshot_hash").map_err(internal)?;
            match (&before, before_hash) {
                (Some(item), Some(hash))
                    if bytes32(hash.clone())?
                        == hash_domain_json(b"dayweave.proposal.item-snapshot.v1\0", item)? => {}
                (None, None) => {}
                _ => return Err(ProposalApplicationError::Internal),
            }
            Ok(StoredEffect {
                operation: parse_operation(
                    &row.try_get::<String, _>("operation").map_err(internal)?,
                )?,
                item_id: row.try_get("item_id").map_err(internal)?,
                before,
            })
        })
        .collect()
}

fn inverse_command(
    effect: &StoredEffect,
    current: &Item,
) -> Result<TransactionalItemCommand, ProposalApplicationError> {
    match effect.operation {
        ProposalOperation::CreateItem => Ok(TransactionalItemCommand::Trash {
            item_id: effect.item_id,
            expected_revision: current.revision,
        }),
        ProposalOperation::ReplaceItem
        | ProposalOperation::TrashItem
        | ProposalOperation::RestoreItem => {
            let before = effect
                .before
                .as_ref()
                .ok_or(ProposalApplicationError::Internal)?;
            Ok(TransactionalItemCommand::RestoreSnapshot {
                item_id: effect.item_id,
                expected_revision: current.revision,
                snapshot: Box::new(before.clone()),
            })
        }
    }
}

async fn update_fences_after_undo(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    fences: &[StoredFence],
    undo_items: &[Item],
) -> Result<(), ProposalApplicationError> {
    for fence in fences {
        let item = undo_items
            .iter()
            .find(|item| item.id == fence.item_id)
            .ok_or(ProposalApplicationError::Internal)?;
        let updated = sqlx::query(
            "UPDATE proposal_application_fences SET undo_revision=$5 WHERE workspace_id=$1 \
             AND user_id=$2 AND application_id=$3 AND item_id=$4 AND undo_revision IS NULL",
        )
        .bind(scope.workspace_id)
        .bind(scope.user_id)
        .bind(application_id)
        .bind(fence.item_id)
        .bind(revision_i64(item.revision)?)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?
        .rows_affected();
        if updated != 1 {
            return Err(ProposalApplicationError::Internal);
        }
    }
    Ok(())
}

async fn mark_application_undone(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    application_id: Uuid,
    undo_audit_id: Uuid,
    undone_at: DateTime<Utc>,
) -> Result<(), ProposalApplicationError> {
    let updated = sqlx::query(
        "UPDATE proposal_applications SET status='undone',revision=2,undo_audit_id=$4,undone_at=$5 \
         WHERE workspace_id=$1 AND user_id=$2 AND id=$3 AND status='applied' AND revision=1",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(application_id)
    .bind(undo_audit_id)
    .bind(undone_at)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 1 {
        Ok(())
    } else {
        Err(ProposalApplicationError::Stale(
            ProposalConflictCode::UndoDiverged,
        ))
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), ProposalApplicationError> {
    if (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
    {
        Ok(())
    } else {
        Err(validation(
            "Idempotency-Key must be 8-128 URL-safe ASCII characters",
        ))
    }
}

const fn operation_name(operation: ProposalOperation) -> &'static str {
    match operation {
        ProposalOperation::CreateItem => "create_item",
        ProposalOperation::ReplaceItem => "replace_item",
        ProposalOperation::TrashItem => "trash_item",
        ProposalOperation::RestoreItem => "restore_item",
    }
}

fn parse_operation(value: &str) -> Result<ProposalOperation, ProposalApplicationError> {
    match value {
        "create_item" => Ok(ProposalOperation::CreateItem),
        "replace_item" => Ok(ProposalOperation::ReplaceItem),
        "trash_item" => Ok(ProposalOperation::TrashItem),
        "restore_item" => Ok(ProposalOperation::RestoreItem),
        _ => Err(ProposalApplicationError::Internal),
    }
}

fn parse_application_status(
    value: &str,
) -> Result<ProposalApplicationStatus, ProposalApplicationError> {
    match value {
        "applied" => Ok(ProposalApplicationStatus::Applied),
        "undone" => Ok(ProposalApplicationStatus::Undone),
        _ => Err(ProposalApplicationError::Internal),
    }
}

fn revision_i64(value: u64) -> Result<i64, ProposalApplicationError> {
    i64::try_from(value).map_err(|_| ProposalApplicationError::Internal)
}

fn revision_u64<T>(value: T) -> Result<u64, ProposalApplicationError>
where
    T: TryInto<u64>,
{
    value
        .try_into()
        .map_err(|_| ProposalApplicationError::Internal)
}

fn positive_usize<T>(value: T) -> Result<usize, ProposalApplicationError>
where
    T: TryInto<usize>,
{
    let value = value
        .try_into()
        .map_err(|_| ProposalApplicationError::Internal)?;
    if value == 0 {
        Err(ProposalApplicationError::Internal)
    } else {
        Ok(value)
    }
}

fn bytes32(value: Vec<u8>) -> Result<[u8; 32], ProposalApplicationError> {
    value
        .try_into()
        .map_err(|_| ProposalApplicationError::Internal)
}

#[allow(clippy::needless_pass_by_value)] // Used directly as a `Result::map_err` adapter.
fn map_item_error(error: ItemRepositoryError) -> ProposalApplicationError {
    match error {
        ItemRepositoryError::RevisionConflict { expected, actual } => {
            ProposalApplicationError::RevisionConflict { expected, actual }
        }
        ItemRepositoryError::NotFound(_) => ProposalApplicationError::NotFound,
        ItemRepositoryError::Duplicate(_) => {
            ProposalApplicationError::Stale(ProposalConflictCode::ItemAlreadyExists)
        }
        ItemRepositoryError::ParentNotFound(_) => {
            ProposalApplicationError::Stale(ProposalConflictCode::ParentNotFound)
        }
        ItemRepositoryError::HierarchyCycle | ItemRepositoryError::SelfParent => {
            ProposalApplicationError::Stale(ProposalConflictCode::HierarchyCycle)
        }
        ItemRepositoryError::DependencyNotFound(_) => {
            ProposalApplicationError::Stale(ProposalConflictCode::DependencyNotFound)
        }
        ItemRepositoryError::DependencyCycle => {
            ProposalApplicationError::Stale(ProposalConflictCode::DependencyCycle)
        }
        ItemRepositoryError::InvalidParentState => {
            ProposalApplicationError::Stale(ProposalConflictCode::InvalidParentState)
        }
        ItemRepositoryError::NonLeafExecutable => {
            ProposalApplicationError::Stale(ProposalConflictCode::NonLeafExecutable)
        }
        ItemRepositoryError::ActiveExecutionConflict { .. }
        | ItemRepositoryError::CrossRecurringSubtreeDependency { .. }
        | ItemRepositoryError::InvalidItem(_) => {
            ProposalApplicationError::Stale(ProposalConflictCode::InvalidItem)
        }
        ItemRepositoryError::HasChildren => {
            ProposalApplicationError::Stale(ProposalConflictCode::HasChildren)
        }
        ItemRepositoryError::DeletedParent => {
            ProposalApplicationError::Stale(ProposalConflictCode::DeletedParent)
        }
        _ => ProposalApplicationError::Internal,
    }
}

#[allow(clippy::needless_pass_by_value)] // Used directly as a `Result::map_err` adapter.
fn map_preview_insert_error(error: sqlx::Error) -> ProposalApplicationError {
    if is_constraint_error(&error) {
        ProposalApplicationError::Stale(ProposalConflictCode::ProposalRevisionMismatch)
    } else {
        ProposalApplicationError::Internal
    }
}

#[allow(clippy::needless_pass_by_value)] // Used directly as a `Result::map_err` adapter.
fn map_application_insert_error(error: sqlx::Error) -> ProposalApplicationError {
    if is_unique_violation(&error) {
        ProposalApplicationError::Stale(ProposalConflictCode::AlreadyApplied)
    } else {
        ProposalApplicationError::Internal
    }
}

#[allow(clippy::needless_pass_by_value)] // Used directly as a `Result::map_err` adapter.
fn map_request_insert_error(error: sqlx::Error) -> ProposalApplicationError {
    if is_unique_violation(&error) {
        ProposalApplicationError::IdempotencyConflict
    } else {
        ProposalApplicationError::Internal
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

fn is_constraint_error(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| matches!(code.as_ref(), "23503" | "23505" | "23514"))
}

fn validation(message: impl Into<String>) -> ProposalApplicationError {
    ProposalApplicationError::Validation(message.into())
}

fn internal<T>(_error: T) -> ProposalApplicationError {
    ProposalApplicationError::Internal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::{ItemKind, ItemStatus, NewItem, ReplaceItem, SplitPolicy};

    fn review_item(id: Uuid, flexible_constraints: Value) -> Item {
        Item::new(
            NewItem {
                id,
                is_sensitive: false,
                kind: ItemKind::Task,
                status: ItemStatus::Planned,
                title: "Dependency review fixture".to_owned(),
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
                flexible_constraints,
                has_own_effort: None,
                split_policy: SplitPolicy::Indivisible,
                importance: 50,
                urgency: 50,
                parent_id: None,
                sibling_order: 0,
                blocked_reason_kind: None,
                blocked_by_item_id: None,
                blocked_reason: None,
            },
            "2026-09-04T12:00:00Z".parse().unwrap(),
        )
        .expect("review fixture must be valid")
    }

    fn dependency_constraints(predecessor_id: Uuid) -> Value {
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

    fn hierarchy_review_item(
        id: Uuid,
        kind: ItemKind,
        status: ItemStatus,
        parent_id: Option<Uuid>,
        sibling_order: u32,
        flexible_constraints: Value,
    ) -> Item {
        Item::new(
            NewItem {
                id,
                is_sensitive: false,
                kind,
                status,
                title: format!("Hierarchy ordering fixture {id}"),
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
                flexible_constraints,
                has_own_effort: None,
                split_policy: SplitPolicy::Indivisible,
                importance: 50,
                urgency: 50,
                parent_id,
                sibling_order,
                blocked_reason_kind: None,
                blocked_by_item_id: None,
                blocked_reason: None,
            },
            "2026-09-04T12:00:00Z".parse().unwrap(),
        )
        .expect("hierarchy fixture must be valid")
    }

    fn hierarchy_replacement(
        item: &Item,
        status: ItemStatus,
        parent_id: Option<Uuid>,
        sibling_order: u32,
        flexible_constraints: Value,
    ) -> ReplaceItem {
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
            flexible_constraints,
            has_own_effort: Some(item.has_own_effort),
            split_policy: item.split_policy.clone(),
            importance: item.importance,
            urgency: item.urgency,
            parent_id,
            sibling_order,
            blocked_reason_kind: item.blocked_reason_kind,
            blocked_by_item_id: item.blocked_by_item_id,
            blocked_reason: item.blocked_reason.clone(),
        }
    }

    fn dependencies(predecessors: &[Uuid]) -> Value {
        let dependencies = predecessors
            .iter()
            .map(|item_id| {
                json!({
                    "item_id": item_id,
                    "relation": "finish_to_start",
                    "minimum_lag": 15,
                    "strength": { "level": "hard" }
                })
            })
            .collect::<Vec<_>>();
        json!({"constraints": {"dependencies": dependencies}})
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn command_order_backtracks_when_the_first_feasible_branch_dead_ends() {
        let ids = (1_u128..=6).map(Uuid::from_u128).collect::<Vec<_>>();
        let mut ordered_routine_with_dependency = dependencies(&[ids[3]]);
        ordered_routine_with_dependency
            .as_object_mut()
            .unwrap()
            .insert("routine_ordered".to_owned(), Value::Bool(true));
        let initial = [
            hierarchy_review_item(
                ids[0],
                ItemKind::Task,
                ItemStatus::Scheduled,
                Some(ids[5]),
                2,
                json!({}),
            ),
            hierarchy_review_item(
                ids[1],
                ItemKind::Task,
                ItemStatus::Planned,
                Some(ids[5]),
                1,
                json!({}),
            ),
            hierarchy_review_item(
                ids[2],
                ItemKind::Task,
                ItemStatus::Planned,
                Some(ids[1]),
                2,
                json!({}),
            ),
            hierarchy_review_item(
                ids[3],
                ItemKind::Task,
                ItemStatus::Planned,
                Some(ids[5]),
                1,
                json!({}),
            ),
            hierarchy_review_item(
                ids[4],
                ItemKind::Routine,
                ItemStatus::Planned,
                Some(ids[2]),
                3,
                ordered_routine_with_dependency,
            ),
            hierarchy_review_item(
                ids[5],
                ItemKind::Routine,
                ItemStatus::Planned,
                None,
                2,
                json!({"routine_ordered": true}),
            ),
        ];
        let initial_by_id = initial
            .iter()
            .cloned()
            .map(|item| (item.id, item))
            .collect::<HashMap<_, _>>();
        let commands = vec![
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: ids[0],
                expected_revision: 1,
                item: hierarchy_replacement(
                    &initial[0],
                    ItemStatus::Planned,
                    Some(ids[1]),
                    2,
                    json!({}),
                ),
            },
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: ids[1],
                expected_revision: 1,
                item: hierarchy_replacement(
                    &initial[1],
                    ItemStatus::Planned,
                    Some(ids[3]),
                    0,
                    json!({}),
                ),
            },
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: ids[2],
                expected_revision: 1,
                item: hierarchy_replacement(
                    &initial[2],
                    ItemStatus::Planned,
                    Some(ids[5]),
                    1,
                    dependencies(&[ids[0], ids[3], ids[5]]),
                ),
            },
            ProposalCommand::ReplaceItem {
                command_id: Uuid::new_v4(),
                item_id: ids[3],
                expected_revision: 1,
                item: hierarchy_replacement(
                    &initial[3],
                    ItemStatus::Planned,
                    Some(ids[2]),
                    3,
                    json!({}),
                ),
            },
        ];

        assert_eq!(
            command_execution_order(
                &commands,
                &initial_by_id,
                "2026-09-04T12:05:00Z".parse().unwrap(),
            )
            .expect("fallback finds the valid projected sequence"),
            vec![0, 3, 2, 1]
        );
    }

    #[test]
    fn dependency_changes_have_a_distinct_diff_field_and_high_risk() {
        let item_id = Uuid::new_v4();
        let before = review_item(item_id, json!({}));
        let after = review_item(item_id, dependency_constraints(Uuid::new_v4()));

        assert_eq!(
            changed_fields(Some(&before), Some(&after)),
            vec![ProposalItemField::Dependencies],
            "the normalized graph is reviewed separately from other scheduling metadata"
        );

        let command = ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item: NewItem {
                id: after.id,
                is_sensitive: after.is_sensitive,
                kind: after.kind,
                status: after.status,
                title: after.title.clone(),
                notes: after.notes.clone(),
                timezone_name: after.timezone_name.clone(),
                duration_kind: Some(after.duration_kind),
                duration_seconds: after.duration_seconds,
                duration_min_seconds: after.duration_min_seconds,
                duration_max_seconds: after.duration_max_seconds,
                duration_source: after.duration_source,
                deadline_kind: Some(after.deadline_kind),
                deadline_date: after.deadline_date,
                deadline_at: after.deadline_at,
                deadline_strength: after.deadline_strength,
                deadline_soft_weight: after.deadline_soft_weight,
                earliest_start_at: after.earliest_start_at,
                recurrence: after.recurrence.clone(),
                flexible_constraints: after.flexible_constraints.clone(),
                has_own_effort: Some(after.has_own_effort),
                split_policy: after.split_policy.clone(),
                importance: after.importance,
                urgency: after.urgency,
                parent_id: after.parent_id,
                sibling_order: after.sibling_order,
                blocked_reason_kind: after.blocked_reason_kind,
                blocked_by_item_id: after.blocked_by_item_id,
                blocked_reason: after.blocked_reason.clone(),
            },
        };
        let risks = proposal_risks(
            std::slice::from_ref(&command),
            &[TransactionalItemEffect {
                before: None,
                after,
                execution_ordinal: 0,
            }],
        );
        let risk = risks
            .iter()
            .find(|risk| risk.code == ProposalRiskCode::ChangesDependencies)
            .expect("dependency changes need an explicit review risk");
        assert_eq!(risk.level, ProposalRiskLevel::High);
        assert!(risk.requires_explicit_approval);
        assert_eq!(risk.command_id, Some(command.command_id()));

        let missing = item_conflict(
            &command,
            &ItemRepositoryError::DependencyNotFound(Uuid::new_v4()),
        );
        assert_eq!(missing.code, ProposalConflictCode::DependencyNotFound);
        assert!(missing.summary.contains("predecessor"));
        let cycle = item_conflict(&command, &ItemRepositoryError::DependencyCycle);
        assert_eq!(cycle.code, ProposalConflictCode::DependencyCycle);
        assert!(cycle.summary.contains("cycle"));
        let recurring_boundary = item_conflict(
            &command,
            &ItemRepositoryError::CrossRecurringSubtreeDependency {
                successor_id: command.target_item_id(),
                predecessor_id: Uuid::new_v4(),
            },
        );
        assert_eq!(recurring_boundary.code, ProposalConflictCode::InvalidItem);
        assert!(recurring_boundary.summary.contains("recurring subtree"));
    }

    #[test]
    fn deadline_risk_classification_uses_the_typed_shape_and_partial_order() {
        let first_instant = Some("2026-09-04T09:00:00Z".parse().unwrap());
        let second_instant = Some("2026-09-04T10:00:00Z".parse().unwrap());
        let no_deadline_before = DeadlineRiskShape::new(
            DeadlineKind::None,
            None,
            first_instant,
            Some(DeadlineStrength::Hard),
            None,
        );
        let no_deadline_after = DeadlineRiskShape::new(
            DeadlineKind::None,
            None,
            second_instant,
            Some(DeadlineStrength::Soft),
            Some(1),
        );
        assert_eq!(
            classify_deadline_risk_change(&no_deadline_before, &no_deadline_after),
            None,
            "an event interval end is not a deadline when deadline_kind is none"
        );

        let first_date = NaiveDate::from_ymd_opt(2026, 9, 4);
        let second_date = NaiveDate::from_ymd_opt(2026, 9, 5);
        let hard_first = DeadlineRiskShape::new(
            DeadlineKind::Date,
            first_date,
            None,
            Some(DeadlineStrength::Hard),
            None,
        );
        let hard_second = DeadlineRiskShape::new(
            DeadlineKind::Date,
            second_date,
            None,
            Some(DeadlineStrength::Hard),
            None,
        );
        assert_eq!(
            classify_deadline_risk_change(&hard_first, &hard_second),
            Some(DeadlineRiskChange::Relaxed),
            "a later date-only deadline is a relaxation"
        );
        assert_eq!(
            classify_deadline_risk_change(&hard_second, &hard_first),
            Some(DeadlineRiskChange::Changed),
            "an earlier deadline is a material tightening"
        );

        let soft_high = DeadlineRiskShape::new(
            DeadlineKind::Date,
            first_date,
            None,
            Some(DeadlineStrength::Soft),
            Some(90),
        );
        let soft_low = DeadlineRiskShape::new(
            DeadlineKind::Date,
            first_date,
            None,
            Some(DeadlineStrength::Soft),
            Some(20),
        );
        assert_eq!(
            classify_deadline_risk_change(&soft_high, &soft_low),
            Some(DeadlineRiskChange::Relaxed),
            "lowering a soft-deadline weight is a relaxation"
        );
        assert_eq!(
            classify_deadline_risk_change(&hard_first, &soft_low),
            Some(DeadlineRiskChange::Relaxed),
            "changing a hard deadline to soft is a relaxation"
        );

        let hard_later = DeadlineRiskShape::new(
            DeadlineKind::Date,
            second_date,
            None,
            Some(DeadlineStrength::Hard),
            None,
        );
        assert_eq!(
            classify_deadline_risk_change(&soft_high, &hard_later),
            Some(DeadlineRiskChange::Changed),
            "mixed tightening and relaxation is not mislabeled as a pure relaxation"
        );
    }
}
