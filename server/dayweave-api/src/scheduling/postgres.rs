use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use dayweave_core::{ExplanationCode, ScheduleBlockKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{
    persistence::{
        DatabaseScope, fetch_item_batch_tx, insert_proposal_tx, lock_canonical_item_space,
        lock_execution_and_canonical_item_space, proposal_from_row,
    },
    proposals::{PROPOSAL_CHANGE_SET_SCHEMA_V1, ProposalCommand},
};

use super::{
    CalendarProjectionFenceError, CalendarProjectionStamp, ComposeScheduleResult, ConflictQuery,
    ConflictReport, ItemSearchQuery, ItemSearchResult, ItemSummary, PlacementAlternative,
    PlacementExplanation, PlacementReason, PlanOperationKind, PlanningSimulationPort,
    ProposalSubmissionError, ProposalSubmissionPort, ProposalSubmissionResult,
    ProposalSubmissionSpec, SCHEDULER_PUBLICATION_SCHEMA, ScheduleAccess, ScheduleBlockView,
    ScheduleConflict, ScheduleDetail, ScheduleQuery, ScheduleQueryPort, ScheduleView,
    SchedulingPortError, SimulatedBlockMove, SimulationConsumption, SimulationIssue,
    SimulationProposalEvidence, SimulationRequest, SimulationResult,
    has_postgres_timestamp_precision, materialize_proposal,
    proposal_bridge::{
        OperationCompilation, RequestCompilation, classify_request, compile_operation,
        finish_evidence, parent_item_id, target_item_id,
    },
    simulation_request_digest, simulation_request_hash,
};

const MAX_CANONICAL_ITEMS: usize = 10_000;
const CALENDAR_PROJECTION_MAX_AGE_MINUTES: i64 = 30;
const MAX_SIMULATION_BYTES: usize = 1024 * 1024;
const MAX_ACTIVE_SIMULATIONS: i64 = 256;
const SIMULATION_TTL: Duration = Duration::minutes(15);
const SIMULATION_MAINTENANCE_INTERVAL: StdDuration = StdDuration::from_hours(1);

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PublishedScheduleRevision {
    pub id: Uuid,
    pub revision: String,
    pub revision_number: u64,
    pub input_digest: String,
    pub horizon_start: DateTime<Utc>,
    pub horizon_end: DateTime<Utc>,
    pub timezone_name: String,
    pub published_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SchedulePublication {
    pub revision: PublishedScheduleRevision,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub struct PublishScheduleSpec {
    pub idempotency_key: Uuid,
    pub request_hash: [u8; 32],
    pub input_digest: [u8; 32],
    pub timezone_name: String,
    pub result: ComposeScheduleResult,
    pub published_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequiredDeferredPlacement {
    deferred_session_id: Uuid,
    source_block_id: Uuid,
    item_id: Uuid,
    item_revision: i64,
    occurrence_id: Option<Uuid>,
    session_index: i32,
    move_start: DateTime<Utc>,
    move_end: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SchedulePublicationError {
    #[error("the authenticated principal does not own this schedule scope")]
    AccessDenied,
    #[error("the idempotency key was already used for different publication content")]
    IdempotencyConflict,
    #[error("the canonical item snapshot changed before publication")]
    StaleComposition,
    #[error("an overlapping execution defer requires its exact pinned placement")]
    DeferredPlacementRequired,
    #[error("the schedule publication payload is invalid")]
    InvalidPayload,
    #[error("schedule publication storage is unavailable")]
    Unavailable,
}

/// Durable adapter for the one configured personal workspace and owner.
#[derive(Clone)]
pub struct PostgresSchedulingRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl std::fmt::Debug for PostgresSchedulingRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresSchedulingRepository")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl PostgresSchedulingRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }

    /// Removes consumed or expired hidden simulation evidence.
    ///
    /// # Errors
    ///
    /// Returns [`SchedulingPortError::Unavailable`] if the scoped maintenance
    /// transaction cannot complete.
    pub async fn maintain_simulation_retention(&self) -> Result<(), SchedulingPortError> {
        let mut transaction = self.pool.begin().await.map_err(storage_port)?;
        lock_owner(&mut transaction, self.scope)
            .await
            .map_err(storage_port)?;
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage_port)?;
        prune_simulations(&mut transaction, self.scope, now).await?;
        transaction.commit().await.map_err(storage_port)
    }

    pub(crate) fn spawn_simulation_maintenance_worker(self: &Arc<Self>) {
        let repository = Arc::clone(self);
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + SIMULATION_MAINTENANCE_INTERVAL;
            let mut interval = tokio::time::interval_at(start, SIMULATION_MAINTENANCE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = repository.maintain_simulation_retention().await {
                    tracing::warn!(%error, "simulation retention maintenance failed");
                }
            }
        });
    }

    /// Captures content-free generation evidence for every selected Calendar
    /// that can reserve schedule capacity.
    ///
    /// A selected collection is unusable until one complete expanded-event
    /// generation covers the whole requested horizon under its current
    /// configuration revision. Read-only/context-only calendars do not form a
    /// capacity-safety fence.
    ///
    /// # Errors
    ///
    /// Returns [`CalendarProjectionFenceError::Incomplete`] if any required
    /// collection is uninitialized, failed, stale or only partially covers the
    /// horizon. Storage failures are redacted as `Unavailable`.
    #[allow(clippy::too_many_lines)] // The query and typed fail-closed validation form one fence.
    pub(crate) async fn calendar_projection_stamps(
        &self,
        horizon_start: DateTime<Utc>,
        horizon_end: DateTime<Utc>,
    ) -> Result<Vec<CalendarProjectionStamp>, CalendarProjectionFenceError> {
        if horizon_start >= horizon_end {
            return Err(CalendarProjectionFenceError::Incomplete);
        }
        let rows = sqlx::query(
            "SELECT collection.id, collection.revision, collection.planning_projection_state, \
             collection.planning_generation, collection.planning_collection_revision, \
             collection.planning_window_start, collection.planning_window_end, \
             collection.planning_window_refreshed_at, statement_timestamp() AS observed_at, \
             (account.status = 'active' AND account.sync_enabled \
              AND account.tombstoned_at IS NULL AND collection.selected \
              AND NOT collection.provider_deleted \
              AND collection.sync_role IN ('blocking', 'writable') \
              AND (collection.confirmed_busy_policy = 'blocking' \
                   OR collection.tentative_policy = 'blocking' \
                   OR collection.free_policy = 'blocking' \
                   OR collection.all_day_policy = 'blocking')) AS configuration_requires_projection, \
             EXISTS(SELECT 1 FROM provider_sync_mappings mapping JOIN items item \
               ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
               WHERE mapping.workspace_id = collection.workspace_id \
                 AND mapping.provider_account_id = collection.provider_account_id \
                 AND mapping.collection_id = collection.id \
                 AND mapping.entity_kind = 'calendar_occurrence' \
                 AND mapping.tombstoned_at IS NULL AND item.trashed_at IS NULL \
                 AND item.scheduling_constraints ? 'calendar_event') AS has_active_blocking_occurrence \
             FROM google_sync_collections collection JOIN provider_accounts account \
               ON account.workspace_id = collection.workspace_id \
              AND account.user_id = collection.user_id \
              AND account.id = collection.provider_account_id \
             WHERE collection.workspace_id = $1 AND collection.user_id = $2 \
               AND collection.collection_kind = 'calendar' \
               AND ((account.status = 'active' AND account.sync_enabled \
                     AND account.tombstoned_at IS NULL AND collection.selected \
                     AND NOT collection.provider_deleted \
                     AND collection.sync_role IN ('blocking', 'writable') \
                     AND (collection.confirmed_busy_policy = 'blocking' \
                          OR collection.tentative_policy = 'blocking' \
                          OR collection.free_policy = 'blocking' \
                          OR collection.all_day_policy = 'blocking')) \
                    OR EXISTS(SELECT 1 FROM provider_sync_mappings mapping JOIN items item \
                      ON item.workspace_id = mapping.workspace_id \
                     AND item.id = mapping.local_entity_id \
                      WHERE mapping.workspace_id = collection.workspace_id \
                        AND mapping.provider_account_id = collection.provider_account_id \
                        AND mapping.collection_id = collection.id \
                        AND mapping.entity_kind = 'calendar_occurrence' \
                        AND mapping.tombstoned_at IS NULL AND item.trashed_at IS NULL \
                        AND item.scheduling_constraints ? 'calendar_event')) \
             ORDER BY collection.id",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CalendarProjectionFenceError::Unavailable)?;

        let mut stamps = Vec::with_capacity(rows.len());
        for row in rows {
            let configuration_requires_projection: bool = row
                .try_get("configuration_requires_projection")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let has_active_blocking_occurrence: bool = row
                .try_get("has_active_blocking_occurrence")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            if !configuration_requires_projection && has_active_blocking_occurrence {
                return Err(CalendarProjectionFenceError::Incomplete);
            }
            let collection_id = row
                .try_get("id")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let revision: i64 = row
                .try_get("revision")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let generation: i64 = row
                .try_get("planning_generation")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let projection_revision: Option<i64> = row
                .try_get("planning_collection_revision")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let window_start: Option<DateTime<Utc>> = row
                .try_get("planning_window_start")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let window_end: Option<DateTime<Utc>> = row
                .try_get("planning_window_end")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let refreshed_at: Option<DateTime<Utc>> =
                row.try_get("planning_window_refreshed_at")
                    .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let observed_at: DateTime<Utc> = row
                .try_get("observed_at")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?;
            let complete = row
                .try_get::<String, _>("planning_projection_state")
                .map_err(|_| CalendarProjectionFenceError::Unavailable)?
                == "complete";
            let (Some(window_start), Some(window_end), Some(refreshed_at)) =
                (window_start, window_end, refreshed_at)
            else {
                return Err(CalendarProjectionFenceError::Incomplete);
            };
            if !complete
                || generation <= 0
                || projection_revision != Some(revision)
                || window_start > horizon_start
                || window_end < horizon_end
                || refreshed_at > observed_at
                || refreshed_at
                    < observed_at - Duration::minutes(CALENDAR_PROJECTION_MAX_AGE_MINUTES)
            {
                return Err(CalendarProjectionFenceError::Incomplete);
            }
            stamps.push(CalendarProjectionStamp {
                collection_id,
                collection_revision: u64::try_from(revision)
                    .map_err(|_| CalendarProjectionFenceError::Unavailable)?,
                generation: u64::try_from(generation)
                    .map_err(|_| CalendarProjectionFenceError::Unavailable)?,
                window_start,
                window_end,
                refreshed_at,
            });
        }
        Ok(stamps)
    }

    /// Looks up a durable publication receipt before expensive recomposition.
    /// An old exact retry succeeds even if that revision is now superseded.
    ///
    /// # Errors
    ///
    /// Returns an access error for a principal outside the configured owner
    /// scope, an idempotency conflict when the key belongs to a different
    /// request, or an unavailable error when durable evidence cannot be read.
    pub async fn publication_receipt(
        &self,
        access: &ScheduleAccess,
        idempotency_key: Uuid,
        request_hash: &[u8; 32],
    ) -> Result<Option<SchedulePublication>, SchedulePublicationError> {
        self.require_access(access)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        lock_execution_and_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        lock_owner(&mut transaction, self.scope)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let receipt =
            publication_receipt_tx(&mut transaction, self.scope, idempotency_key, request_hash)
                .await?;
        transaction
            .commit()
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        Ok(receipt)
    }

    /// Atomically publishes one immutable schedule revision, or returns the
    /// original receipt for an exact concurrent/retried idempotency key.
    ///
    /// # Errors
    ///
    /// Returns an access, validation, idempotency, or stale-composition error
    /// without committing a partial publication. Storage failures are reported
    /// as unavailable without exposing database details.
    #[allow(clippy::too_many_lines)] // Keeps the publication transaction and all integrity fences together.
    pub async fn publish(
        &self,
        access: &ScheduleAccess,
        spec: PublishScheduleSpec,
    ) -> Result<SchedulePublication, SchedulePublicationError> {
        self.require_access(access)?;
        let expected_digest = decode_prefixed_sha256(&spec.result.input_digest)
            .ok_or(SchedulePublicationError::InvalidPayload)?;
        if expected_digest != spec.input_digest
            || spec.result.plan.horizon_end <= spec.result.plan.horizon_start
            || spec.timezone_name.trim().is_empty()
            || spec.timezone_name.len() > 100
            || spec.timezone_name.parse::<chrono_tz::Tz>().is_err()
        {
            return Err(SchedulePublicationError::InvalidPayload);
        }
        let (publication_hash, snapshot) =
            validate_publishable_compose_result(&spec.timezone_name, &spec.result)?;
        let horizon_start = offset_to_chrono(spec.result.plan.horizon_start)?;
        let horizon_end = offset_to_chrono(spec.result.plan.horizon_end)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;

        // Execution Start owns this mutex before it enters canonical item
        // space. Publication follows the same order so a defer cannot race a
        // schedule seal that omits or changes its promised placement.
        lock_execution_and_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        lock_owner(&mut transaction, self.scope)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;

        // Historical receipts remain authoritative after the durable owner
        // fence, but before any fresh item, Calendar, or defer validation.
        if let Some(replayed) = publication_receipt_tx(
            &mut transaction,
            self.scope,
            spec.idempotency_key,
            &spec.request_hash,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(|_| SchedulePublicationError::Unavailable)?;
            return Ok(replayed);
        }

        assert_current_item_snapshot(
            &mut transaction,
            self.scope,
            &spec.result.source_item_revisions,
        )
        .await?;
        let deferred_placements = required_deferred_placements_tx(
            &mut transaction,
            self.scope,
            horizon_start,
            horizon_end,
            &spec.result,
        )
        .await?;
        assert_current_calendar_projection(
            &mut transaction,
            self.scope,
            horizon_start,
            horizon_end,
            &spec.result.calendar_projection_stamps,
        )
        .await?;

        let current = sqlx::query(
            "SELECT id, revision_number, input_digest, publication_hash, horizon_start, horizon_end, \
             timezone_name, published_at FROM schedule_revisions \
             WHERE workspace_id = $1 AND created_by_user_id = $2 AND state = 'published' \
             FOR UPDATE",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;
        let parent_id = current
            .as_ref()
            .map(|row| row.try_get::<Uuid, _>("id"))
            .transpose()
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let parent_revision_number = current
            .as_ref()
            .map(|row| row.try_get::<i64, _>("revision_number"))
            .transpose()
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let database_now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let current_published_at = current
            .as_ref()
            .map(|row| row.try_get::<Option<DateTime<Utc>>, _>("published_at"))
            .transpose()
            .map_err(|_| SchedulePublicationError::Unavailable)?
            .flatten();
        // The caller timestamp is informational only. Publication time is
        // captured after both serialization locks and cannot move behind the
        // currently published revision if the host clock steps backwards.
        let published_at = current_published_at
            .map_or(database_now, |current| std::cmp::max(current, database_now));

        if let Some(current) = current.as_ref() {
            let stored_publication_hash: Option<Vec<u8>> = current
                .try_get("publication_hash")
                .map_err(|_| SchedulePublicationError::Unavailable)?;
            if stored_publication_hash.as_deref() == Some(publication_hash.as_slice())
                && revision_has_deferred_placements_tx(
                    &mut transaction,
                    self.scope.workspace_id,
                    parent_id.ok_or(SchedulePublicationError::Unavailable)?,
                    &deferred_placements,
                )
                .await?
            {
                bind_publication_key_tx(
                    &mut transaction,
                    self.scope,
                    spec.idempotency_key,
                    &spec.request_hash,
                    parent_id.ok_or(SchedulePublicationError::Unavailable)?,
                    published_at,
                )
                .await?;
                let revision = revision_from_row(current)?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| SchedulePublicationError::Unavailable)?;
                return Ok(SchedulePublication {
                    revision,
                    replayed: false,
                });
            }
        }

        let next_revision: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision_number), 0) + 1 FROM schedule_revisions \
             WHERE workspace_id = $1",
        )
        .bind(self.scope.workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;
        if next_revision <= 0 {
            return Err(SchedulePublicationError::Unavailable);
        }

        let revision_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO schedule_revisions (id, workspace_id, revision_number, \
             parent_revision_id, state, horizon_start, horizon_end, timezone_name, solver_version, \
             input_digest, publication_hash, created_by_user_id, created_at, published_at) \
             VALUES ($1, $2, $3, $4, 'draft', $5, $6, $7, $8, $9, $10, $11, $12, NULL)",
        )
        .bind(revision_id)
        .bind(self.scope.workspace_id)
        .bind(next_revision)
        .bind(parent_id)
        .bind(horizon_start)
        .bind(horizon_end)
        .bind(&spec.timezone_name)
        .bind(SCHEDULER_PUBLICATION_SCHEMA)
        .bind(spec.input_digest.as_slice())
        .bind(publication_hash.as_slice())
        .bind(self.scope.user_id)
        .bind(published_at)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;

        for (ordinal, block) in spec.result.plan.blocks.iter().enumerate() {
            let ordinal =
                i32::try_from(ordinal).map_err(|_| SchedulePublicationError::InvalidPayload)?;
            let evidence = json!({
                "schema_version": 1,
                "source_block_id": block.id,
                "occurrence_id": block.occurrence_id,
                "external_block_id": block.external_block_id,
                "session_index": block.session_index,
                "core_kind": block.kind,
                "explanations": block.explanations,
            });
            sqlx::query(
                "INSERT INTO schedule_blocks (id, source_block_id, workspace_id, \
                 schedule_revision_id, item_id, block_kind, title_snapshot, starts_at, ends_at, \
                 timezone_name, ordinal, is_fixed, is_sensitive, constraint_snapshot) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            )
            .bind(Uuid::new_v4())
            .bind(block.id)
            .bind(self.scope.workspace_id)
            .bind(revision_id)
            .bind(block.item_id.map(|item| item.0))
            .bind(block_kind_name(block.kind))
            .bind(&block.title)
            .bind(offset_to_chrono(block.start)?)
            .bind(offset_to_chrono(block.end)?)
            .bind(&spec.timezone_name)
            .bind(ordinal)
            .bind(!matches!(block.kind, ScheduleBlockKind::Planned))
            .bind(block.is_sensitive)
            .bind(evidence)
            .execute(&mut *transaction)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        }

        insert_deferred_placements_tx(
            &mut transaction,
            self.scope.workspace_id,
            revision_id,
            &deferred_placements,
            published_at,
        )
        .await?;

        sqlx::query(
            "INSERT INTO schedule_revision_details (workspace_id, user_id, \
             schedule_revision_id, result_snapshot, created_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(revision_id)
        .bind(snapshot)
        .bind(published_at)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;

        if let Some(parent_id) = parent_id {
            let affected = sqlx::query(
                "UPDATE schedule_revisions SET state = 'superseded', superseded_at = $3 \
                 WHERE workspace_id = $1 AND id = $2 AND state = 'published'",
            )
            .bind(self.scope.workspace_id)
            .bind(parent_id)
            .bind(published_at)
            .execute(&mut *transaction)
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?
            .rows_affected();
            if affected != 1 {
                return Err(SchedulePublicationError::StaleComposition);
            }
        }
        let sealed = sqlx::query(
            "UPDATE schedule_revisions SET state = 'published', published_at = $3 \
             WHERE workspace_id = $1 AND id = $2 AND state = 'draft' \
             RETURNING id, revision_number, input_digest, horizon_start, horizon_end, \
               timezone_name, published_at",
        )
        .bind(self.scope.workspace_id)
        .bind(revision_id)
        .bind(published_at)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?
        .ok_or(SchedulePublicationError::Unavailable)?;
        let published_revision = revision_from_row(&sealed)?;
        bind_publication_key_tx(
            &mut transaction,
            self.scope,
            spec.idempotency_key,
            &spec.request_hash,
            revision_id,
            published_at,
        )
        .await?;
        sqlx::query(
            "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
             entity_type, entity_id, base_revision, result_revision, outcome, metadata, occurred_at) \
             VALUES ($1, $2, $3, 'schedule.published', 'schedule_revision', $4, $5, $6, \
             'succeeded', jsonb_build_object('idempotency_key', $7::text), $8)",
        )
        .bind(Uuid::new_v4())
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(revision_id)
        .bind(parent_revision_number)
        .bind(next_revision)
        .bind(spec.idempotency_key)
        .bind(published_at)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;

        transaction
            .commit()
            .await
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        Ok(SchedulePublication {
            revision: published_revision,
            replayed: false,
        })
    }

    fn require_access(&self, access: &ScheduleAccess) -> Result<(), SchedulePublicationError> {
        if access.workspace_id != Some(self.scope.workspace_id)
            || access.user_id != Some(self.scope.user_id)
            || access.subject.is_empty()
            || access.subject.len() > 512
            || access.subject.chars().any(char::is_control)
        {
            return Err(SchedulePublicationError::AccessDenied);
        }
        Ok(())
    }

    fn require_query_access(&self, access: &ScheduleAccess) -> Result<(), SchedulingPortError> {
        self.require_access(access)
            .map_err(|_| SchedulingPortError::NotFound)
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)] // Query privacy and redaction rules remain visibly co-located.
impl ScheduleQueryPort for PostgresSchedulingRepository {
    async fn get_schedule(
        &self,
        access: &ScheduleAccess,
        query: ScheduleQuery,
    ) -> Result<ScheduleView, SchedulingPortError> {
        self.require_query_access(access)?;
        validate_range(query.start, query.end)?;
        let revision = current_revision_pool(&self.pool, self.scope).await?;
        let rows = sqlx::query(
            "SELECT source_block_id, item_id, title_snapshot, starts_at, ends_at, \
             block_kind, is_sensitive FROM schedule_blocks \
             WHERE workspace_id = $1 AND schedule_revision_id = $2 \
               AND starts_at < $3 AND ends_at > $4 \
             ORDER BY starts_at, ends_at, ordinal, source_block_id",
        )
        .bind(self.scope.workspace_id)
        .bind(revision.id)
        .bind(query.end)
        .bind(query.start)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_port)?;
        let item_ids = rows
            .iter()
            .filter_map(|row| row.try_get::<Option<Uuid>, _>("item_id").ok().flatten())
            .collect::<BTreeSet<_>>();
        let current_sensitivity =
            current_item_sensitivity_pool(&self.pool, self.scope, &item_ids).await?;
        let mut redacted_count = 0;
        let mut blocks = Vec::with_capacity(rows.len());
        for row in rows {
            let item_id: Option<Uuid> = row.try_get("item_id").map_err(storage_port)?;
            let sensitive: bool = row
                .try_get::<bool, _>("is_sensitive")
                .map_err(storage_port)?
                || item_id.is_some_and(|id| current_sensitivity.get(&id).copied().unwrap_or(true));
            let private = sensitive && !access.include_sensitive;
            let busy_only = query.detail == ScheduleDetail::BusyOnly;
            if private {
                redacted_count += 1;
            }
            blocks.push(ScheduleBlockView {
                id: (!private && !busy_only)
                    .then(|| {
                        row.try_get::<Uuid, _>("source_block_id")
                            .map(|id| id.to_string())
                    })
                    .transpose()
                    .map_err(storage_port)?,
                item_id: (!private && query.detail == ScheduleDetail::Full)
                    .then_some(item_id)
                    .flatten()
                    .map(|id| id.to_string()),
                title: (!private && !busy_only)
                    .then(|| row.try_get::<Option<String>, _>("title_snapshot"))
                    .transpose()
                    .map_err(storage_port)?
                    .flatten(),
                start: row.try_get("starts_at").map_err(storage_port)?,
                end: row.try_get("ends_at").map_err(storage_port)?,
                kind: if private {
                    "busy".to_owned()
                } else {
                    row.try_get("block_kind").map_err(storage_port)?
                },
                status: "scheduled".to_owned(),
                redacted: private || busy_only,
            });
        }
        Ok(ScheduleView {
            revision: revision.label,
            timezone: revision.timezone_name,
            start: query.start,
            end: query.end,
            blocks,
            redacted_count,
        })
    }

    async fn search_items(
        &self,
        access: &ScheduleAccess,
        query: ItemSearchQuery,
    ) -> Result<ItemSearchResult, SchedulingPortError> {
        self.require_query_access(access)?;
        if query.limit == 0 || query.limit > 100 {
            return Err(SchedulingPortError::InvalidQuery(
                "limit must be between 1 and 100".to_owned(),
            ));
        }
        if query
            .text
            .as_ref()
            .is_some_and(|value| value.len() > 500 || value.chars().any(char::is_control))
            || [&query.status, &query.kind].into_iter().any(|value| {
                value
                    .as_ref()
                    .is_some_and(|value| value.len() > 100 || value.chars().any(char::is_control))
            })
        {
            return Err(SchedulingPortError::InvalidQuery(
                "search filters exceed the supported bounds".to_owned(),
            ));
        }
        let requested_project_id =
            normalized_uuid_filter("project_id", query.project_id.as_deref())?;
        let requested_goal_id = normalized_uuid_filter("goal_id", query.goal_id.as_deref())?;
        validate_optional_instant(query.start)?;
        validate_optional_instant(query.end)?;
        if let (Some(start), Some(end)) = (query.start, query.end) {
            validate_range(start, end)?;
        }
        let revision = current_revision_pool(&self.pool, self.scope).await?;
        let rows = search_item_rows(&self.pool, self.scope, revision.id).await?;
        if rows.len() > MAX_CANONICAL_ITEMS {
            return Err(SchedulingPortError::Unavailable(
                "canonical item count exceeds the supported limit".to_owned(),
            ));
        }
        let text = query.text.as_ref().map(|value| value.to_lowercase());
        let mut redacted_count = 0;
        let mut items = Vec::new();
        for row in rows {
            let title: String = row.try_get("title").map_err(storage_port)?;
            let status: String = row.try_get("status").map_err(storage_port)?;
            let kind: String = row.try_get("kind").map_err(storage_port)?;
            let project_id: Option<Uuid> = row.try_get("parent_item_id").map_err(storage_port)?;
            let constraints: Value = row
                .try_get("scheduling_constraints")
                .map_err(storage_port)?;
            let goal_ids = goal_ids(&constraints);
            let scheduled_start: Option<DateTime<Utc>> =
                row.try_get("scheduled_start").map_err(storage_port)?;
            let matches = text
                .as_ref()
                .is_none_or(|text| title.to_lowercase().contains(text))
                && query.status.as_ref().is_none_or(|value| value == &status)
                && query.kind.as_ref().is_none_or(|value| value == &kind)
                && requested_project_id.as_ref().is_none_or(|value| {
                    project_id.map(|id| id.to_string()).as_ref() == Some(value)
                })
                && requested_goal_id
                    .as_ref()
                    .is_none_or(|value| goal_ids.contains(value))
                && query
                    .start
                    .is_none_or(|start| scheduled_start.is_some_and(|value| value >= start))
                && query
                    .end
                    .is_none_or(|end| scheduled_start.is_some_and(|value| value < end));
            if !matches {
                continue;
            }
            let sensitive: bool = row.try_get("effective_sensitive").map_err(storage_port)?;
            if sensitive && !access.include_sensitive {
                redacted_count += 1;
                continue;
            }
            let goal_id = requested_goal_id
                .as_ref()
                .filter(|value| goal_ids.contains(*value))
                .cloned()
                .or_else(|| goal_ids.first().cloned());
            items.push(ItemSummary {
                id: row
                    .try_get::<Uuid, _>("id")
                    .map_err(storage_port)?
                    .to_string(),
                title,
                status,
                kind,
                project_id: project_id.map(|id| id.to_string()),
                goal_id,
                deadline: row.try_get("deadline_at").map_err(storage_port)?,
                scheduled_start,
            });
            if items.len() == query.limit {
                break;
            }
        }
        Ok(ItemSearchResult {
            revision: revision.label,
            items,
            redacted_count,
        })
    }

    async fn explain_placement(
        &self,
        access: &ScheduleAccess,
        block_id: &str,
    ) -> Result<PlacementExplanation, SchedulingPortError> {
        self.require_query_access(access)?;
        let block_id = Uuid::parse_str(block_id).map_err(|_| SchedulingPortError::NotFound)?;
        let revision = current_revision_pool(&self.pool, self.scope).await?;
        let row = sqlx::query(
            "SELECT constraint_snapshot, item_id, is_sensitive FROM schedule_blocks \
             WHERE workspace_id = $1 AND schedule_revision_id = $2 AND source_block_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(revision.id)
        .bind(block_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_port)?
        .ok_or(SchedulingPortError::NotFound)?;
        let item_id: Option<Uuid> = row.try_get("item_id").map_err(storage_port)?;
        let current =
            current_item_sensitivity_pool(&self.pool, self.scope, &item_id.into_iter().collect())
                .await?;
        let sensitive: bool = row
            .try_get::<bool, _>("is_sensitive")
            .map_err(storage_port)?
            || item_id.is_some_and(|id| current.get(&id).copied().unwrap_or(true));
        if sensitive && !access.include_sensitive {
            return Err(SchedulingPortError::NotFound);
        }
        let snapshot: Value = row.try_get("constraint_snapshot").map_err(storage_port)?;
        let explanations: Vec<dayweave_core::PlacementExplanation> =
            serde_json::from_value(snapshot.get("explanations").cloned().ok_or_else(|| {
                SchedulingPortError::Unavailable("placement evidence is invalid".to_owned())
            })?)
            .map_err(|_| {
                SchedulingPortError::Unavailable("placement evidence is invalid".to_owned())
            })?;
        let reasons: Vec<_> = explanations
            .iter()
            .map(|evidence| PlacementReason {
                code: explanation_code_name(evidence.code),
                message: evidence.message.clone(),
                strength: "scheduler".to_owned(),
            })
            .collect();
        let summary = if explanations.is_empty() {
            "Placed by the deterministic scheduler without additional evidence.".to_owned()
        } else {
            explanations
                .iter()
                .map(|evidence| evidence.message.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        Ok(PlacementExplanation {
            block_id: block_id.to_string(),
            summary,
            active_constraints: reasons.iter().map(|reason| reason.code.clone()).collect(),
            reasons,
            alternatives: Vec::<PlacementAlternative>::new(),
            stability_cost: 0,
            sensitive,
        })
    }

    async fn get_conflicts(
        &self,
        access: &ScheduleAccess,
        query: ConflictQuery,
    ) -> Result<ConflictReport, SchedulingPortError> {
        self.require_query_access(access)?;
        validate_range(query.start, query.end)?;
        let revision = current_revision_pool(&self.pool, self.scope).await?;
        let snapshot: Value = sqlx::query_scalar(
            "SELECT result_snapshot FROM schedule_revision_details \
             WHERE workspace_id = $1 AND user_id = $2 AND schedule_revision_id = $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(revision.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_port)?
        .ok_or(SchedulingPortError::RepublishRequired)?;
        let persisted: Vec<PersistedConflict> =
            serde_json::from_value(snapshot.get("conflicts").cloned().unwrap_or(Value::Null))
                .map_err(|_| {
                    SchedulingPortError::Unavailable("schedule evidence is invalid".to_owned())
                })?;
        let mut persisted = persisted
            .into_iter()
            .filter(|conflict| {
                conflict.start.is_none_or(|start| start < query.end)
                    && conflict.end.is_none_or(|end| end > query.start)
            })
            .collect::<Vec<_>>();
        let mut related_ids = BTreeSet::new();
        let mut corrupt_related_ids = BTreeSet::new();
        for (index, conflict) in persisted.iter().enumerate() {
            for id in &conflict.related_item_ids {
                match Uuid::parse_str(id) {
                    Ok(id) => {
                        related_ids.insert(id);
                    }
                    Err(_) => {
                        corrupt_related_ids.insert(index);
                    }
                }
            }
        }
        let current_related =
            current_item_sensitivity_pool(&self.pool, self.scope, &related_ids).await?;
        let block_rows = sqlx::query(
            "SELECT item_id, starts_at, ends_at, is_sensitive FROM schedule_blocks \
             WHERE workspace_id = $1 AND schedule_revision_id = $2 \
               AND starts_at < $3 AND ends_at > $4",
        )
        .bind(self.scope.workspace_id)
        .bind(revision.id)
        .bind(query.end)
        .bind(query.start)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_port)?;
        let block_item_ids = block_rows
            .iter()
            .filter_map(|row| row.try_get::<Option<Uuid>, _>("item_id").ok().flatten())
            .collect::<BTreeSet<_>>();
        let current_blocks =
            current_item_sensitivity_pool(&self.pool, self.scope, &block_item_ids).await?;
        let mut private_intervals = Vec::new();
        for row in block_rows {
            let item_id: Option<Uuid> = row.try_get("item_id").map_err(storage_port)?;
            let private = row
                .try_get::<bool, _>("is_sensitive")
                .map_err(storage_port)?
                || item_id.is_some_and(|id| current_blocks.get(&id).copied().unwrap_or(true));
            if private {
                private_intervals.push((
                    row.try_get::<DateTime<Utc>, _>("starts_at")
                        .map_err(storage_port)?,
                    row.try_get::<DateTime<Utc>, _>("ends_at")
                        .map_err(storage_port)?,
                ));
            }
        }
        let mut redacted_count = 0;
        let mut conflicts = Vec::new();
        for (index, mut conflict) in persisted.drain(..).enumerate() {
            let current_private = conflict
                .related_item_ids
                .iter()
                .filter_map(|id| Uuid::parse_str(id).ok())
                .any(|id| current_related.get(&id).copied().unwrap_or(true));
            let overlaps_private = conflict
                .start
                .zip(conflict.end)
                .is_some_and(|(start, end)| {
                    private_intervals
                        .iter()
                        .any(|(private_start, private_end)| {
                            *private_start < end && *private_end > start
                        })
                });
            conflict.sensitive = conflict.sensitive
                || current_private
                || corrupt_related_ids.contains(&index)
                || overlaps_private;
            if conflict.sensitive && !access.include_sensitive {
                redacted_count += 1;
            } else {
                conflicts.push(conflict.into_view());
            }
        }
        Ok(ConflictReport {
            revision: revision.label,
            conflicts,
            redacted_count,
        })
    }
}

#[allow(clippy::too_many_lines)] // Simulation persistence and its proof commitment stay auditable together.
#[async_trait]
impl PlanningSimulationPort for PostgresSchedulingRepository {
    async fn simulate(
        &self,
        access: &ScheduleAccess,
        request: SimulationRequest,
    ) -> Result<SimulationResult, SchedulingPortError> {
        self.require_query_access(access)?;
        validate_simulation_request(&request)?;
        let request_hash = simulation_request_hash(&request)?;
        let request_digest = simulation_request_digest(&request)?;
        let digest_bytes = decode_hex(&request_digest, 16).ok_or_else(|| {
            SchedulingPortError::InvalidQuery("simulation digest is invalid".to_owned())
        })?;
        let subject_hash = subject_hash(&access.subject)?;
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| {
            SchedulingPortError::Unavailable("secure randomness is unavailable".to_owned())
        })?;
        let token = format!("sim_{}", URL_SAFE_NO_PAD.encode(random));
        random.zeroize();
        let token_hash = simulation_token_hash(&token);
        let mut transaction = self.pool.begin().await.map_err(storage_port)?;
        lock_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(storage_port)?;
        lock_owner(&mut transaction, self.scope)
            .await
            .map_err(storage_port)?;
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage_port)?;
        let revision = current_revision_tx(&mut transaction, self.scope).await?;
        if request.base_revision != revision.label {
            return Err(SchedulingPortError::RevisionConflict {
                current_revision: revision.label,
            });
        }
        prune_simulations(&mut transaction, self.scope, now).await?;
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 \
             AND consumed_at IS NULL AND expires_at > $3",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_port)?;
        if active >= MAX_ACTIVE_SIMULATIONS {
            return Err(SchedulingPortError::InvalidQuery(
                "too many active simulations; consume or wait for an existing token".to_owned(),
            ));
        }
        let (result, privacy_evidence, proposal_evidence) = simulate_against_revision(
            &mut transaction,
            self.scope,
            access,
            revision.id,
            request,
            token.clone(),
            request_digest,
            now,
        )
        .await?;
        let mut snapshot = serde_json::to_value(&result).map_err(|_| {
            SchedulingPortError::Unavailable("simulation result cannot be encoded".to_owned())
        })?;
        let snapshot_object = snapshot.as_object_mut().ok_or_else(|| {
            SchedulingPortError::Unavailable("simulation result is invalid".to_owned())
        })?;
        snapshot_object.remove("simulation_token");
        snapshot_object.insert(
            "privacy_evidence".to_owned(),
            serde_json::to_value(privacy_evidence).map_err(|_| {
                SchedulingPortError::Unavailable(
                    "simulation privacy evidence cannot be encoded".to_owned(),
                )
            })?,
        );
        snapshot_object.insert(
            "proposal_evidence".to_owned(),
            serde_json::to_value(&proposal_evidence).map_err(|_| {
                SchedulingPortError::Unavailable(
                    "simulation proposal evidence cannot be encoded".to_owned(),
                )
            })?,
        );
        let encoded = serde_json::to_vec(&snapshot).map_err(|_| {
            SchedulingPortError::Unavailable("simulation result cannot be encoded".to_owned())
        })?;
        if encoded.len() > MAX_SIMULATION_BYTES {
            return Err(SchedulingPortError::InvalidQuery(
                "simulation result exceeds the supported size".to_owned(),
            ));
        }
        let simulation_id = Uuid::new_v4();
        let expires_at = now + SIMULATION_TTL;
        let compiled_payload_hash = proposal_evidence
            .change_set()
            .map(|change_set| {
                serde_json::to_value(change_set)
                    .map_err(|_| {
                        SchedulingPortError::Unavailable(
                            "compiled proposal payload cannot be encoded".to_owned(),
                        )
                    })
                    .and_then(|payload| proposal_payload_hash(&payload))
            })
            .transpose()?;
        let compilation_outcome = if compiled_payload_hash.is_some() {
            "actionable"
        } else {
            "manual_review"
        };
        let evidence_hash = simulation_evidence_hash(
            self.scope,
            simulation_id,
            &subject_hash,
            &request_hash,
            revision.id,
            &revision.label,
            now,
            expires_at,
            &snapshot,
        )?;
        sqlx::query(
            "INSERT INTO schedule_simulations (id, workspace_id, user_id, token_hash, subject_hash, \
             request_digest, base_revision_id, base_revision_label, result_snapshot, created_at, expires_at, \
             evidence_schema, request_hash, evidence_hash, compilation_outcome, compiled_payload_hash) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 1, $12, $13, $14, $15)",
        )
        .bind(simulation_id)
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(token_hash.as_slice())
        .bind(subject_hash.as_slice())
        .bind(digest_bytes)
        .bind(revision.id)
        .bind(&revision.label)
        .bind(snapshot)
        .bind(now)
        .bind(expires_at)
        .bind(request_hash.as_slice())
        .bind(evidence_hash.as_slice())
        .bind(compilation_outcome)
        .bind(compiled_payload_hash.as_ref().map(<[u8; 32]>::as_slice))
        .execute(&mut *transaction)
        .await
        .map_err(storage_port)?;
        transaction.commit().await.map_err(storage_port)?;
        Ok(result)
    }

    async fn consume_simulation(
        &self,
        access: &ScheduleAccess,
        token: &str,
        expected_request_digest: &str,
    ) -> Result<SimulationConsumption, SchedulingPortError> {
        self.require_query_access(access)?;
        validate_simulation_token(token)?;
        let expected_digest = decode_hex(expected_request_digest, 16).ok_or_else(|| {
            SchedulingPortError::InvalidQuery(
                "expected_request_digest must be 32 lowercase hexadecimal characters".to_owned(),
            )
        })?;
        let token_hash = simulation_token_hash(token);
        let subject_hash = subject_hash(&access.subject)?;
        let mut transaction = self.pool.begin().await.map_err(storage_port)?;
        lock_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(storage_port)?;
        lock_owner(&mut transaction, self.scope)
            .await
            .map_err(storage_port)?;
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage_port)?;
        let result = consume_simulation_tx(
            &mut transaction,
            self.scope,
            access,
            token,
            &token_hash,
            &subject_hash,
            &expected_digest,
            None,
            now,
        )
        .await?;
        transaction.commit().await.map_err(storage_port)?;
        Ok(result)
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)] // The single transaction's validation and commit fences stay auditable together.
impl ProposalSubmissionPort for PostgresSchedulingRepository {
    async fn submit_proposal(
        &self,
        access: &ScheduleAccess,
        spec: ProposalSubmissionSpec,
    ) -> Result<ProposalSubmissionResult, ProposalSubmissionError> {
        self.require_access(access)
            .map_err(|_| ProposalSubmissionError::AccessDenied)?;
        validate_simulation_request(&spec.request)?;
        if !(8..=128).contains(&spec.idempotency_key.len())
            || !spec.idempotency_key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(ProposalSubmissionError::Unavailable);
        }
        let expected_digest_label = simulation_request_digest(&spec.request)?;
        let expected_request_hash = simulation_request_hash(&spec.request)?;
        let expected_digest = decode_hex(&expected_digest_label, 16).ok_or_else(|| {
            ProposalSubmissionError::Simulation(SchedulingPortError::InvalidQuery(
                "expected simulation digest is invalid".to_owned(),
            ))
        })?;
        validate_simulation_token(&spec.simulation_token)?;
        let simulation_subject_hash = subject_hash(&access.subject)?;
        let receipt_subject_hash = proposal_subject_hash(&access.subject)?;
        let key_hash = proposal_idempotency_key_hash(&spec.idempotency_key);
        let token_hash = simulation_token_hash(&spec.simulation_token);
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;
        lock_canonical_item_space(&mut transaction, self.scope.workspace_id)
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;
        lock_owner(&mut transaction, self.scope)
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;

        if let Some(proposal) = proposal_submission_receipt_tx(
            &mut transaction,
            self.scope,
            &receipt_subject_hash,
            &key_hash,
            &spec.request_fingerprint,
        )
        .await?
        {
            transaction
                .commit()
                .await
                .map_err(|_| ProposalSubmissionError::Unavailable)?;
            return Ok(ProposalSubmissionResult {
                proposal,
                duplicate: true,
            });
        }

        let consumption = consume_simulation_tx(
            &mut transaction,
            self.scope,
            access,
            &spec.simulation_token,
            &token_hash,
            &simulation_subject_hash,
            &expected_digest,
            Some(&expected_request_hash),
            now,
        )
        .await?;
        if consumption.result.request_digest != expected_digest_label
            || consumption.result.base_revision != spec.request.base_revision
        {
            return Err(ProposalSubmissionError::Simulation(
                SchedulingPortError::InvalidQuery(
                    "simulation token does not match the proposal base revision".to_owned(),
                ),
            ));
        }
        let proposal =
            materialize_proposal(&access.subject, &spec, &consumption.proposal_evidence, now)?;
        let proof = consumption
            .persistence_proof
            .as_ref()
            .ok_or(ProposalSubmissionError::Unavailable)?;
        let payload_hash = proposal_payload_hash(&proposal.payload)?;
        if (proof.compilation_outcome == "actionable"
            && proof.compiled_payload_hash != Some(payload_hash))
            || (proof.compilation_outcome == "manual_review"
                && proof.compiled_payload_hash.is_some())
        {
            return Err(ProposalSubmissionError::Unavailable);
        }

        insert_proposal_tx(&mut transaction, self.scope, &proposal)
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;
        sqlx::query(
            "INSERT INTO mcp_proposal_submissions (workspace_id, user_id, subject_hash, key_hash, \
             request_fingerprint, proposal_id, completed_at, simulation_id, simulation_subject_hash, \
             simulation_request_digest, simulation_request_hash, simulation_base_revision_id, \
             simulation_created_at, simulation_expires_at, simulation_evidence_schema, \
             simulation_evidence_hash, compilation_outcome, compiled_payload_hash, \
             proposal_payload_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
             $12, $13, $14, $15, $16, $17, $18, $19)",
        )
        .bind(self.scope.workspace_id)
        .bind(self.scope.user_id)
        .bind(receipt_subject_hash.as_slice())
        .bind(key_hash.as_slice())
        .bind(spec.request_fingerprint.as_slice())
        .bind(proposal.id)
        .bind(now)
        .bind(proof.simulation_id)
        .bind(proof.subject_hash.as_slice())
        .bind(proof.request_digest.as_slice())
        .bind(proof.request_hash.as_slice())
        .bind(proof.base_revision_id)
        .bind(proof.created_at)
        .bind(proof.expires_at)
        .bind(proof.evidence_schema)
        .bind(proof.evidence_hash.as_slice())
        .bind(&proof.compilation_outcome)
        .bind(proof.compiled_payload_hash.as_ref().map(<[u8; 32]>::as_slice))
        .bind(payload_hash.as_slice())
        .execute(&mut *transaction)
        .await
        .map_err(|_| ProposalSubmissionError::Unavailable)?;
        let proposal = proposal_submission_receipt_tx(
            &mut transaction,
            self.scope,
            &receipt_subject_hash,
            &key_hash,
            &spec.request_fingerprint,
        )
        .await?
        .ok_or(ProposalSubmissionError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;
        Ok(ProposalSubmissionResult {
            proposal,
            duplicate: false,
        })
    }
}

async fn proposal_submission_receipt_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    subject_hash: &[u8; 32],
    key_hash: &[u8; 32],
    request_fingerprint: &[u8; 32],
) -> Result<Option<crate::proposals::Proposal>, ProposalSubmissionError> {
    let row = sqlx::query(
        "SELECT receipt.request_fingerprint, proposal.id, proposal.revision, \
         proposal.submitted_by_subject, proposal.source, proposal.source_reference, proposal.kind, \
         proposal.status, proposal.title, proposal.explanation, proposal.payload, \
         proposal.decision_note, proposal.created_at, proposal.updated_at, proposal.expires_at, \
         proposal.decided_at FROM mcp_proposal_submissions AS receipt \
         JOIN proposals AS proposal ON proposal.workspace_id = receipt.workspace_id \
           AND proposal.id = receipt.proposal_id \
         WHERE receipt.workspace_id = $1 AND receipt.user_id = $2 \
           AND receipt.subject_hash = $3 AND receipt.key_hash = $4 FOR UPDATE OF receipt",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(subject_hash.as_slice())
    .bind(key_hash.as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| ProposalSubmissionError::Unavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored: Vec<u8> = row
        .try_get("request_fingerprint")
        .map_err(|_| ProposalSubmissionError::Unavailable)?;
    if stored.as_slice() != request_fingerprint {
        return Err(ProposalSubmissionError::IdempotencyConflict);
    }
    proposal_from_row(&row)
        .map(Some)
        .map_err(|_| ProposalSubmissionError::Unavailable)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn consume_simulation_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    access: &ScheduleAccess,
    token: &str,
    token_hash: &[u8; 32],
    subject_hash: &[u8; 32],
    expected_digest: &[u8],
    expected_request_hash: Option<&[u8; 32]>,
    now: DateTime<Utc>,
) -> Result<SimulationConsumption, SchedulingPortError> {
    let row = sqlx::query(
        "SELECT id, request_digest, request_hash, base_revision_id, base_revision_label, \
           result_snapshot, created_at, expires_at, evidence_schema, evidence_hash, \
           compilation_outcome, compiled_payload_hash \
         FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 \
         AND token_hash = $3 AND subject_hash = $4 AND consumed_at IS NULL AND expires_at > $5 \
         FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(token_hash.as_slice())
    .bind(subject_hash.as_slice())
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_port)?
    .ok_or(SchedulingPortError::NotFound)?;
    let stored_digest: Vec<u8> = row.try_get("request_digest").map_err(storage_port)?;
    if stored_digest != expected_digest {
        return Err(SchedulingPortError::InvalidQuery(
            "simulation token does not match the submitted operations".to_owned(),
        ));
    }
    let stored_request_hash = fixed_bytes::<32>(
        row.try_get::<Vec<u8>, _>("request_hash")
            .map_err(storage_port)?,
        "simulation request hash",
    )?;
    if expected_request_hash.is_some_and(|expected| expected != &stored_request_hash) {
        return Err(SchedulingPortError::InvalidQuery(
            "simulation token does not match the full submitted request".to_owned(),
        ));
    }
    let revision = current_revision_tx(transaction, scope).await?;
    let base_revision_id: Uuid = row.try_get("base_revision_id").map_err(storage_port)?;
    let base_revision_label: String = row.try_get("base_revision_label").map_err(storage_port)?;
    if revision.id != base_revision_id || revision.label != base_revision_label {
        return Err(SchedulingPortError::RevisionConflict {
            current_revision: revision.label,
        });
    }
    let simulation_id: Uuid = row.try_get("id").map_err(storage_port)?;
    let affected = sqlx::query(
        "UPDATE schedule_simulations SET consumed_at = $4 WHERE workspace_id = $1 \
         AND user_id = $2 AND id = $3 AND consumed_at IS NULL",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(simulation_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage_port)?
    .rows_affected();
    if affected != 1 {
        return Err(SchedulingPortError::NotFound);
    }
    let mut snapshot: Value = row.try_get("result_snapshot").map_err(storage_port)?;
    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(storage_port)?;
    let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(storage_port)?;
    let evidence_schema: i16 = row.try_get("evidence_schema").map_err(storage_port)?;
    let evidence_hash = fixed_bytes::<32>(
        row.try_get::<Vec<u8>, _>("evidence_hash")
            .map_err(storage_port)?,
        "simulation evidence hash",
    )?;
    let compilation_outcome: String = row.try_get("compilation_outcome").map_err(storage_port)?;
    let compiled_payload_hash = row
        .try_get::<Option<Vec<u8>>, _>("compiled_payload_hash")
        .map_err(storage_port)?
        .map(|value| fixed_bytes::<32>(value, "compiled proposal payload hash"))
        .transpose()?;
    let recomputed_evidence_hash = simulation_evidence_hash(
        scope,
        simulation_id,
        subject_hash,
        &stored_request_hash,
        base_revision_id,
        &base_revision_label,
        created_at,
        expires_at,
        &snapshot,
    )?;
    if evidence_schema != 1 || evidence_hash != recomputed_evidence_hash {
        return Err(SchedulingPortError::NotFound);
    }
    let snapshot_object = snapshot.as_object_mut().ok_or_else(|| {
        SchedulingPortError::Unavailable("simulation result is invalid".to_owned())
    })?;
    let privacy_evidence: SimulationPrivacyEvidence = serde_json::from_value(
        snapshot_object
            .remove("privacy_evidence")
            .ok_or(SchedulingPortError::NotFound)?,
    )
    .map_err(|_| SchedulingPortError::NotFound)?;
    let proposal_evidence: SimulationProposalEvidence = serde_json::from_value(
        snapshot_object
            .remove("proposal_evidence")
            .ok_or(SchedulingPortError::NotFound)?,
    )
    .map_err(|_| SchedulingPortError::NotFound)?;
    if privacy_evidence.schema_version != 1
        || privacy_evidence.item_ids.len() > 100
        || privacy_evidence.block_ids.len() > 100
        || privacy_evidence
            .item_ids
            .len()
            .saturating_add(privacy_evidence.block_ids.len())
            > 100
    {
        return Err(SchedulingPortError::NotFound);
    }
    if !proposal_evidence.is_valid() {
        return Err(SchedulingPortError::NotFound);
    }
    let recomputed_compiled_payload_hash = proposal_evidence
        .change_set()
        .map(|change_set| {
            serde_json::to_value(change_set)
                .map_err(|_| {
                    SchedulingPortError::Unavailable(
                        "compiled proposal payload cannot be encoded".to_owned(),
                    )
                })
                .and_then(|payload| proposal_payload_hash(&payload))
        })
        .transpose()?;
    let expected_outcome = if recomputed_compiled_payload_hash.is_some() {
        "actionable"
    } else {
        "manual_review"
    };
    if compilation_outcome != expected_outcome
        || compiled_payload_hash != recomputed_compiled_payload_hash
    {
        return Err(SchedulingPortError::NotFound);
    }
    snapshot_object.insert(
        "simulation_token".to_owned(),
        Value::String(token.to_owned()),
    );
    let result: SimulationResult = serde_json::from_value(snapshot)
        .map_err(|_| SchedulingPortError::Unavailable("simulation result is invalid".to_owned()))?;
    let application_ready = proposal_evidence.change_set().is_some();
    if result.application_ready != application_ready
        || result.change_set_schema
            != application_ready.then(|| PROPOSAL_CHANGE_SET_SCHEMA_V1.to_owned())
    {
        return Err(SchedulingPortError::NotFound);
    }
    if !access.include_sensitive
        && simulation_privacy_evidence_is_sensitive(
            transaction,
            scope,
            base_revision_id,
            &privacy_evidence,
        )
        .await?
    {
        return Err(SchedulingPortError::NotFound);
    }
    if !proposal_evidence_is_current(transaction, scope, &proposal_evidence).await? {
        return Err(SchedulingPortError::InvalidQuery(
            "canonical item or provider state changed; simulate again".to_owned(),
        ));
    }
    Ok(SimulationConsumption {
        result,
        proposal_evidence,
        persistence_proof: Some(super::SimulationPersistenceProof {
            simulation_id,
            subject_hash: *subject_hash,
            request_digest: fixed_bytes::<16>(stored_digest, "simulation request digest")?,
            request_hash: stored_request_hash,
            base_revision_id,
            created_at,
            expires_at,
            evidence_schema,
            evidence_hash,
            compilation_outcome,
            compiled_payload_hash,
        }),
    })
}

async fn proposal_evidence_is_current(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    evidence: &SimulationProposalEvidence,
) -> Result<bool, SchedulingPortError> {
    let Some(change_set) = evidence.change_set() else {
        return Ok(true);
    };
    for command in &change_set.commands {
        let (item_id, expected_revision, expect_deleted) = match command {
            ProposalCommand::CreateItem { item, .. } => {
                let occupied: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id = $1 AND id = $2)",
                )
                .bind(scope.workspace_id)
                .bind(item.id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(storage_port)?;
                if occupied {
                    return Ok(false);
                }
                if let Some(parent_id) = item.parent_id
                    && (!active_item_exists_tx(transaction, scope, parent_id).await?
                        || item_has_active_provider_mapping_tx(transaction, scope, parent_id)
                            .await?)
                {
                    return Ok(false);
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
        let state = sqlx::query(
            "SELECT revision, trashed_at IS NOT NULL AS deleted FROM items \
             WHERE workspace_id = $1 AND id = $2",
        )
        .bind(scope.workspace_id)
        .bind(item_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_port)?;
        let Some(state) = state else {
            return Ok(false);
        };
        let revision: i64 = state.try_get("revision").map_err(storage_port)?;
        let deleted: bool = state.try_get("deleted").map_err(storage_port)?;
        if u64::try_from(revision).ok() != Some(expected_revision) || deleted != expect_deleted {
            return Ok(false);
        }
        if item_has_active_provider_mapping_tx(transaction, scope, item_id).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn fixed_bytes<const LENGTH: usize>(
    value: Vec<u8>,
    label: &str,
) -> Result<[u8; LENGTH], SchedulingPortError> {
    value
        .try_into()
        .map_err(|_| SchedulingPortError::Unavailable(format!("{label} has an invalid length")))
}

#[derive(Debug)]
struct CurrentRevision {
    id: Uuid,
    label: String,
    timezone_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedConflict {
    id: String,
    kind: String,
    severity: String,
    message: String,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    related_item_ids: Vec<String>,
    penalty: u64,
    sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SimulationPrivacyEvidence {
    schema_version: u8,
    item_ids: BTreeSet<Uuid>,
    block_ids: BTreeSet<Uuid>,
    sensitive_at_simulation: bool,
}

impl PersistedConflict {
    fn into_view(self) -> ScheduleConflict {
        ScheduleConflict {
            id: self.id,
            kind: self.kind,
            severity: self.severity,
            message: self.message,
            start: self.start,
            end: self.end,
            related_item_ids: self.related_item_ids,
            penalty: self.penalty,
            sensitive: self.sensitive,
        }
    }
}

fn durable_snapshot(
    result: &ComposeScheduleResult,
    publication_hash: &[u8; 32],
) -> Result<Value, SchedulePublicationError> {
    let conflicts = result
        .plan
        .violations
        .iter()
        .enumerate()
        .map(|(index, violation)| {
            let related_item_ids: Vec<_> =
                violation.item_ids.iter().map(ToString::to_string).collect();
            let unknown_or_sensitive_item = violation.item_ids.iter().any(|item_id| {
                result
                    .source_item_sensitivity
                    .get(&item_id.0)
                    .copied()
                    .unwrap_or(true)
            });
            let overlapping_sensitive_block = result.plan.blocks.iter().any(|block| {
                block.is_sensitive
                    && (block
                        .item_id
                        .is_some_and(|item_id| violation.item_ids.contains(&item_id))
                        || violation
                            .start
                            .zip(violation.end)
                            .is_some_and(|(start, end)| block.start < end && block.end > start))
            });
            let sensitivity_is_ambiguous = violation.item_ids.is_empty()
                && (violation.start.is_none() || violation.end.is_none());
            let sensitive = unknown_or_sensitive_item
                || overlapping_sensitive_block
                || sensitivity_is_ambiguous;
            Ok(PersistedConflict {
                id: format!("conflict_{}_{index}", &encode_hex(publication_hash)[..16]),
                kind: serde_name(&violation.kind)?,
                severity: serde_name(&violation.severity)?,
                message: violation.message.clone(),
                start: violation.start.map(offset_to_chrono).transpose()?,
                end: violation.end.map(offset_to_chrono).transpose()?,
                related_item_ids,
                penalty: violation.penalty,
                sensitive,
            })
        })
        .collect::<Result<Vec<_>, SchedulePublicationError>>()?;
    let snapshot = json!({
        "schema_version": 1,
        "scheduler_publication_schema": SCHEDULER_PUBLICATION_SCHEMA,
        "compose": result,
        "evidence": {
            "source_item_sensitivity": result.source_item_sensitivity,
            "calendar_projection_stamps": result.calendar_projection_stamps,
        },
        "conflicts": conflicts,
    });
    if json_contains_unsafe_text(&snapshot, 0) {
        return Err(SchedulePublicationError::InvalidPayload);
    }
    let size = serde_json::to_vec(&snapshot)
        .map_err(|_| SchedulePublicationError::InvalidPayload)?
        .len();
    if size > 8 * 1024 * 1024 {
        return Err(SchedulePublicationError::InvalidPayload);
    }
    Ok(snapshot)
}

pub(super) fn validate_publishable_compose_result(
    timezone_name: &str,
    result: &ComposeScheduleResult,
) -> Result<([u8; 32], Value), SchedulePublicationError> {
    validate_publication_result(result)?;
    let publication_hash = publication_content_hash(timezone_name, result)?;
    let snapshot = durable_snapshot(result, &publication_hash)?;
    Ok((publication_hash, snapshot))
}

fn validate_publication_result(
    result: &ComposeScheduleResult,
) -> Result<(), SchedulePublicationError> {
    let revisions = &result.source_item_revisions;
    if result.source_item_count != revisions.len()
        || revisions.len() > MAX_CANONICAL_ITEMS
        || revisions.values().any(|revision| *revision == 0)
        || result.source_item_sensitivity.len() != revisions.len()
        || !revisions.keys().eq(result.source_item_sensitivity.keys())
        || result
            .accepted_item_count
            .checked_add(result.rejected_items.len())
            != Some(result.source_item_count)
    {
        return Err(SchedulePublicationError::InvalidPayload);
    }
    let horizon_start = offset_to_chrono(result.plan.horizon_start)?;
    let horizon_end = offset_to_chrono(result.plan.horizon_end)?;
    let mut previous_collection_id = None;
    for stamp in &result.calendar_projection_stamps {
        if stamp.collection_revision == 0
            || stamp.generation == 0
            || stamp.window_start >= stamp.window_end
            || stamp.window_start > horizon_start
            || stamp.window_end < horizon_end
            || !stamp
                .window_start
                .timestamp_subsec_nanos()
                .is_multiple_of(1_000)
            || !stamp
                .window_end
                .timestamp_subsec_nanos()
                .is_multiple_of(1_000)
            || !stamp
                .refreshed_at
                .timestamp_subsec_nanos()
                .is_multiple_of(1_000)
            || previous_collection_id.is_some_and(|previous| previous >= stamp.collection_id)
        {
            return Err(SchedulePublicationError::InvalidPayload);
        }
        previous_collection_id = Some(stamp.collection_id);
    }
    let mut block_ids = BTreeSet::new();
    for block in &result.plan.blocks {
        if !block_ids.insert(block.id) {
            return Err(SchedulePublicationError::InvalidPayload);
        }
        // This mirrors the narrowest durable column contract. Keep it here as
        // a repository integrity fence even though the HTTP compose boundary
        // rejects untrusted fixed-block titles earlier.
        if block.title.trim().is_empty()
            || block.title.chars().count() > 500
            || block.title.chars().any(char::is_control)
        {
            return Err(SchedulePublicationError::InvalidPayload);
        }
        if let Some(item_id) = block.item_id
            && (revisions.get(&item_id.0).is_none()
                || result.source_item_sensitivity.get(&item_id.0) != Some(&block.is_sensitive))
        {
            return Err(SchedulePublicationError::InvalidPayload);
        }
    }
    let mut rejected_ids = BTreeSet::new();
    for rejected in &result.rejected_items {
        if !rejected_ids.insert(rejected.item_id)
            || result.source_item_sensitivity.get(&rejected.item_id) != Some(&rejected.is_sensitive)
        {
            return Err(SchedulePublicationError::InvalidPayload);
        }
    }
    Ok(())
}

pub(crate) fn publication_content_hash(
    timezone_name: &str,
    result: &ComposeScheduleResult,
) -> Result<[u8; 32], SchedulePublicationError> {
    #[derive(Serialize)]
    struct Content<'a> {
        domain: &'static str,
        scheduler_publication_schema: &'static str,
        timezone_name: &'a str,
        result: &'a ComposeScheduleResult,
        source_item_sensitivity: &'a BTreeMap<Uuid, bool>,
        calendar_projection_stamps: &'a [CalendarProjectionStamp],
    }
    let bytes = serde_json::to_vec(&Content {
        domain: "dayweave.schedule-publication-content.v1",
        scheduler_publication_schema: SCHEDULER_PUBLICATION_SCHEMA,
        timezone_name,
        result,
        source_item_sensitivity: &result.source_item_sensitivity,
        calendar_projection_stamps: &result.calendar_projection_stamps,
    })
    .map_err(|_| SchedulePublicationError::InvalidPayload)?;
    Ok(Sha256::digest(bytes).into())
}

/// Selects the authoritative execution head for every semantic work session,
/// then turns each current-revision defer that touches this horizon into an
/// exact publication obligation.
///
/// The window/state filters intentionally live outside the ranking CTE. A
/// newer active lease for the same semantic identity supersedes an older defer
/// even when the active row has an earlier protocol timestamp.
async fn required_deferred_placements_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    horizon_start: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
    result: &ComposeScheduleResult,
) -> Result<Vec<RequiredDeferredPlacement>, SchedulePublicationError> {
    let rows = sqlx::query(
        "WITH semantic_heads AS MATERIALIZED ( \
           SELECT session.id, session.item_id, session.item_revision, session.occurrence_id, \
             session.session_index, session.state, session.move_start, session.move_end, \
             ROW_NUMBER() OVER ( \
               PARTITION BY session.item_id, session.item_revision, session.occurrence_id, \
                 session.session_index \
               ORDER BY CASE WHEN session.id = execution_state.active_session_id \
                 THEN 1 ELSE 0 END DESC, session.updated_at DESC, session.id DESC \
             ) AS semantic_rank \
           FROM execution_sessions AS session \
           JOIN execution_state ON execution_state.workspace_id = session.workspace_id \
           WHERE session.workspace_id = $1 \
         ) \
         SELECT head.id, head.item_id, head.item_revision, head.occurrence_id, \
           head.session_index, head.move_start, head.move_end \
         FROM semantic_heads AS head \
         JOIN items AS item ON item.workspace_id = $1 AND item.id = head.item_id \
         WHERE head.semantic_rank = 1 AND head.state = 'deferred' \
           AND item.trashed_at IS NULL AND item.revision = head.item_revision \
           AND head.move_start < $3 AND head.move_end > $2 \
         ORDER BY head.item_id, head.item_revision, head.occurrence_id NULLS FIRST, \
           head.session_index, head.id",
    )
    .bind(scope.workspace_id)
    .bind(horizon_start)
    .bind(horizon_end)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| SchedulePublicationError::Unavailable)?;

    let mut placements = Vec::with_capacity(rows.len());
    for row in rows {
        let deferred_session_id: Uuid = row
            .try_get("id")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let item_id: Uuid = row
            .try_get("item_id")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let item_revision: i64 = row
            .try_get("item_revision")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let occurrence_id: Option<Uuid> = row
            .try_get("occurrence_id")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let session_index: i32 = row
            .try_get("session_index")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let move_start: DateTime<Utc> = row
            .try_get("move_start")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let move_end: DateTime<Utc> = row
            .try_get("move_end")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        if move_start < horizon_start || move_end > horizon_end {
            return Err(SchedulePublicationError::DeferredPlacementRequired);
        }
        let item_revision_u64 =
            u64::try_from(item_revision).map_err(|_| SchedulePublicationError::Unavailable)?;

        let matching = result
            .plan
            .blocks
            .iter()
            .filter(|block| {
                block
                    .item_id
                    .is_some_and(|candidate| candidate.0 == item_id)
                    && block.occurrence_id.map(|candidate| candidate.0) == occurrence_id
                    && i32::from(block.session_index) == session_index
                    && result.source_item_revisions.get(&item_id) == Some(&item_revision_u64)
            })
            .collect::<Vec<_>>();
        let [block] = matching.as_slice() else {
            return Err(SchedulePublicationError::DeferredPlacementRequired);
        };
        if block.kind != ScheduleBlockKind::Pinned
            || offset_to_chrono(block.start)? != move_start
            || offset_to_chrono(block.end)? != move_end
        {
            return Err(SchedulePublicationError::DeferredPlacementRequired);
        }
        placements.push(RequiredDeferredPlacement {
            deferred_session_id,
            source_block_id: block.id,
            item_id,
            item_revision,
            occurrence_id,
            session_index,
            move_start,
            move_end,
        });
    }
    Ok(placements)
}

async fn revision_has_deferred_placements_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    revision_id: Uuid,
    placements: &[RequiredDeferredPlacement],
) -> Result<bool, SchedulePublicationError> {
    for placement in placements {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS ( \
               SELECT 1 FROM schedule_deferred_placements \
               WHERE workspace_id = $1 AND schedule_revision_id = $2 \
                 AND deferred_execution_session_id = $3 AND source_block_id = $4 \
                 AND item_id = $5 AND item_revision = $6 \
                 AND occurrence_id IS NOT DISTINCT FROM $7 \
                 AND session_index = $8 AND move_start = $9 AND move_end = $10 \
             )",
        )
        .bind(workspace_id)
        .bind(revision_id)
        .bind(placement.deferred_session_id)
        .bind(placement.source_block_id)
        .bind(placement.item_id)
        .bind(placement.item_revision)
        .bind(placement.occurrence_id)
        .bind(placement.session_index)
        .bind(placement.move_start)
        .bind(placement.move_end)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;
        if !exists {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn insert_deferred_placements_tx(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    revision_id: Uuid,
    placements: &[RequiredDeferredPlacement],
    created_at: DateTime<Utc>,
) -> Result<(), SchedulePublicationError> {
    for placement in placements {
        sqlx::query(
            "INSERT INTO schedule_deferred_placements (workspace_id, schedule_revision_id, \
             deferred_execution_session_id, source_block_id, item_id, item_revision, occurrence_id, \
             session_index, move_start, move_end, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(workspace_id)
        .bind(revision_id)
        .bind(placement.deferred_session_id)
        .bind(placement.source_block_id)
        .bind(placement.item_id)
        .bind(placement.item_revision)
        .bind(placement.occurrence_id)
        .bind(placement.session_index)
        .bind(placement.move_start)
        .bind(placement.move_end)
        .bind(created_at)
        .execute(&mut **transaction)
        .await
        .map_err(|_| SchedulePublicationError::Unavailable)?;
    }
    Ok(())
}

async fn bind_publication_key_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    key: Uuid,
    request_hash: &[u8; 32],
    revision_id: Uuid,
    created_at: DateTime<Utc>,
) -> Result<(), SchedulePublicationError> {
    sqlx::query(
        "INSERT INTO schedule_publication_requests (workspace_id, user_id, idempotency_key, \
         request_hash, schedule_revision_id, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(key)
    .bind(request_hash.as_slice())
    .bind(revision_id)
    .bind(created_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| SchedulePublicationError::Unavailable)?;
    Ok(())
}

async fn publication_receipt_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    key: Uuid,
    request_hash: &[u8; 32],
) -> Result<Option<SchedulePublication>, SchedulePublicationError> {
    let row = sqlx::query(
        "SELECT publication.request_hash, revision.id, revision.revision_number, \
         revision.input_digest, revision.horizon_start, revision.horizon_end, \
         revision.timezone_name, revision.published_at \
         FROM schedule_publication_requests AS publication \
         JOIN schedule_revisions AS revision ON revision.workspace_id = publication.workspace_id \
          AND revision.id = publication.schedule_revision_id \
         WHERE publication.workspace_id = $1 AND publication.user_id = $2 \
          AND publication.idempotency_key = $3 FOR UPDATE OF publication",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| SchedulePublicationError::Unavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored: Vec<u8> = row
        .try_get("request_hash")
        .map_err(|_| SchedulePublicationError::Unavailable)?;
    if stored.as_slice() != request_hash {
        return Err(SchedulePublicationError::IdempotencyConflict);
    }
    Ok(Some(SchedulePublication {
        revision: revision_from_row(&row)?,
        replayed: true,
    }))
}

fn revision_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PublishedScheduleRevision, SchedulePublicationError> {
    let id: Uuid = row
        .try_get("id")
        .map_err(|_| SchedulePublicationError::Unavailable)?;
    let number: i64 = row
        .try_get("revision_number")
        .map_err(|_| SchedulePublicationError::Unavailable)?;
    let digest: Vec<u8> = row
        .try_get("input_digest")
        .map_err(|_| SchedulePublicationError::Unavailable)?;
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| SchedulePublicationError::Unavailable)?;
    Ok(PublishedScheduleRevision {
        id,
        revision: revision_label(number, id),
        revision_number: u64::try_from(number)
            .map_err(|_| SchedulePublicationError::Unavailable)?,
        input_digest: encode_prefixed_sha256(&digest),
        horizon_start: row
            .try_get("horizon_start")
            .map_err(|_| SchedulePublicationError::Unavailable)?,
        horizon_end: row
            .try_get("horizon_end")
            .map_err(|_| SchedulePublicationError::Unavailable)?,
        timezone_name: row
            .try_get("timezone_name")
            .map_err(|_| SchedulePublicationError::Unavailable)?,
        published_at: row
            .try_get("published_at")
            .map_err(|_| SchedulePublicationError::Unavailable)?,
    })
}

async fn assert_current_item_snapshot(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    expected: &BTreeMap<Uuid, u64>,
) -> Result<(), SchedulePublicationError> {
    let rows = sqlx::query(
        "SELECT id, revision FROM items WHERE workspace_id = $1 AND trashed_at IS NULL \
         ORDER BY id FOR SHARE",
    )
    .bind(scope.workspace_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| SchedulePublicationError::Unavailable)?;
    if rows.len() > MAX_CANONICAL_ITEMS || rows.len() != expected.len() {
        return Err(SchedulePublicationError::StaleComposition);
    }
    for row in rows {
        let id: Uuid = row
            .try_get("id")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let revision: i64 = row
            .try_get("revision")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        if revision <= 0 || expected.get(&id).copied() != u64::try_from(revision).ok() {
            return Err(SchedulePublicationError::StaleComposition);
        }
    }
    Ok(())
}

/// Rechecks and share-locks every Calendar collection row after the canonical
/// item-space lock. Locking even currently unselected rows prevents a
/// concurrent configuration update from introducing a new blocking source as
/// a publication is sealed.
#[allow(clippy::too_many_lines)] // Lock, eligibility, freshness, and exact stamp checks are one fence.
async fn assert_current_calendar_projection(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    horizon_start: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
    expected: &[CalendarProjectionStamp],
) -> Result<(), SchedulePublicationError> {
    let rows = sqlx::query(
        "SELECT collection.id, collection.selected, collection.provider_deleted, \
         collection.sync_role, collection.confirmed_busy_policy, collection.tentative_policy, \
         collection.free_policy, collection.all_day_policy, collection.revision, \
         collection.planning_projection_state, collection.planning_generation, \
         collection.planning_collection_revision, collection.planning_window_start, \
         collection.planning_window_end, collection.planning_window_refreshed_at, \
         account.status AS account_status, account.sync_enabled, account.tombstoned_at, \
         statement_timestamp() AS observed_at, \
         EXISTS(SELECT 1 FROM provider_sync_mappings mapping JOIN items item \
           ON item.workspace_id = mapping.workspace_id AND item.id = mapping.local_entity_id \
           WHERE mapping.workspace_id = collection.workspace_id \
             AND mapping.provider_account_id = collection.provider_account_id \
             AND mapping.collection_id = collection.id \
             AND mapping.entity_kind = 'calendar_occurrence' \
             AND mapping.tombstoned_at IS NULL AND item.trashed_at IS NULL \
             AND item.scheduling_constraints ? 'calendar_event') AS has_active_blocking_occurrence \
         FROM google_sync_collections collection JOIN provider_accounts account \
           ON account.workspace_id = collection.workspace_id \
          AND account.user_id = collection.user_id \
          AND account.id = collection.provider_account_id \
         WHERE collection.workspace_id = $1 AND collection.user_id = $2 \
           AND collection.collection_kind = 'calendar' \
         ORDER BY collection.id FOR SHARE OF collection, account",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| SchedulePublicationError::Unavailable)?;

    let mut actual = Vec::new();
    for row in rows {
        let selected: bool = row
            .try_get("selected")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let provider_deleted: bool = row
            .try_get("provider_deleted")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let role: String = row
            .try_get("sync_role")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let policy_blocks = [
            "confirmed_busy_policy",
            "tentative_policy",
            "free_policy",
            "all_day_policy",
        ]
        .into_iter()
        .map(|column| row.try_get::<String, _>(column))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SchedulePublicationError::Unavailable)?
        .into_iter()
        .any(|policy| policy == "blocking");
        let account_active = row
            .try_get::<String, _>("account_status")
            .map_err(|_| SchedulePublicationError::Unavailable)?
            == "active"
            && row
                .try_get::<bool, _>("sync_enabled")
                .map_err(|_| SchedulePublicationError::Unavailable)?
            && row
                .try_get::<Option<DateTime<Utc>>, _>("tombstoned_at")
                .map_err(|_| SchedulePublicationError::Unavailable)?
                .is_none();
        let required = account_active
            && selected
            && !provider_deleted
            && matches!(role.as_str(), "blocking" | "writable")
            && policy_blocks;
        let has_active_blocking_occurrence: bool = row
            .try_get("has_active_blocking_occurrence")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        if !required && has_active_blocking_occurrence {
            return Err(SchedulePublicationError::StaleComposition);
        }
        if !required {
            continue;
        }

        let state: String = row
            .try_get("planning_projection_state")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let revision: i64 = row
            .try_get("revision")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let generation: i64 = row
            .try_get("planning_generation")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let projection_revision: Option<i64> = row
            .try_get("planning_collection_revision")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let window_start: Option<DateTime<Utc>> = row
            .try_get("planning_window_start")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let window_end: Option<DateTime<Utc>> = row
            .try_get("planning_window_end")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let refreshed_at: Option<DateTime<Utc>> = row
            .try_get("planning_window_refreshed_at")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let observed_at: DateTime<Utc> = row
            .try_get("observed_at")
            .map_err(|_| SchedulePublicationError::Unavailable)?;
        let (Some(window_start), Some(window_end), Some(refreshed_at)) =
            (window_start, window_end, refreshed_at)
        else {
            return Err(SchedulePublicationError::StaleComposition);
        };
        if state != "complete"
            || revision <= 0
            || generation <= 0
            || projection_revision != Some(revision)
            || window_start > horizon_start
            || window_end < horizon_end
            || refreshed_at > observed_at
            || refreshed_at < observed_at - Duration::minutes(CALENDAR_PROJECTION_MAX_AGE_MINUTES)
        {
            return Err(SchedulePublicationError::StaleComposition);
        }
        actual.push(CalendarProjectionStamp {
            collection_id: row
                .try_get("id")
                .map_err(|_| SchedulePublicationError::Unavailable)?,
            collection_revision: u64::try_from(revision)
                .map_err(|_| SchedulePublicationError::Unavailable)?,
            generation: u64::try_from(generation)
                .map_err(|_| SchedulePublicationError::Unavailable)?,
            window_start,
            window_end,
            refreshed_at,
        });
    }
    if actual != expected {
        return Err(SchedulePublicationError::StaleComposition);
    }
    Ok(())
}

async fn lock_owner(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), sqlx::Error> {
    let exists: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2 \
         AND role = 'owner' AND removed_at IS NULL FOR UPDATE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await?;
    if exists.is_none() {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

async fn current_revision_pool(
    pool: &PgPool,
    scope: DatabaseScope,
) -> Result<CurrentRevision, SchedulingPortError> {
    let row = sqlx::query(
        "SELECT id, revision_number, timezone_name FROM schedule_revisions \
         WHERE workspace_id = $1 AND created_by_user_id = $2 AND state = 'published'",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_port)?
    .ok_or(SchedulingPortError::NotFound)?;
    current_revision_from_row(&row)
}

async fn current_revision_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<CurrentRevision, SchedulingPortError> {
    let row = sqlx::query(
        "SELECT id, revision_number, timezone_name FROM schedule_revisions \
         WHERE workspace_id = $1 AND created_by_user_id = $2 AND state = 'published' FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_port)?
    .ok_or(SchedulingPortError::NotFound)?;
    current_revision_from_row(&row)
}

const EFFECTIVE_SENSITIVITY_QUERY: &str = "WITH RECURSIVE sensitivity AS ( \
       SELECT item.id, item.is_sensitive AS effective_sensitive, ARRAY[item.id]::uuid[] AS path \
       FROM items AS item \
       WHERE item.workspace_id = $1 AND item.trashed_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM item_hierarchy AS edge \
           WHERE edge.workspace_id = item.workspace_id AND edge.child_item_id = item.id) \
       UNION ALL \
       SELECT child.id, parent.effective_sensitive OR child.is_sensitive, parent.path || child.id \
       FROM sensitivity AS parent \
       JOIN item_hierarchy AS edge ON edge.workspace_id = $1 AND edge.parent_item_id = parent.id \
       JOIN items AS child ON child.workspace_id = edge.workspace_id \
         AND child.id = edge.child_item_id AND child.trashed_at IS NULL \
       WHERE cardinality(parent.path) <= 10000 AND NOT child.id = ANY(parent.path) \
     ) \
     SELECT requested.id, COALESCE(BOOL_OR(sensitivity.effective_sensitive), true) AS sensitive \
     FROM UNNEST($2::uuid[]) AS requested(id) \
     LEFT JOIN sensitivity ON sensitivity.id = requested.id \
     GROUP BY requested.id";

fn sensitivity_rows(
    rows: Vec<sqlx::postgres::PgRow>,
) -> Result<BTreeMap<Uuid, bool>, SchedulingPortError> {
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("id").map_err(storage_port)?,
                row.try_get("sensitive").map_err(storage_port)?,
            ))
        })
        .collect()
}

async fn current_item_sensitivity_pool(
    pool: &PgPool,
    scope: DatabaseScope,
    ids: &BTreeSet<Uuid>,
) -> Result<BTreeMap<Uuid, bool>, SchedulingPortError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = sqlx::query(EFFECTIVE_SENSITIVITY_QUERY)
        .bind(scope.workspace_id)
        .bind(ids.iter().copied().collect::<Vec<_>>())
        .fetch_all(pool)
        .await
        .map_err(storage_port)?;
    sensitivity_rows(rows)
}

async fn current_item_sensitivity_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    ids: &BTreeSet<Uuid>,
) -> Result<BTreeMap<Uuid, bool>, SchedulingPortError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = sqlx::query(EFFECTIVE_SENSITIVITY_QUERY)
        .bind(scope.workspace_id)
        .bind(ids.iter().copied().collect::<Vec<_>>())
        .fetch_all(&mut **transaction)
        .await
        .map_err(storage_port)?;
    sensitivity_rows(rows)
}

async fn simulation_privacy_evidence_is_sensitive(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    revision_id: Uuid,
    evidence: &SimulationPrivacyEvidence,
) -> Result<bool, SchedulingPortError> {
    if evidence.sensitive_at_simulation {
        return Ok(true);
    }

    let mut item_ids = evidence.item_ids.clone();
    if !evidence.block_ids.is_empty() {
        let rows = sqlx::query(
            "SELECT source_block_id, item_id, is_sensitive FROM schedule_blocks \
             WHERE workspace_id = $1 AND schedule_revision_id = $2 \
               AND source_block_id = ANY($3::uuid[])",
        )
        .bind(scope.workspace_id)
        .bind(revision_id)
        .bind(evidence.block_ids.iter().copied().collect::<Vec<_>>())
        .fetch_all(&mut **transaction)
        .await
        .map_err(storage_port)?;
        let mut found_blocks = BTreeSet::new();
        for row in rows {
            found_blocks.insert(
                row.try_get::<Uuid, _>("source_block_id")
                    .map_err(storage_port)?,
            );
            if row
                .try_get::<bool, _>("is_sensitive")
                .map_err(storage_port)?
            {
                return Ok(true);
            }
            if let Some(item_id) = row
                .try_get::<Option<Uuid>, _>("item_id")
                .map_err(storage_port)?
            {
                item_ids.insert(item_id);
            }
        }
        if found_blocks != evidence.block_ids {
            return Ok(true);
        }
    }
    let current = current_item_sensitivity_tx(transaction, scope, &item_ids).await?;
    Ok(item_ids
        .iter()
        .any(|id| current.get(id).copied().unwrap_or(true)))
}

fn current_revision_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CurrentRevision, SchedulingPortError> {
    let id: Uuid = row.try_get("id").map_err(storage_port)?;
    let number: i64 = row.try_get("revision_number").map_err(storage_port)?;
    if number <= 0 {
        return Err(SchedulingPortError::Unavailable(
            "published schedule revision is invalid".to_owned(),
        ));
    }
    Ok(CurrentRevision {
        id,
        label: revision_label(number, id),
        timezone_name: row.try_get("timezone_name").map_err(storage_port)?,
    })
}

async fn search_item_rows(
    pool: &PgPool,
    scope: DatabaseScope,
    revision_id: Uuid,
) -> Result<Vec<sqlx::postgres::PgRow>, SchedulingPortError> {
    sqlx::query(
        "WITH RECURSIVE sensitivity AS ( \
           SELECT item.id, item.is_sensitive AS effective_sensitive, ARRAY[item.id]::uuid[] AS path \
           FROM items AS item \
           WHERE item.workspace_id = $1 AND item.trashed_at IS NULL \
             AND NOT EXISTS (SELECT 1 FROM item_hierarchy AS edge \
               WHERE edge.workspace_id = item.workspace_id AND edge.child_item_id = item.id) \
           UNION ALL \
           SELECT child.id, parent.effective_sensitive OR child.is_sensitive, parent.path || child.id \
           FROM sensitivity AS parent \
           JOIN item_hierarchy AS edge ON edge.workspace_id = $1 AND edge.parent_item_id = parent.id \
           JOIN items AS child ON child.workspace_id = edge.workspace_id \
             AND child.id = edge.child_item_id AND child.trashed_at IS NULL \
           WHERE cardinality(parent.path) <= 10000 AND NOT child.id = ANY(parent.path) \
         ), scheduled AS ( \
           SELECT item_id, MIN(starts_at) AS scheduled_start FROM schedule_blocks \
           WHERE workspace_id = $1 AND schedule_revision_id = $2 AND item_id IS NOT NULL \
           GROUP BY item_id \
         ) \
         SELECT item.id, item.title, item.status, item.kind, item.deadline_at, \
           item.scheduling_constraints, edge.parent_item_id, scheduled.scheduled_start, \
           COALESCE(sensitivity.effective_sensitive, true) AS effective_sensitive \
         FROM items AS item \
         LEFT JOIN sensitivity ON sensitivity.id = item.id \
         LEFT JOIN item_hierarchy AS edge ON edge.workspace_id = item.workspace_id \
           AND edge.child_item_id = item.id \
         LEFT JOIN scheduled ON scheduled.item_id = item.id \
         WHERE item.workspace_id = $1 AND item.trashed_at IS NULL \
         ORDER BY item.title, item.id LIMIT 10001",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .fetch_all(pool)
    .await
    .map_err(storage_port)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One auditable table covers every operation kind and redaction branch.
async fn simulate_against_revision(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    access: &ScheduleAccess,
    revision_id: Uuid,
    request: SimulationRequest,
    token: String,
    request_digest: String,
    now: DateTime<Utc>,
) -> Result<
    (
        SimulationResult,
        SimulationPrivacyEvidence,
        SimulationProposalEvidence,
    ),
    SchedulingPortError,
> {
    let rows = sqlx::query(
        "SELECT source_block_id, item_id, block_kind, is_fixed, is_sensitive FROM schedule_blocks \
         WHERE workspace_id = $1 AND schedule_revision_id = $2 ORDER BY ordinal, source_block_id",
    )
    .bind(scope.workspace_id)
    .bind(revision_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_port)?;
    let mut blocks = BTreeMap::new();
    let sensitivity: Value = sqlx::query_scalar(
        "SELECT result_snapshot #> '{evidence,source_item_sensitivity}' \
         FROM schedule_revision_details WHERE workspace_id = $1 AND user_id = $2 \
         AND schedule_revision_id = $3",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(revision_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_port)?
    .ok_or(SchedulingPortError::RepublishRequired)?;
    let historical_sensitivity: BTreeMap<Uuid, bool> = serde_json::from_value(sensitivity)
        .map_err(|_| {
            SchedulingPortError::Unavailable("schedule sensitivity evidence is invalid".to_owned())
        })?;
    let mut item_ids = historical_sensitivity
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    for row in &rows {
        if let Some(item_id) = row
            .try_get::<Option<Uuid>, _>("item_id")
            .map_err(storage_port)?
        {
            item_ids.insert(item_id);
        }
    }
    for operation in &request.operations {
        if let Some(item_id) = target_item_id(operation)? {
            item_ids.insert(item_id);
        }
        if let Some(parent_id) = parent_item_id(operation)? {
            item_ids.insert(parent_id);
        }
    }
    let current_sensitivity = current_item_sensitivity_tx(transaction, scope, &item_ids).await?;
    let sensitive_items = item_ids
        .iter()
        .filter(|id| {
            historical_sensitivity.get(id).copied().unwrap_or(false)
                || current_sensitivity.get(id).copied().unwrap_or(true)
        })
        .copied()
        .collect::<BTreeSet<_>>();
    for row in rows {
        let id: Uuid = row.try_get("source_block_id").map_err(storage_port)?;
        let item_id: Option<Uuid> = row.try_get("item_id").map_err(storage_port)?;
        let sensitive = row
            .try_get::<bool, _>("is_sensitive")
            .map_err(storage_port)?
            || item_id.is_some_and(|id| sensitive_items.contains(&id));
        blocks.insert(
            id,
            (
                sensitive,
                row.try_get::<bool, _>("is_fixed").map_err(storage_port)?,
                row.try_get::<String, _>("block_kind")
                    .map_err(storage_port)?,
            ),
        );
    }
    let moved_blocks: Vec<SimulatedBlockMove> = Vec::new();
    let mut warnings = Vec::new();
    let mut privacy_evidence = SimulationPrivacyEvidence {
        schema_version: 1,
        item_ids: BTreeSet::new(),
        block_ids: BTreeSet::new(),
        sensitive_at_simulation: false,
    };
    for operation in &request.operations {
        if let Some(parent_id) = parent_item_id(operation)? {
            privacy_evidence.item_ids.insert(parent_id);
            privacy_evidence.sensitive_at_simulation |= sensitive_items.contains(&parent_id);
            if sensitive_items.contains(&parent_id) && !access.include_sensitive {
                warnings.push(issue(
                    "redacted_parent",
                    "A private parent item cannot be changed through this integration.",
                    Vec::new(),
                ));
            }
        }
        if let Some(target_id) = operation
            .target_id
            .as_deref()
            .and_then(|target| Uuid::parse_str(target).ok())
        {
            if operation.kind == PlanOperationKind::MoveBlock {
                privacy_evidence.block_ids.insert(target_id);
                privacy_evidence.sensitive_at_simulation |= blocks
                    .get(&target_id)
                    .is_none_or(|(sensitive, _, _)| *sensitive);
            } else if operation_targets_item(operation.kind) {
                privacy_evidence.item_ids.insert(target_id);
                privacy_evidence.sensitive_at_simulation |= sensitive_items.contains(&target_id);
                if sensitive_items.contains(&target_id) && !access.include_sensitive {
                    warnings.push(issue(
                        "redacted_item",
                        "A private item cannot be changed through this integration.",
                        Vec::new(),
                    ));
                    continue;
                }
            }
        }
        match operation.kind {
            PlanOperationKind::MoveBlock => {
                let target = operation.target_id.as_deref().ok_or_else(|| {
                    SchedulingPortError::InvalidQuery("move_block requires target_id".to_owned())
                })?;
                let block_id = Uuid::parse_str(target).map_err(|_| {
                    SchedulingPortError::InvalidQuery("move_block target_id must be a UUID".to_owned())
                })?;
                let Some((sensitive, fixed, kind)) = blocks.get(&block_id) else {
                    warnings.push(issue(
                        "unknown_block",
                        "The requested block no longer exists.",
                        vec![target.to_owned()],
                    ));
                    continue;
                };
                if *sensitive && !access.include_sensitive {
                    warnings.push(issue(
                        "redacted_block",
                        "A private block cannot be changed through this integration.",
                        Vec::new(),
                    ));
                    continue;
                }
                if *fixed || kind != "planned" {
                    warnings.push(issue(
                        "not_movable",
                        "Pinned, calendar, external, and otherwise fixed blocks cannot be moved by this simulation.",
                        vec![target.to_owned()],
                    ));
                } else {
                    warnings.push(issue(
                        "not_modeled",
                        "Move feasibility is not modeled until the scheduler can prove horizon, availability, overlap, and hard-constraint safety.",
                        vec![target.to_owned()],
                    ));
                }
            }
            PlanOperationKind::DeleteItem => warnings.push(issue(
                "confirmation_required",
                "Deletion remains a proposal and requires explicit confirmation in DayWeave.",
                operation.target_id.clone().into_iter().collect(),
            )),
            PlanOperationKind::CreateItem
            | PlanOperationKind::CompleteItem
            | PlanOperationKind::UpdateConstraint
            | PlanOperationKind::CreateEvent => warnings.push(issue(
                "device_review_required",
                "The operation can become a typed proposal, but only an authorized DayWeave device can preview and apply it.",
                operation.target_id.clone().into_iter().collect(),
            )),
            PlanOperationKind::UpdateItem
            | PlanOperationKind::GoalBreakdown
            | PlanOperationKind::ReplaceSchedule => warnings.push(issue(
                "not_modeled",
                &format!(
                    "{} is not modeled by the current what-if engine; the operation remains proposal-only.",
                    operation_kind_name(operation.kind)
                ),
                operation.target_id.clone().into_iter().collect(),
            )),
        }
    }
    let proposal_evidence = compile_proposal_evidence_tx(
        transaction,
        scope,
        access,
        &request.operations,
        &sensitive_items,
        now,
    )
    .await?;
    let application_ready = proposal_evidence.change_set().is_some();
    if !application_ready {
        warnings.push(issue(
            "manual_review_only",
            "This exact simulation can be saved for manual review, but it cannot be applied as a typed change set.",
            Vec::new(),
        ));
    }
    Ok((
        SimulationResult {
            simulation_token: token,
            request_digest,
            base_revision: request.base_revision,
            application_ready,
            change_set_schema: application_ready.then(|| PROPOSAL_CHANGE_SET_SCHEMA_V1.to_owned()),
            moved_blocks,
            unscheduled_item_ids: Vec::new(),
            violations: Vec::new(),
            warnings,
        },
        privacy_evidence,
        proposal_evidence,
    ))
}

async fn compile_proposal_evidence_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    access: &ScheduleAccess,
    operations: &[super::PlanOperation],
    sensitive_items: &BTreeSet<Uuid>,
    now: DateTime<Utc>,
) -> Result<SimulationProposalEvidence, SchedulingPortError> {
    let proposal_kind = match classify_request(operations) {
        RequestCompilation::Actionable(kind) => kind,
        RequestCompilation::ManualReview(reason) => {
            return Ok(SimulationProposalEvidence::manual_review(vec![
                reason.to_owned(),
            ]));
        }
    };
    let mut compilations = Vec::with_capacity(operations.len());
    for operation in operations {
        if let Some(parent_id) = parent_item_id(operation)? {
            if sensitive_items.contains(&parent_id) && !access.include_sensitive {
                compilations.push(OperationCompilation::ManualReview("redacted_parent"));
                continue;
            }
            if item_has_active_provider_mapping_tx(transaction, scope, parent_id).await? {
                compilations.push(OperationCompilation::ManualReview(
                    "provider_managed_parent",
                ));
                continue;
            }
            if !active_item_exists_tx(transaction, scope, parent_id).await? {
                return Err(SchedulingPortError::InvalidQuery(format!(
                    "{} parent item was not found",
                    operation_kind_name(operation.kind)
                )));
            }
        }
        let current = if let Some(item_id) = target_item_id(operation)? {
            if sensitive_items.contains(&item_id) && !access.include_sensitive {
                compilations.push(OperationCompilation::ManualReview("redacted_item"));
                continue;
            }
            if item_has_active_provider_mapping_tx(transaction, scope, item_id).await? {
                compilations.push(OperationCompilation::ManualReview("provider_managed_item"));
                continue;
            }
            if !active_item_exists_tx(transaction, scope, item_id).await? {
                return Err(SchedulingPortError::InvalidQuery(format!(
                    "{} target item was not found",
                    operation_kind_name(operation.kind)
                )));
            }
            Some(
                fetch_item_batch_tx(transaction, scope.workspace_id, item_id, false)
                    .await
                    .map_err(|_| {
                        SchedulingPortError::Unavailable(
                            "canonical item evidence is unavailable".to_owned(),
                        )
                    })?,
            )
        } else {
            None
        };
        compilations.push(compile_operation(operation, current.as_ref(), now)?);
    }
    finish_evidence(proposal_kind, compilations)
}

async fn active_item_exists_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_id: Uuid,
) -> Result<bool, SchedulingPortError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM items WHERE workspace_id = $1 AND id = $2 \
         AND trashed_at IS NULL)",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_port)
}

async fn item_has_active_provider_mapping_tx(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_id: Uuid,
) -> Result<bool, SchedulingPortError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM provider_sync_mappings WHERE workspace_id = $1 \
         AND entity_kind IN ('item', 'calendar_occurrence') AND local_entity_id = $2 \
         AND tombstoned_at IS NULL)",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_port)
}

async fn prune_simulations(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    now: DateTime<Utc>,
) -> Result<(), SchedulingPortError> {
    sqlx::query(
        "DELETE FROM schedule_simulations WHERE workspace_id = $1 AND user_id = $2 \
         AND (expires_at <= $3 OR consumed_at IS NOT NULL)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage_port)?;
    Ok(())
}

fn validate_simulation_request(request: &SimulationRequest) -> Result<(), SchedulingPortError> {
    if request.base_revision.is_empty() || request.base_revision.len() > 100 {
        return Err(SchedulingPortError::InvalidQuery(
            "base_revision is invalid".to_owned(),
        ));
    }
    if request.operations.is_empty() || request.operations.len() > 100 {
        return Err(SchedulingPortError::InvalidQuery(
            "operations must contain between 1 and 100 entries".to_owned(),
        ));
    }
    if request.assumptions.len() > 20
        || request
            .assumptions
            .iter()
            .any(|value| value.len() > 2_000 || value.chars().any(char::is_control))
        || request.operations.iter().any(|operation| {
            operation
                .target_id
                .as_ref()
                .is_some_and(|value| value.len() > 100 || value.chars().any(char::is_control))
                || operation.parameters.iter().any(|(key, value)| {
                    key.chars().any(char::is_control) || json_contains_unsafe_text(value, 0)
                })
        })
        || serde_json::to_vec(request).map_or(true, |value| value.len() > MAX_SIMULATION_BYTES)
    {
        return Err(SchedulingPortError::InvalidQuery(
            "simulation request exceeds the supported bounds".to_owned(),
        ));
    }
    Ok(())
}

fn json_contains_unsafe_text(value: &Value, depth: usize) -> bool {
    if depth > 64 {
        return true;
    }
    match value {
        Value::String(value) => value.chars().any(char::is_control),
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_unsafe_text(value, depth + 1)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            key.chars().any(char::is_control) || json_contains_unsafe_text(value, depth + 1)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn validate_simulation_token(token: &str) -> Result<(), SchedulingPortError> {
    let Some(payload) = token.strip_prefix("sim_") else {
        return Err(SchedulingPortError::NotFound);
    };
    if payload.len() != 43 || !payload.is_ascii() {
        return Err(SchedulingPortError::NotFound);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| SchedulingPortError::NotFound)?;
    if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(decoded) != payload {
        return Err(SchedulingPortError::NotFound);
    }
    Ok(())
}

fn subject_hash(subject: &str) -> Result<[u8; 32], SchedulingPortError> {
    if subject.is_empty() || subject.len() > 512 || subject.chars().any(char::is_control) {
        return Err(SchedulingPortError::NotFound);
    }
    let mut digest = Sha256::new();
    digest.update(b"dayweave.schedule-simulation-subject.v1\0");
    digest.update(subject.as_bytes());
    Ok(digest.finalize().into())
}

fn simulation_token_hash(token: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dayweave.schedule-simulation-token.v1\0");
    digest.update(token.as_bytes());
    digest.finalize().into()
}

fn proposal_subject_hash(subject: &str) -> Result<[u8; 32], SchedulingPortError> {
    if subject.is_empty() || subject.len() > 512 || subject.chars().any(char::is_control) {
        return Err(SchedulingPortError::NotFound);
    }
    let mut digest = Sha256::new();
    digest.update(b"dayweave.mcp-proposal-subject.v1\0");
    digest.update(subject.as_bytes());
    Ok(digest.finalize().into())
}

fn proposal_idempotency_key_hash(key: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dayweave.mcp-proposal-idempotency-key.v1\0");
    digest.update(key.as_bytes());
    digest.finalize().into()
}

fn proposal_payload_hash(payload: &Value) -> Result<[u8; 32], SchedulingPortError> {
    let encoded = serde_json::to_vec(payload).map_err(|_| {
        SchedulingPortError::Unavailable("proposal payload cannot be encoded".to_owned())
    })?;
    let mut digest = Sha256::new();
    digest.update(b"dayweave.mcp-proposal-payload.v1\0");
    digest.update(encoded);
    Ok(digest.finalize().into())
}

#[allow(clippy::too_many_arguments)]
fn simulation_evidence_hash(
    scope: DatabaseScope,
    simulation_id: Uuid,
    subject_hash: &[u8; 32],
    request_hash: &[u8; 32],
    base_revision_id: Uuid,
    base_revision_label: &str,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    snapshot: &Value,
) -> Result<[u8; 32], SchedulingPortError> {
    let commitment = json!({
        "workspace_id": scope.workspace_id,
        "user_id": scope.user_id,
        "simulation_id": simulation_id,
        "subject_hash": URL_SAFE_NO_PAD.encode(subject_hash),
        "request_hash": URL_SAFE_NO_PAD.encode(request_hash),
        "base_revision_id": base_revision_id,
        "base_revision_label": base_revision_label,
        "created_at": created_at,
        "expires_at": expires_at,
        "snapshot": snapshot,
    });
    let encoded = serde_json::to_vec(&commitment).map_err(|_| {
        SchedulingPortError::Unavailable("simulation evidence cannot be encoded".to_owned())
    })?;
    let mut digest = Sha256::new();
    digest.update(b"dayweave.mcp-simulation-evidence.v1\0");
    digest.update(encoded);
    Ok(digest.finalize().into())
}

fn validate_range(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<(), SchedulingPortError> {
    if !has_postgres_timestamp_precision(start) || !has_postgres_timestamp_precision(end) {
        return Err(SchedulingPortError::InvalidQuery(
            "date boundaries must use PostgreSQL microsecond precision".to_owned(),
        ));
    }
    if end <= start {
        return Err(SchedulingPortError::InvalidQuery(
            "end must be after start".to_owned(),
        ));
    }
    if end - start > Duration::days(90) {
        return Err(SchedulingPortError::InvalidQuery(
            "date range must not exceed 90 days".to_owned(),
        ));
    }
    Ok(())
}

fn validate_optional_instant(value: Option<DateTime<Utc>>) -> Result<(), SchedulingPortError> {
    if value.is_some_and(|value| !has_postgres_timestamp_precision(value)) {
        return Err(SchedulingPortError::InvalidQuery(
            "date boundaries must use PostgreSQL microsecond precision".to_owned(),
        ));
    }
    Ok(())
}

fn normalized_uuid_filter(
    name: &str,
    value: Option<&str>,
) -> Result<Option<String>, SchedulingPortError> {
    value
        .map(|value| {
            if value.len() > 100 || value.chars().any(char::is_control) {
                return Err(SchedulingPortError::InvalidQuery(format!(
                    "{name} is invalid"
                )));
            }
            Uuid::parse_str(value)
                .map(|id| id.to_string())
                .map_err(|_| SchedulingPortError::InvalidQuery(format!("{name} must be a UUID")))
        })
        .transpose()
}

fn goal_ids(value: &Value) -> Vec<String> {
    let Some(values) = value.get("goal_ids").and_then(Value::as_array) else {
        return Vec::new();
    };
    if values.len() > 100 {
        return Vec::new();
    }
    let parsed: Option<BTreeSet<_>> = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .map(|id| id.to_string())
        })
        .collect();
    parsed.unwrap_or_default().into_iter().collect()
}

fn issue(code: &str, message: &str, related_ids: Vec<String>) -> SimulationIssue {
    SimulationIssue {
        code: code.to_owned(),
        message: message.to_owned(),
        related_ids,
    }
}

fn block_kind_name(kind: ScheduleBlockKind) -> &'static str {
    match kind {
        ScheduleBlockKind::Planned => "planned",
        ScheduleBlockKind::Pinned => "pinned",
        ScheduleBlockKind::CalendarEvent => "calendar_event",
        ScheduleBlockKind::ExternalFixed => "external_fixed",
    }
}

fn operation_kind_name(kind: PlanOperationKind) -> &'static str {
    match kind {
        PlanOperationKind::CreateItem => "create_item",
        PlanOperationKind::UpdateItem => "update_item",
        PlanOperationKind::MoveBlock => "move_block",
        PlanOperationKind::CompleteItem => "complete_item",
        PlanOperationKind::DeleteItem => "delete_item",
        PlanOperationKind::UpdateConstraint => "update_constraint",
        PlanOperationKind::CreateEvent => "create_event",
        PlanOperationKind::GoalBreakdown => "goal_breakdown",
        PlanOperationKind::ReplaceSchedule => "replace_schedule",
    }
}

const fn operation_targets_item(kind: PlanOperationKind) -> bool {
    matches!(
        kind,
        PlanOperationKind::UpdateItem
            | PlanOperationKind::CompleteItem
            | PlanOperationKind::DeleteItem
            | PlanOperationKind::UpdateConstraint
            | PlanOperationKind::GoalBreakdown
    )
}

fn explanation_code_name(code: ExplanationCode) -> String {
    serde_name(&code).unwrap_or_else(|_| "unknown".to_owned())
}

fn serde_name<T: Serialize>(value: &T) -> Result<String, SchedulePublicationError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(SchedulePublicationError::InvalidPayload)
}

fn offset_to_chrono(
    value: time::OffsetDateTime,
) -> Result<DateTime<Utc>, SchedulePublicationError> {
    DateTime::from_timestamp(value.unix_timestamp(), value.nanosecond())
        .ok_or(SchedulePublicationError::InvalidPayload)
}

fn revision_label(number: i64, id: Uuid) -> String {
    format!("{number}:{id}")
}

fn encode_prefixed_sha256(value: &[u8; 32]) -> String {
    format!("sha256:{}", encode_hex(value))
}

pub(crate) fn decode_prefixed_sha256(value: &str) -> Option<[u8; 32]> {
    let bytes = decode_hex(value.strip_prefix("sha256:")?, 32)?;
    bytes.try_into().ok()
}

fn decode_hex(value: &str, expected_bytes: usize) -> Option<Vec<u8>> {
    if value.len() != expected_bytes * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Some((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn encode_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn storage_port(_error: impl std::fmt::Debug) -> SchedulingPortError {
    SchedulingPortError::Unavailable("canonical schedule storage is unavailable".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_digests_and_tokens_reject_noncanonical_encodings() {
        let digest = [7_u8; 32];
        let encoded = encode_prefixed_sha256(&digest);
        assert_eq!(decode_prefixed_sha256(&encoded), Some(digest));
        assert!(decode_prefixed_sha256(&encoded.to_uppercase()).is_none());
        assert!(decode_prefixed_sha256("sha256:00").is_none());

        let token = format!("sim_{}", URL_SAFE_NO_PAD.encode([8_u8; 32]));
        assert!(validate_simulation_token(&token).is_ok());
        assert!(validate_simulation_token("sim_not/canonical").is_err());
        assert!(validate_simulation_token(&format!("sim_{}", "A".repeat(10_000))).is_err());
    }

    #[test]
    fn core_block_kinds_are_persisted_without_collapsing_semantics() {
        assert_eq!(block_kind_name(ScheduleBlockKind::Planned), "planned");
        assert_eq!(block_kind_name(ScheduleBlockKind::Pinned), "pinned");
        assert_eq!(
            block_kind_name(ScheduleBlockKind::CalendarEvent),
            "calendar_event"
        );
        assert_eq!(
            block_kind_name(ScheduleBlockKind::ExternalFixed),
            "external_fixed"
        );
    }
}
