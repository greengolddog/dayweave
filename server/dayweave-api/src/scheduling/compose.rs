use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    ops::Deref,
};

use chrono::{DateTime, Utc};
use dayweave_compose::{
    CanonicalItem, CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy,
    ComposeScheduleRequest, FixedBlockSourceInput, IgnoredPreviousAssignment, MAX_CANONICAL_ITEMS,
    ManualPlacementInput, ManualPlacementReleaseInput, PrepareScheduleError, PreparedSchedule,
    PreviousAssignmentInput, RejectedScheduleItem, prepare_canonical_schedule,
    validate_schedule_request,
};
use dayweave_core::{
    ExecutionPlanningContext, ItemId, ManualPlacementViolationCode, OccurrenceId, PlanRequest,
    ScheduleBlockKind, ScheduleError, SchedulePlan, Scheduler,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::items::{
    Item, ItemKind, ItemQuery, ItemService, ItemServiceError, ItemStatus, SplitPolicy,
};

use super::{
    CalendarProjectionFenceError, CalendarProjectionStamp,
    postgres::{
        AuthoritativePlanningEvidence, ExecutionPlanningEvidenceError, PostgresSchedulingRepository,
    },
};

type AssignmentIdentity = (Uuid, Option<Uuid>);
type ManualBlockIdentity = (Uuid, Option<Uuid>, u16, i128, i128);
const MAX_MANUAL_ASSESSMENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ComposeScheduleResult {
    pub input_digest: String,
    pub source_item_count: usize,
    /// Exact active repository snapshot used to compose this response.
    ///
    /// Clients that pulled deltas immediately before previewing must compare this map with their
    /// cache. Item reads and preview composition are separate HTTP operations, so counts alone
    /// cannot detect a same-cardinality concurrent replacement.
    #[schema(value_type = Object)]
    pub source_item_revisions: BTreeMap<Uuid, u64>,
    /// Effective sensitivity, including sensitive ancestors, for the same
    /// exact canonical snapshot. Durable schedule readers use this evidence to
    /// redact conflicts and unscheduled work without reinterpreting history.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) source_item_sensitivity: BTreeMap<Uuid, bool>,
    /// Content-free Google Calendar generation evidence bound into the input
    /// digest and rechecked by durable publication. It remains internal so the
    /// strict macOS/Android response contract does not change.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) calendar_projection_stamps: Vec<CalendarProjectionStamp>,
    /// Full private execution and current-publication fence used by the
    /// scheduler. Publication compares it under the execution lock and stores
    /// it only inside the private durable snapshot.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) planning_evidence: AuthoritativePlanningEvidence,
    /// Exact manual proposal inputs retained only for durable publication
    /// evidence and per-block audit binding.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) manual_placements: Vec<ManualPlacementInput>,
    /// Exact explicit retained-pin releases retained for the private durable
    /// publication and audit boundary.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) manual_placement_releases: Vec<ManualPlacementReleaseInput>,
    /// Exact normalized scheduler input retained only in the private durable
    /// publication snapshot. Execution defer assessment reuses this policy so
    /// callers cannot weaken availability or constraints at approval time.
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) planning_request: PlanRequest,
    /// Canonical items accepted by this scheduler schema. This includes Inbox
    /// subtrees and retained nonblocking calendar context even though neither
    /// emits a work item or schedule block.
    pub accepted_item_count: usize,
    pub rejected_items: Vec<RejectedScheduleItem>,
    pub ignored_previous_assignments: Vec<IgnoredPreviousAssignment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_placement_assessments: Vec<ManualPlacementAssessmentOutput>,
    #[schema(value_type = Object)]
    pub plan: Rfc3339SchedulePlan,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementAssessmentOutput {
    pub placement_id: Uuid,
    pub environment_digest: String,
    pub approval_digest: String,
    pub approval_required: bool,
    pub violations: Vec<ManualPlacementViolationOutput>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementApproval {
    pub placement_id: Uuid,
    pub approval_digest: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementViolationOutput {
    #[schema(value_type = String)]
    pub code: ManualPlacementViolationCode,
    pub item_ids: Vec<Uuid>,
    pub occurrence_ids: Vec<Uuid>,
    pub conflicting_block_ids: Vec<Uuid>,
    pub conflicting_blocks: Vec<ManualPlacementConflictOutput>,
    pub start: String,
    pub end: String,
    pub boundary_start: Option<String>,
    pub boundary_end: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementConflictOutput {
    pub block_id: Uuid,
    pub item_id: Option<Uuid>,
    pub occurrence_id: Option<Uuid>,
    pub external_block_id: Option<Uuid>,
    #[schema(value_type = String)]
    pub kind: ScheduleBlockKind,
    pub start: String,
    pub end: String,
}

/// Owner-only, content-free recovery view for durable user placements.
/// Grouping is preserved so a client can release or replace exactly one
/// complete retained placement after losing its local journal.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetainedManualPlacementCatalog {
    pub current_schedule_revision_id: Option<Uuid>,
    pub placements: Vec<RetainedManualPlacementSummary>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetainedManualPlacementSummary {
    pub placement_id: Uuid,
    pub source_schedule_revision_id: Option<Uuid>,
    pub assignments: Vec<RetainedManualPlacementAssignmentSummary>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetainedManualPlacementAssignmentSummary {
    pub item_id: Uuid,
    pub published_item_revision: u64,
    pub occurrence_id: Option<Uuid>,
    pub blocks: Vec<dayweave_compose::PreviousBlockInput>,
}

#[derive(Serialize)]
struct ManualPlacementApprovalEvidence<'a> {
    schema: &'static str,
    input_digest: &'a str,
    environment_digest: &'a str,
    placement: &'a ManualPlacementInput,
    violations: &'a [dayweave_core::ManualPlacementViolation],
}

/// JSON-facing schedule plan with every instant encoded as RFC 3339.
///
/// The core engine keeps `time`'s native representation for non-JSON uses;
/// this wrapper makes the HTTP contract directly consumable by Swift/Kotlin
/// date decoders and avoids leaking crate-specific human-readable formatting.
#[derive(Debug, Clone)]
pub struct Rfc3339SchedulePlan(SchedulePlan);

impl Rfc3339SchedulePlan {
    #[must_use]
    pub fn into_inner(self) -> SchedulePlan {
        self.0
    }
}

impl Deref for Rfc3339SchedulePlan {
    type Target = SchedulePlan;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for Rfc3339SchedulePlan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let plan = &self.0;
        let output = SchedulePlanOutput {
            as_of: rfc3339(plan.as_of).map_err(serde::ser::Error::custom)?,
            horizon_start: rfc3339(plan.horizon_start).map_err(serde::ser::Error::custom)?,
            horizon_end: rfc3339(plan.horizon_end).map_err(serde::ser::Error::custom)?,
            blocks: plan
                .blocks
                .iter()
                .map(ScheduleBlockOutput::try_from)
                .collect::<Result<_, _>>()
                .map_err(serde::ser::Error::custom)?,
            unscheduled: &plan.unscheduled,
            decisions: &plan.decisions,
            violations: plan
                .violations
                .iter()
                .map(PlanViolationOutput::try_from)
                .collect::<Result<_, _>>()
                .map_err(serde::ser::Error::custom)?,
            score: &plan.score,
            occurrences: plan
                .occurrences
                .iter()
                .map(OccurrenceOutput::try_from)
                .collect::<Result<_, _>>()
                .map_err(serde::ser::Error::custom)?,
        };
        output.serialize(serializer)
    }
}

#[derive(Serialize)]
struct SchedulePlanOutput<'a> {
    as_of: String,
    horizon_start: String,
    horizon_end: String,
    blocks: Vec<ScheduleBlockOutput<'a>>,
    unscheduled: &'a [dayweave_core::UnscheduledWork],
    decisions: &'a [dayweave_core::PlanDecision],
    violations: Vec<PlanViolationOutput<'a>>,
    score: &'a dayweave_core::PlanScore,
    occurrences: Vec<OccurrenceOutput>,
}

#[derive(Serialize)]
struct ScheduleBlockOutput<'a> {
    id: Uuid,
    is_sensitive: bool,
    item_id: Option<ItemId>,
    occurrence_id: Option<OccurrenceId>,
    external_block_id: Option<Uuid>,
    title: &'a str,
    start: String,
    end: String,
    session_index: u16,
    kind: dayweave_core::ScheduleBlockKind,
    explanations: &'a [dayweave_core::PlacementExplanation],
}

impl<'a> TryFrom<&'a dayweave_core::ScheduleBlock> for ScheduleBlockOutput<'a> {
    type Error = time::error::Format;

    fn try_from(block: &'a dayweave_core::ScheduleBlock) -> Result<Self, Self::Error> {
        Ok(Self {
            id: block.id,
            is_sensitive: block.is_sensitive,
            item_id: block.item_id,
            occurrence_id: block.occurrence_id,
            external_block_id: block.external_block_id,
            title: &block.title,
            start: rfc3339(block.start)?,
            end: rfc3339(block.end)?,
            session_index: block.session_index,
            kind: block.kind,
            explanations: &block.explanations,
        })
    }
}

#[derive(Serialize)]
struct PlanViolationOutput<'a> {
    kind: dayweave_core::ViolationKind,
    severity: dayweave_core::ViolationSeverity,
    item_ids: &'a [ItemId],
    occurrence_ids: &'a [OccurrenceId],
    start: Option<String>,
    end: Option<String>,
    penalty: u64,
    message: &'a str,
}

impl<'a> TryFrom<&'a dayweave_core::PlanViolation> for PlanViolationOutput<'a> {
    type Error = time::error::Format;

    fn try_from(violation: &'a dayweave_core::PlanViolation) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: violation.kind,
            severity: violation.severity,
            item_ids: &violation.item_ids,
            occurrence_ids: &violation.occurrence_ids,
            start: violation.start.map(rfc3339).transpose()?,
            end: violation.end.map(rfc3339).transpose()?,
            penalty: violation.penalty,
            message: &violation.message,
        })
    }
}

#[derive(Serialize)]
struct OccurrenceOutput {
    id: OccurrenceId,
    series_item_id: ItemId,
    identity: dayweave_core::RecurrenceOccurrenceIdentity,
    nominal_start: String,
    nominal_end: String,
    window_start: String,
    window_end: String,
    local_date: Option<String>,
    ordinal: u32,
    state: dayweave_core::OccurrenceState,
}

impl TryFrom<&dayweave_core::Occurrence> for OccurrenceOutput {
    type Error = time::error::Format;

    fn try_from(occurrence: &dayweave_core::Occurrence) -> Result<Self, Self::Error> {
        Ok(Self {
            id: occurrence.id,
            series_item_id: occurrence.series_item_id,
            identity: occurrence.identity,
            nominal_start: rfc3339(occurrence.nominal_start)?,
            nominal_end: rfc3339(occurrence.nominal_end)?,
            window_start: rfc3339(occurrence.window_start)?,
            window_end: rfc3339(occurrence.window_end)?,
            local_date: occurrence.local_date.map(|date| date.to_string()),
            ordinal: occurrence.ordinal,
            state: occurrence.state,
        })
    }
}

#[derive(Debug, Error)]
pub enum ComposeScheduleError {
    #[error("invalid schedule preview request: {0}")]
    InvalidRequest(String),
    #[error("canonical item count exceeds the supported limit of {MAX_CANONICAL_ITEMS}")]
    TooManyItems,
    #[error(transparent)]
    ItemService(#[from] ItemServiceError),
    #[error("schedule engine rejected the composed input: {0}")]
    Scheduler(#[from] ScheduleError),
    #[error("schedule preview input could not be encoded")]
    Encoding,
    #[error("selected Google Calendar projection does not cover the requested horizon")]
    CalendarProjectionIncomplete,
    #[error("Google Calendar projection evidence is temporarily unavailable")]
    CalendarProjectionUnavailable,
    #[error("execution or published-assignment evidence changed during preview")]
    ExecutionEvidenceChanged,
    #[error("execution planning evidence is temporarily unavailable")]
    ExecutionEvidenceUnavailable,
    #[error("authoritative manual placement evidence changed: {0}")]
    AuthoritativeManualPlacementChanged(String),
}

impl ComposeScheduleError {
    #[must_use]
    pub const fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidRequest(_)
                | Self::TooManyItems
                | Self::Scheduler(_)
                | Self::AuthoritativeManualPlacementChanged(_)
        )
    }
}

/// Loads the canonical item graph and computes a deterministic, side-effect-free preview.
///
/// Invalid legacy item metadata is isolated in `rejected_items`; malformed preview inputs
/// fail the entire request so a caller cannot mistake partial request interpretation for a
/// valid plan.
///
/// # Errors
///
/// Returns an error when canonical storage is unavailable, request bounds or references are
/// invalid, the item count is unsafe, input encoding fails, or the deterministic scheduler
/// rejects the composed graph.
pub async fn compose_canonical_schedule(
    service: &ItemService,
    projection: &PostgresSchedulingRepository,
    request: ComposeScheduleRequest,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    compose_canonical_schedule_inner(service, Some(projection), request).await
}

/// Explicit in-memory composition path for deployments without `PostgreSQL` or
/// Google Calendar projection state. Production `PostgreSQL` callers must use
/// [`compose_canonical_schedule`] so Calendar capacity cannot be bypassed.
pub(crate) async fn compose_canonical_schedule_unfenced(
    service: &ItemService,
    request: ComposeScheduleRequest,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    compose_canonical_schedule_inner(service, None, request).await
}

/// Composes against a stable canonical-item and Calendar-generation snapshot.
/// Generation evidence is read on both sides of the item list so a committed
/// projection/configuration change cannot produce a mixed preview.
async fn compose_canonical_schedule_inner(
    service: &ItemService,
    projection: Option<&PostgresSchedulingRepository>,
    mut request: ComposeScheduleRequest,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    validate_schedule_request(&request).map_err(map_prepare_error)?;
    let planning_before = match projection {
        Some(projection) => projection
            .authoritative_planning_evidence()
            .await
            .map_err(map_execution_evidence_error)?,
        None => AuthoritativePlanningEvidence::default(),
    };
    let projection_before = match projection {
        Some(projection) => projection
            .calendar_projection_stamps(request.horizon_start, request.horizon_end)
            .await
            .map_err(map_projection_fence_error)?,
        None => Vec::new(),
    };
    let items = service
        .list(ItemQuery {
            parent_id: None,
            include_deleted: false,
            limit: MAX_CANONICAL_ITEMS + 1,
        })
        .await?;
    if items.len() > MAX_CANONICAL_ITEMS {
        return Err(ComposeScheduleError::TooManyItems);
    }
    let projection_after = match projection {
        Some(projection) => projection
            .calendar_projection_stamps(request.horizon_start, request.horizon_end)
            .await
            .map_err(map_projection_fence_error)?,
        None => Vec::new(),
    };
    if projection_before != projection_after {
        return Err(ComposeScheduleError::CalendarProjectionIncomplete);
    }
    let planning_after = match projection {
        Some(projection) => projection
            .authoritative_planning_evidence()
            .await
            .map_err(map_execution_evidence_error)?,
        None => AuthoritativePlanningEvidence::default(),
    };
    if planning_before != planning_after {
        return Err(ComposeScheduleError::ExecutionEvidenceChanged);
    }
    normalize_manual_placements_for_execution(&mut request, &planning_before.execution)?;
    validate_manual_placement_item_revisions(&request, &items)?;
    validate_manual_placement_sources(&request, &planning_before)?;
    let pruned_assignment_identities =
        merge_retained_manual_placements(&mut request, &planning_before, &items)?;
    let (retained_placement_was_injected, manual_execution_evidence_was_applied) =
        manual_placement_staleness_flags(&request, &planning_before);
    let untrusted_assignments =
        replace_with_authoritative_assignments(&mut request, &planning_before)?;
    request.previous_assignments.retain(|assignment| {
        !pruned_assignment_identities.contains(&(assignment.item_id, assignment.occurrence_id))
    });
    if let Err(error) = validate_schedule_request(&request) {
        if retained_placement_was_injected {
            return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                "retained placement evidence no longer satisfies the compose request".to_owned(),
            ));
        }
        return Err(map_prepare_error(error));
    }
    compose_items_with_projection_for_schema(
        items,
        request,
        super::SCHEDULER_PUBLICATION_SCHEMA,
        projection_before,
        planning_before,
        untrusted_assignments,
    )
    .map_err(|error| {
        if (retained_placement_was_injected || manual_execution_evidence_was_applied)
            && matches!(
                error,
                ComposeScheduleError::InvalidRequest(_) | ComposeScheduleError::Scheduler(_)
            )
        {
            ComposeScheduleError::AuthoritativeManualPlacementChanged(
                "a retained placement no longer composes against current scheduling evidence"
                    .to_owned(),
            )
        } else {
            error
        }
    })
}

fn manual_placement_staleness_flags(
    request: &ComposeScheduleRequest,
    evidence: &AuthoritativePlanningEvidence,
) -> (bool, bool) {
    let retained_was_injected = request.manual_placements.iter().any(|placement| {
        evidence
            .retained_manual_placements
            .iter()
            .any(|state| state.placement.id == placement.id)
    });
    let execution_was_applied = request
        .manual_placements
        .iter()
        .flat_map(|placement| &placement.assignments)
        .any(|assignment| {
            evidence.execution.work_units.iter().any(|unit| {
                unit.item_id.0 == assignment.item_id
                    && unit.occurrence_id.map(|occurrence| occurrence.0) == assignment.occurrence_id
            })
        });
    (retained_was_injected, execution_was_applied)
}

fn validate_manual_placement_sources(
    request: &ComposeScheduleRequest,
    evidence: &AuthoritativePlanningEvidence,
) -> Result<(), ComposeScheduleError> {
    let (retained_ids, retained_by_identity) = retained_manual_placement_indexes(evidence)?;
    let released_placement_ids = request
        .manual_placement_releases
        .iter()
        .map(|release| release.placement_id)
        .collect::<BTreeSet<_>>();
    for release in &request.manual_placement_releases {
        if Some(release.source_schedule_revision_id) != evidence.published_revision_id
            || !retained_ids.contains(&release.placement_id)
        {
            return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                format!(
                    "manual placement release {} is stale or does not target a retained placement",
                    release.id
                ),
            ));
        }
    }
    let published_by_identity: BTreeMap<_, _> = evidence
        .previous_assignments
        .iter()
        .map(|assignment| ((assignment.item_id, assignment.occurrence_id), assignment))
        .collect();
    for placement in &request.manual_placements {
        if placement.source_schedule_revision_id != evidence.published_revision_id {
            return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                format!(
                    "manual placement {} is based on a stale published schedule",
                    placement.id
                ),
            ));
        }
        for assignment in &placement.assignments {
            let Some(source) =
                published_by_identity.get(&(assignment.item_id, assignment.occurrence_id))
            else {
                continue;
            };
            let high_water = execution_high_water_for_identity(
                &evidence.execution,
                assignment.item_id,
                assignment.occurrence_id,
            );
            let source_shape: BTreeMap<_, _> = source
                .blocks
                .iter()
                .filter(|block| high_water.is_none_or(|high| block.session_index > high))
                .map(|block| {
                    (
                        block.session_index,
                        block.end.signed_duration_since(block.start),
                    )
                })
                .collect();
            let requested_shape: BTreeMap<_, _> = assignment
                .blocks
                .iter()
                .map(|block| {
                    (
                        block.session_index,
                        block.end.signed_duration_since(block.start),
                    )
                })
                .collect();
            let remaining_source_count = source
                .blocks
                .iter()
                .filter(|block| high_water.is_none_or(|high| block.session_index > high))
                .count();
            let source_is_unique = source_shape.len() == remaining_source_count;
            let request_is_unique = requested_shape.len() == assignment.blocks.len();
            let preserves_every_source_session = source_shape
                .iter()
                .all(|(index, duration)| requested_shape.get(index) == Some(duration));
            let retained_source =
                retained_by_identity.get(&(assignment.item_id, assignment.occurrence_id));
            let shape_is_authorized = retained_source.map_or_else(
                || {
                    source_is_unique
                        && request_is_unique
                        && (preserves_every_source_session
                            || source.item_revision != assignment.item_revision
                            || execution_credit_changes_source_shape(&evidence.execution, source))
                },
                |placement_id| {
                    released_placement_ids.contains(placement_id)
                        || (source_is_unique
                            && request_is_unique
                            && source_shape == requested_shape)
                },
            );
            if !shape_is_authorized {
                return Err(ComposeScheduleError::InvalidRequest(format!(
                    "manual placement {} must preserve every remaining source session and duration",
                    placement.id
                )));
            }
        }
    }
    Ok(())
}

fn execution_credit_changes_source_shape(
    execution: &ExecutionPlanningContext,
    source: &PreviousAssignmentInput,
) -> bool {
    let Some(unit) = execution.work_units.iter().find(|unit| {
        unit.item_id.0 == source.item_id
            && unit.occurrence_id.map(|occurrence| occurrence.0) == source.occurrence_id
    }) else {
        return false;
    };
    let Some(high_water) = execution_unit_high_water(unit) else {
        return false;
    };
    let consumed_source_minutes = source
        .blocks
        .iter()
        .filter(|block| block.session_index <= high_water)
        .try_fold(0_u64, |total, block| {
            let seconds = block.end.signed_duration_since(block.start).num_seconds();
            u64::try_from(seconds)
                .ok()
                .map(|seconds| total.saturating_add(seconds.saturating_add(59) / 60))
        });
    let credited_minutes = unit.credited_seconds.saturating_add(59) / 60;
    consumed_source_minutes != Some(credited_minutes)
}

fn normalize_manual_placements_for_execution(
    request: &mut ComposeScheduleRequest,
    execution: &ExecutionPlanningContext,
) -> Result<(), ComposeScheduleError> {
    for assignment in request
        .manual_placements
        .iter_mut()
        .flat_map(|placement| &mut placement.assignments)
    {
        let Some(unit) = execution.work_units.iter().find(|unit| {
            unit.item_id.0 == assignment.item_id
                && unit.occurrence_id.map(|occurrence| occurrence.0) == assignment.occurrence_id
        }) else {
            continue;
        };
        if !unit.reservations.is_empty() {
            return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                format!(
                    "item {} now has an active execution reservation",
                    assignment.item_id
                ),
            ));
        }
        if unit.disposition.is_some() {
            return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                format!("item {} is no longer actionable", assignment.item_id),
            ));
        }
        if let Some(high_water) = execution_unit_high_water(unit) {
            assignment
                .blocks
                .retain(|block| block.session_index > high_water);
            if assignment.blocks.is_empty() {
                return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                    format!(
                        "item {} has no remaining requested sessions after execution",
                        assignment.item_id
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn execution_high_water_for_identity(
    execution: &ExecutionPlanningContext,
    item_id: Uuid,
    occurrence_id: Option<Uuid>,
) -> Option<u16> {
    execution
        .work_units
        .iter()
        .find(|unit| {
            unit.item_id.0 == item_id
                && unit.occurrence_id.map(|occurrence| occurrence.0) == occurrence_id
        })
        .and_then(execution_unit_high_water)
}

fn execution_unit_high_water(unit: &dayweave_core::ExecutionWorkUnit) -> Option<u16> {
    unit.used_session_indices
        .iter()
        .copied()
        .chain(
            unit.reservations
                .iter()
                .map(|reservation| reservation.session_index),
        )
        .max()
}

fn validate_manual_placement_item_revisions(
    request: &ComposeScheduleRequest,
    items: &[Item],
) -> Result<(), ComposeScheduleError> {
    let revisions = items
        .iter()
        .map(|item| (item.id, item.revision))
        .collect::<BTreeMap<_, _>>();
    for assignment in request
        .manual_placements
        .iter()
        .flat_map(|placement| &placement.assignments)
    {
        if revisions.get(&assignment.item_id) != Some(&assignment.item_revision) {
            return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
                format!(
                    "item {} no longer has revision {}",
                    assignment.item_id, assignment.item_revision
                ),
            ));
        }
    }
    Ok(())
}

fn retained_manual_placement_indexes(
    evidence: &AuthoritativePlanningEvidence,
) -> Result<(BTreeSet<Uuid>, BTreeMap<AssignmentIdentity, Uuid>), ComposeScheduleError> {
    let retained_ids = evidence
        .retained_manual_placements
        .iter()
        .map(|state| state.placement.id)
        .collect::<BTreeSet<_>>();
    if retained_ids.len() != evidence.retained_manual_placements.len() {
        return Err(ComposeScheduleError::InvalidRequest(
            "retained manual placement evidence is inconsistent".to_owned(),
        ));
    }
    let retained_by_identity = evidence
        .retained_manual_placements
        .iter()
        .flat_map(|state| {
            state.placement.assignments.iter().map(move |assignment| {
                (
                    (assignment.item_id, assignment.occurrence_id),
                    state.placement.id,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let assignment_count = evidence
        .retained_manual_placements
        .iter()
        .map(|state| state.placement.assignments.len())
        .sum::<usize>();
    if retained_by_identity.len() != assignment_count {
        return Err(ComposeScheduleError::InvalidRequest(
            "retained manual placement assignments are inconsistent".to_owned(),
        ));
    }
    Ok((retained_ids, retained_by_identity))
}

fn merge_retained_manual_placements(
    request: &mut ComposeScheduleRequest,
    evidence: &AuthoritativePlanningEvidence,
    items: &[Item],
) -> Result<BTreeSet<AssignmentIdentity>, ComposeScheduleError> {
    if evidence.retained_manual_placements.is_empty() {
        return Ok(BTreeSet::new());
    }
    let current_items: BTreeMap<_, _> = items.iter().map(|item| (item.id, item)).collect();
    let plannable_item_ids = canonical_plannable_item_ids(request, items)?;
    let retained_ids: BTreeSet<_> = evidence
        .retained_manual_placements
        .iter()
        .map(|state| state.placement.id)
        .collect();
    if request
        .manual_placements
        .iter()
        .any(|placement| retained_ids.contains(&placement.id))
    {
        return Err(ComposeScheduleError::InvalidRequest(
            "a replacement manual placement must use a fresh id".to_owned(),
        ));
    }
    let released_ids = request
        .manual_placement_releases
        .iter()
        .map(|release| release.placement_id)
        .collect::<BTreeSet<_>>();

    let requested_identities = request
        .manual_placements
        .iter()
        .map(|placement| {
            (
                placement.id,
                placement
                    .assignments
                    .iter()
                    .map(|assignment| (assignment.item_id, assignment.occurrence_id))
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let earliest = request.as_of.max(request.horizon_start);
    let mut pruned_identities = BTreeSet::new();
    for state in &evidence.retained_manual_placements {
        let retained_identities = state
            .placement
            .assignments
            .iter()
            .map(|assignment| (assignment.item_id, assignment.occurrence_id))
            .collect::<BTreeSet<_>>();
        let replacements = requested_identities
            .iter()
            .filter(|(_, identities)| !identities.is_disjoint(&retained_identities))
            .collect::<Vec<_>>();
        if !replacements.is_empty() {
            if replacements.len() != 1 || replacements[0].1 != retained_identities {
                return Err(ComposeScheduleError::InvalidRequest(format!(
                    "manual placement {} must be replaced as one complete assignment group",
                    state.placement.id
                )));
            }
            pruned_identities.extend(retained_identities);
            continue;
        }
        if released_ids.contains(&state.placement.id) {
            pruned_identities.extend(retained_identities);
            continue;
        }

        if !retained_placement_is_actionable(&state.placement, &current_items, &plannable_item_ids)
        {
            pruned_identities.extend(retained_identities);
            continue;
        }
        if retained_placement_has_new_execution_claim(&state.placement, &evidence.execution) {
            pruned_identities.extend(retained_identities);
            continue;
        }

        let mut carried = state.placement.clone();
        if !retained_placement_is_inside_horizon(&carried, request, earliest)? {
            pruned_identities.extend(retained_identities);
            continue;
        }
        for assignment in &mut carried.assignments {
            assignment.item_revision = current_items[&assignment.item_id].revision;
        }
        request.manual_placements.push(carried);
    }
    Ok(pruned_identities)
}

fn retained_placement_is_inside_horizon(
    placement: &ManualPlacementInput,
    request: &ComposeScheduleRequest,
    earliest: DateTime<Utc>,
) -> Result<bool, ComposeScheduleError> {
    let overlaps_horizon = placement.assignments.iter().any(|assignment| {
        assignment
            .blocks
            .iter()
            .any(|block| block.end > earliest && block.start < request.horizon_end)
    });
    if !overlaps_horizon {
        let wholly_expired = placement.assignments.iter().all(|assignment| {
            assignment
                .blocks
                .iter()
                .all(|block| block.end <= request.as_of)
        });
        if wholly_expired {
            return Ok(false);
        }
        return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
            format!(
                "retained manual placement {} is outside the current planning horizon; expand the horizon or explicitly release it",
                placement.id
            ),
        ));
    }
    if placement.assignments.iter().any(|assignment| {
        assignment
            .blocks
            .iter()
            .any(|block| block.start < earliest || block.end > request.horizon_end)
    }) {
        return Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(
            format!(
                "retained manual placement {} crosses the current planning boundary",
                placement.id
            ),
        ));
    }
    Ok(true)
}

fn retained_placement_is_actionable(
    placement: &ManualPlacementInput,
    current_items: &BTreeMap<Uuid, &Item>,
    plannable_item_ids: &BTreeSet<Uuid>,
) -> bool {
    placement.assignments.iter().all(|assignment| {
        current_items.get(&assignment.item_id).is_some_and(|item| {
            plannable_item_ids.contains(&item.id)
                && !item_is_suppressed_by_hierarchy(item.id, current_items)
                && !item.status.is_terminal()
                && item.status != ItemStatus::InProgress
                && item.is_executable
                && item.duration_seconds.is_some()
        })
    })
}

fn canonical_plannable_item_ids(
    request: &ComposeScheduleRequest,
    items: &[Item],
) -> Result<BTreeSet<Uuid>, ComposeScheduleError> {
    let mut eligibility_request = request.clone();
    eligibility_request.previous_assignments.clear();
    eligibility_request.manual_placements.clear();
    eligibility_request.manual_placement_releases.clear();
    let prepared = prepare_canonical_schedule(
        items.iter().cloned().map(into_canonical_item).collect(),
        eligibility_request,
    )
    .map_err(map_prepare_error)?;
    Ok(prepared
        .plan_request
        .items
        .into_iter()
        .map(|item| item.id.0)
        .collect())
}

fn item_is_suppressed_by_hierarchy(item_id: Uuid, current_items: &BTreeMap<Uuid, &Item>) -> bool {
    let mut cursor = Some(item_id);
    let mut visited = BTreeSet::new();
    while let Some(current_id) = cursor {
        if !visited.insert(current_id) {
            return true;
        }
        let Some(item) = current_items.get(&current_id) else {
            return true;
        };
        if item.status == ItemStatus::Inbox {
            return true;
        }
        cursor = item.parent_id;
    }
    false
}

fn retained_placement_has_new_execution_claim(
    placement: &ManualPlacementInput,
    execution: &ExecutionPlanningContext,
) -> bool {
    placement.assignments.iter().any(|assignment| {
        execution
            .work_units
            .iter()
            .find(|unit| {
                unit.item_id.0 == assignment.item_id
                    && unit.occurrence_id.map(|occurrence| occurrence.0) == assignment.occurrence_id
            })
            .is_some_and(|unit| {
                !unit.reservations.is_empty()
                    || unit.disposition.is_some()
                    || unit
                        .used_session_indices
                        .iter()
                        .max()
                        .is_some_and(|high_water| {
                            assignment
                                .blocks
                                .iter()
                                .any(|block| block.session_index <= *high_water)
                        })
            })
    })
}

pub(crate) fn retained_manual_placement_catalog(
    evidence: &AuthoritativePlanningEvidence,
) -> RetainedManualPlacementCatalog {
    let mut placements = evidence
        .retained_manual_placements
        .iter()
        .map(|state| {
            let mut assignments = state
                .placement
                .assignments
                .iter()
                .map(|assignment| {
                    let mut blocks = assignment.blocks.clone();
                    blocks.sort_by_key(|block| (block.session_index, block.start, block.end));
                    RetainedManualPlacementAssignmentSummary {
                        item_id: assignment.item_id,
                        published_item_revision: assignment.item_revision,
                        occurrence_id: assignment.occurrence_id,
                        blocks,
                    }
                })
                .collect::<Vec<_>>();
            assignments.sort_by_key(|assignment| {
                (
                    assignment.item_id,
                    assignment.occurrence_id,
                    assignment.published_item_revision,
                )
            });
            RetainedManualPlacementSummary {
                placement_id: state.placement.id,
                source_schedule_revision_id: state.placement.source_schedule_revision_id,
                assignments,
            }
        })
        .collect::<Vec<_>>();
    placements.sort_by_key(|placement| placement.placement_id);
    RetainedManualPlacementCatalog {
        current_schedule_revision_id: evidence.published_revision_id,
        placements,
    }
}

const fn map_projection_fence_error(error: CalendarProjectionFenceError) -> ComposeScheduleError {
    match error {
        CalendarProjectionFenceError::Incomplete => {
            ComposeScheduleError::CalendarProjectionIncomplete
        }
        CalendarProjectionFenceError::Unavailable => {
            ComposeScheduleError::CalendarProjectionUnavailable
        }
    }
}

const fn map_execution_evidence_error(
    _error: ExecutionPlanningEvidenceError,
) -> ComposeScheduleError {
    ComposeScheduleError::ExecutionEvidenceUnavailable
}

#[cfg(test)]
fn compose_items(
    source_items: Vec<Item>,
    request: ComposeScheduleRequest,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    compose_items_for_schema(source_items, request, super::SCHEDULER_PUBLICATION_SCHEMA)
}

#[cfg(test)]
fn compose_items_for_schema(
    source_items: Vec<Item>,
    mut request: ComposeScheduleRequest,
    scheduler_publication_schema: &str,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    let planning_evidence = AuthoritativePlanningEvidence {
        execution: ExecutionPlanningContext::default(),
        published_revision_id: None,
        previous_assignments: request.previous_assignments.clone(),
        retained_manual_placements: Vec::new(),
    };
    let ignored = replace_with_authoritative_assignments(&mut request, &planning_evidence)?;
    compose_items_with_projection_for_schema(
        source_items,
        request,
        scheduler_publication_schema,
        Vec::new(),
        planning_evidence,
        ignored,
    )
}

fn compose_items_with_projection_for_schema(
    source_items: Vec<Item>,
    request: ComposeScheduleRequest,
    scheduler_publication_schema: &str,
    calendar_projection_stamps: Vec<CalendarProjectionStamp>,
    planning_evidence: AuthoritativePlanningEvidence,
    untrusted_assignments: Vec<IgnoredPreviousAssignment>,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    if !calendar_projection_stamps.is_empty()
        && request
            .fixed_blocks
            .iter()
            .any(|block| matches!(block.source, FixedBlockSourceInput::GoogleCalendar))
    {
        return Err(ComposeScheduleError::InvalidRequest(
            "caller-supplied Google Calendar fixed blocks cannot be combined with the authoritative Calendar projection"
                .to_owned(),
        ));
    }
    let source_items = source_items.into_iter().map(into_canonical_item).collect();
    let prepared = prepare_canonical_schedule(source_items, request).map_err(map_prepare_error)?;
    compose_prepared_for_schema(
        prepared,
        scheduler_publication_schema,
        calendar_projection_stamps,
        planning_evidence,
        untrusted_assignments,
    )
}

fn compose_prepared_for_schema(
    prepared: PreparedSchedule,
    scheduler_publication_schema: &str,
    calendar_projection_stamps: Vec<CalendarProjectionStamp>,
    planning_evidence: AuthoritativePlanningEvidence,
    untrusted_assignments: Vec<IgnoredPreviousAssignment>,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    let PreparedSchedule {
        timezone_name,
        source_item_count,
        source_item_revisions,
        effective_sensitivity,
        accepted_item_count,
        rejected_items,
        mut ignored_previous_assignments,
        manual_placements,
        manual_placement_releases,
        plan_request,
    } = prepared;
    ignored_previous_assignments.extend(untrusted_assignments);
    let input_digest = request_digest(
        scheduler_publication_schema,
        &timezone_name,
        &source_item_revisions,
        &calendar_projection_stamps,
        &planning_evidence.execution,
        &plan_request,
    )?;
    let plan = Scheduler.plan_with_execution(&plan_request, &planning_evidence.execution)?;
    let manual_placement_assessments = build_manual_placement_assessments(
        &input_digest,
        &manual_placements,
        &plan,
        &planning_evidence.retained_manual_placements,
    )?;
    let result = ComposeScheduleResult {
        input_digest,
        source_item_count,
        source_item_revisions,
        source_item_sensitivity: effective_sensitivity,
        calendar_projection_stamps,
        planning_evidence,
        manual_placements,
        manual_placement_releases,
        planning_request: plan_request.clone(),
        accepted_item_count,
        rejected_items,
        ignored_previous_assignments,
        manual_placement_assessments,
        plan: Rfc3339SchedulePlan(plan),
    };
    let validation = if scheduler_publication_schema == super::SCHEDULER_PUBLICATION_SCHEMA {
        super::postgres::validate_publishable_compose_result(&timezone_name, &result).map(|_| ())
    } else {
        super::postgres::validate_composed_result_for_schema(
            scheduler_publication_schema,
            &timezone_name,
            &result,
        )
    };
    validation.map_err(|_| {
        ComposeScheduleError::InvalidRequest(
            "composed schedule exceeds the durable publication contract".to_owned(),
        )
    })?;
    Ok(result)
}

fn into_canonical_item(item: Item) -> CanonicalItem {
    CanonicalItem {
        id: item.id,
        is_sensitive: item.is_sensitive,
        kind: match item.kind {
            ItemKind::Event => CanonicalItemKind::Event,
            ItemKind::Task => CanonicalItemKind::Task,
            ItemKind::Habit => CanonicalItemKind::Habit,
            ItemKind::Routine => CanonicalItemKind::Routine,
            ItemKind::Goal => CanonicalItemKind::Goal,
            ItemKind::Break => CanonicalItemKind::Break,
        },
        status: match item.status {
            ItemStatus::Inbox => CanonicalItemStatus::Inbox,
            ItemStatus::Planned => CanonicalItemStatus::Planned,
            ItemStatus::Scheduled => CanonicalItemStatus::Scheduled,
            ItemStatus::InProgress => CanonicalItemStatus::InProgress,
            ItemStatus::Paused => CanonicalItemStatus::Paused,
            ItemStatus::Completed => CanonicalItemStatus::Completed,
            ItemStatus::Skipped => CanonicalItemStatus::Skipped,
            ItemStatus::Cancelled => CanonicalItemStatus::Cancelled,
        },
        title: item.title,
        notes: item.notes,
        timezone_name: item.timezone_name,
        duration_seconds: item.duration_seconds,
        deadline_at: item.deadline_at,
        earliest_start_at: item.earliest_start_at,
        recurrence: item.recurrence,
        flexible_constraints: item.flexible_constraints,
        split_policy: match item.split_policy {
            SplitPolicy::Indivisible => CanonicalSplitPolicy::Indivisible,
            SplitPolicy::Splittable {
                minimum_chunk_seconds,
                maximum_chunk_seconds,
            } => CanonicalSplitPolicy::Splittable {
                minimum_chunk_seconds,
                maximum_chunk_seconds,
            },
        },
        importance: item.importance,
        urgency: item.urgency,
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
        is_executable: item.is_executable,
        revision: item.revision,
        created_at: item.created_at,
        updated_at: item.updated_at,
        completed_at: item.completed_at,
        deleted_at: item.deleted_at,
    }
}

fn map_prepare_error(error: PrepareScheduleError) -> ComposeScheduleError {
    match error {
        PrepareScheduleError::InvalidRequest(message) => {
            ComposeScheduleError::InvalidRequest(message)
        }
        PrepareScheduleError::TooManyItems => ComposeScheduleError::TooManyItems,
        PrepareScheduleError::DuplicateCanonicalItem(_)
        | PrepareScheduleError::InvalidCanonicalItem(_)
        | PrepareScheduleError::AccountingOverflow => ComposeScheduleError::Encoding,
    }
}

fn replace_with_authoritative_assignments(
    request: &mut ComposeScheduleRequest,
    evidence: &AuthoritativePlanningEvidence,
) -> Result<Vec<IgnoredPreviousAssignment>, ComposeScheduleError> {
    let requested = std::mem::take(&mut request.previous_assignments);
    let mut requested_identities = BTreeSet::new();
    let trusted_by_identity: BTreeMap<_, _> = evidence
        .previous_assignments
        .iter()
        .map(|assignment| ((assignment.item_id, assignment.occurrence_id), assignment))
        .collect();
    let mut ignored = Vec::new();
    for assignment in requested {
        let identity = (assignment.item_id, assignment.occurrence_id);
        if !requested_identities.insert(identity) {
            return Err(ComposeScheduleError::InvalidRequest(format!(
                "duplicate previous assignment for item {} and occurrence",
                assignment.item_id
            )));
        }
        if trusted_by_identity.get(&identity).copied() == Some(&assignment) {
            continue;
        }
        ignored.push(IgnoredPreviousAssignment {
            item_id: assignment.item_id,
            requested_revision: assignment.item_revision,
            current_revision: trusted_by_identity
                .get(&identity)
                .map(|trusted| trusted.item_revision),
            reason: "not an exact current published assignment".to_owned(),
        });
    }
    request
        .previous_assignments
        .clone_from(&evidence.previous_assignments);
    Ok(ignored)
}

pub(super) fn build_manual_placement_assessments(
    input_digest: &str,
    placements: &[ManualPlacementInput],
    plan: &SchedulePlan,
    retained: &[super::postgres::PersistedManualPlacementState],
) -> Result<Vec<ManualPlacementAssessmentOutput>, ComposeScheduleError> {
    let exact_blocks = plan
        .blocks
        .iter()
        .filter_map(|block| {
            (block.kind == ScheduleBlockKind::Pinned).then_some((
                block.item_id?.0,
                block.occurrence_id.map(|occurrence| occurrence.0),
                block.session_index,
                block.start.unix_timestamp_nanos(),
                block.end.unix_timestamp_nanos(),
            ))
        })
        .collect::<BTreeSet<_>>();
    let core_by_id: BTreeMap<_, _> = plan
        .manual_placement_assessments
        .iter()
        .map(|assessment| (assessment.placement_id, assessment))
        .collect();
    if core_by_id.len() != placements.len() {
        return Err(ComposeScheduleError::InvalidRequest(
            "manual placements did not materialize exactly".to_owned(),
        ));
    }
    let mut result = Vec::with_capacity(placements.len());
    for placement in placements {
        let assessment = core_by_id.get(&placement.id).copied().ok_or_else(|| {
            ComposeScheduleError::InvalidRequest(
                "manual placement did not bind to scheduled work".to_owned(),
            )
        })?;
        validate_exact_manual_placement(placement, &exact_blocks)?;
        let (environment_digest, approval_digest) =
            manual_placement_digests(input_digest, placement, assessment)?;
        let violations = map_manual_placement_violations(&assessment.violations)?;
        let was_authorized = retained
            .iter()
            .find(|state| retained_placement_covers(placement, &state.placement))
            .is_some_and(|state| {
                state.environment_digest == environment_digest
                    && violations
                        .iter()
                        .all(|violation| state.authorized_violations.contains(violation))
            });
        result.push(ManualPlacementAssessmentOutput {
            placement_id: placement.id,
            environment_digest,
            approval_digest,
            approval_required: !violations.is_empty() && !was_authorized,
            violations,
        });
    }
    result.sort_by_key(|assessment| assessment.placement_id);
    let encoded_size = serde_json::to_vec(&result)
        .map_err(|_| ComposeScheduleError::Encoding)?
        .len();
    if encoded_size > MAX_MANUAL_ASSESSMENT_BYTES {
        return Err(ComposeScheduleError::InvalidRequest(
            "manual placement assessment exceeds the supported evidence size".to_owned(),
        ));
    }
    Ok(result)
}

fn validate_exact_manual_placement(
    placement: &ManualPlacementInput,
    exact_blocks: &BTreeSet<ManualBlockIdentity>,
) -> Result<(), ComposeScheduleError> {
    for assignment in &placement.assignments {
        for source in &assignment.blocks {
            let start_nanos = i128::from(source.start.timestamp_micros()) * 1_000;
            let end_nanos = i128::from(source.end.timestamp_micros()) * 1_000;
            let exact = exact_blocks.contains(&(
                assignment.item_id,
                assignment.occurrence_id,
                source.session_index,
                start_nanos,
                end_nanos,
            ));
            if !exact {
                return Err(ComposeScheduleError::InvalidRequest(
                    "manual placement was not retained as exact pinned demand".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn manual_placement_digests(
    input_digest: &str,
    placement: &ManualPlacementInput,
    assessment: &dayweave_core::ManualPlacementAssessment,
) -> Result<(String, String), ComposeScheduleError> {
    let environment_digest = prefixed_sha256(&assessment.environment_digest)?;
    let evidence = serde_json::to_vec(&ManualPlacementApprovalEvidence {
        schema: "dayweave-manual-placement-approval/1",
        input_digest,
        environment_digest: &environment_digest,
        placement,
        violations: &assessment.violations,
    })
    .map_err(|_| ComposeScheduleError::Encoding)?;
    let approval_digest = prefixed_sha256(&Sha256::digest(evidence))?;
    Ok((environment_digest, approval_digest))
}

fn prefixed_sha256(bytes: &[u8]) -> Result<String, ComposeScheduleError> {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").map_err(|_| ComposeScheduleError::Encoding)?;
    }
    Ok(encoded)
}

pub(crate) fn map_manual_placement_violations(
    violations: &[dayweave_core::ManualPlacementViolation],
) -> Result<Vec<ManualPlacementViolationOutput>, ComposeScheduleError> {
    violations
        .iter()
        .map(|violation| {
            Ok(ManualPlacementViolationOutput {
                code: violation.code,
                item_ids: violation.item_ids.iter().map(|item| item.0).collect(),
                occurrence_ids: violation
                    .occurrence_ids
                    .iter()
                    .map(|occurrence| occurrence.0)
                    .collect(),
                conflicting_block_ids: violation.conflicting_block_ids.clone(),
                conflicting_blocks: violation
                    .conflicting_blocks
                    .iter()
                    .map(|block| {
                        Ok(ManualPlacementConflictOutput {
                            block_id: block.block_id,
                            item_id: block.item_id.map(|item| item.0),
                            occurrence_id: block.occurrence_id.map(|occurrence| occurrence.0),
                            external_block_id: block.external_block_id,
                            kind: block.kind,
                            start: rfc3339(block.start)?,
                            end: rfc3339(block.end)?,
                        })
                    })
                    .collect::<Result<Vec<_>, time::error::Format>>()?,
                start: rfc3339(violation.start)?,
                end: rfc3339(violation.end)?,
                boundary_start: violation.boundary_start.map(rfc3339).transpose()?,
                boundary_end: violation.boundary_end.map(rfc3339).transpose()?,
                message: violation.message.clone(),
            })
        })
        .collect::<Result<Vec<_>, time::error::Format>>()
        .map_err(|_| ComposeScheduleError::Encoding)
}

fn retained_placement_covers(
    current: &ManualPlacementInput,
    retained: &ManualPlacementInput,
) -> bool {
    if current.id != retained.id
        || current.source_schedule_revision_id != retained.source_schedule_revision_id
        || current.assignments.is_empty()
    {
        return false;
    }
    current.assignments.iter().all(|assignment| {
        retained.assignments.iter().any(|source| {
            assignment.item_id == source.item_id
                && assignment.occurrence_id == source.occurrence_id
                && assignment.item_revision == source.item_revision
                && !assignment.blocks.is_empty()
                && assignment
                    .blocks
                    .iter()
                    .all(|block| source.blocks.contains(block))
        })
    })
}

fn rfc3339(value: OffsetDateTime) -> Result<String, time::error::Format> {
    value.format(&Rfc3339)
}

pub(super) fn request_digest(
    scheduler_publication_schema: &str,
    timezone_name: &str,
    source_item_revisions: &BTreeMap<Uuid, u64>,
    calendar_projection_stamps: &[CalendarProjectionStamp],
    execution: &ExecutionPlanningContext,
    request: &PlanRequest,
) -> Result<String, ComposeScheduleError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        scheduler_publication_schema: &'a str,
        timezone_name: &'a str,
        source_item_revisions: &'a BTreeMap<Uuid, u64>,
        calendar_projection_stamps: &'a [CalendarProjectionStamp],
        execution: &'a ExecutionPlanningContext,
        request: &'a PlanRequest,
    }

    let bytes = serde_json::to_vec(&DigestInput {
        scheduler_publication_schema,
        timezone_name,
        source_item_revisions,
        calendar_projection_stamps,
        execution,
        request,
    })
    .map_err(|_| ComposeScheduleError::Encoding)?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|_| ComposeScheduleError::Encoding)?;
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use chrono::{TimeZone as _, Utc};
    use dayweave_compose::{
        AvailabilityInput, EnergyInput, FixedBlockInput, ManualPlacementAssignmentInput,
        ManualPlacementReleaseInput, PreviousAssignmentInput, PreviousBlockInput,
        SchedulerConfigInput,
    };
    use dayweave_core::{ItemKind as PlanningItemKind, RecurrenceContext, WorkItem};
    use serde_json::json;

    fn canonical_item(id: Uuid) -> Item {
        Item {
            id,
            is_sensitive: false,
            kind: ItemKind::Task,
            status: ItemStatus::Planned,
            title: "Write schedule bridge".into(),
            notes: None,
            timezone_name: "Europe/Madrid".into(),
            duration_seconds: Some(3_600),
            deadline_at: Some(Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap()),
            earliest_start_at: None,
            recurrence: None,
            flexible_constraints: json!({"energy": "deep", "preferred_start_minute": 540}),
            split_policy: SplitPolicy::Indivisible,
            importance: 80,
            urgency: 60,
            parent_id: None,
            sibling_order: 0,
            is_executable: true,
            revision: 3,
            created_at: Utc.with_ymd_and_hms(2026, 8, 29, 8, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 8, 29, 8, 0, 0).unwrap(),
            completed_at: None,
            deleted_at: None,
        }
    }

    fn preview_request() -> ComposeScheduleRequest {
        ComposeScheduleRequest {
            as_of: Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap(),
            horizon_start: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
            horizon_end: Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).unwrap(),
            timezone_name: "Europe/Madrid".into(),
            availability: vec![AvailabilityInput {
                start: Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 9, 1, 16, 0, 0).unwrap(),
                contexts: BTreeSet::new(),
                location: None,
                energy: EnergyInput::Deep,
            }],
            fixed_blocks: Vec::new(),
            previous_assignments: Vec::new(),
            manual_placements: Vec::new(),
            manual_placement_releases: Vec::new(),
            config: SchedulerConfigInput::default(),
            recurrence_context: RecurrenceContext::default(),
        }
    }

    fn map_plannable(item: &Item) -> WorkItem {
        let prepared =
            prepare_canonical_schedule(vec![into_canonical_item(item.clone())], preview_request())
                .expect("valid canonical preparation");
        assert!(prepared.rejected_items.is_empty());
        prepared
            .plan_request
            .items
            .into_iter()
            .next()
            .expect("expected plannable item")
    }

    fn retained_manual_state(
        placement: ManualPlacementInput,
    ) -> super::super::postgres::PersistedManualPlacementState {
        super::super::postgres::PersistedManualPlacementState {
            placement,
            environment_digest: format!("sha256:{}", "11".repeat(32)),
            assessment_digest: format!("sha256:{}", "22".repeat(32)),
            authorized_violations: Vec::new(),
            authorization: super::super::postgres::ManualPlacementAuthorization::ConflictFree,
        }
    }

    fn apply_manual_placement_lifecycle(
        request: &mut ComposeScheduleRequest,
        evidence: &AuthoritativePlanningEvidence,
        items: &[Item],
    ) -> Result<(), ComposeScheduleError> {
        normalize_manual_placements_for_execution(request, &evidence.execution)?;
        validate_manual_placement_item_revisions(request, items)?;
        validate_manual_placement_sources(request, evidence)?;
        let pruned = merge_retained_manual_placements(request, evidence, items)?;
        replace_with_authoritative_assignments(request, evidence)?;
        request
            .previous_assignments
            .retain(|assignment| !pruned.contains(&(assignment.item_id, assignment.occurrence_id)));
        validate_schedule_request(request).map_err(map_prepare_error)?;
        Ok(())
    }

    #[test]
    fn composes_canonical_item_and_is_digest_stable() {
        const V5_DIGEST: &str =
            "sha256:bdf9e77bbfd56d28bfc8743fbefe8782b77c7ebbcba1e330b14cb49f36e8077a";
        const V5_RESPONSE: &str = r#"{"input_digest":"sha256:bdf9e77bbfd56d28bfc8743fbefe8782b77c7ebbcba1e330b14cb49f36e8077a","source_item_count":1,"source_item_revisions":{"00000000-0000-0000-0000-000000000001":3},"accepted_item_count":1,"rejected_items":[],"ignored_previous_assignments":[],"plan":{"as_of":"2026-09-01T09:00:00+02:00","horizon_start":"2026-09-01T02:00:00+02:00","horizon_end":"2026-09-02T02:00:00+02:00","blocks":[{"id":"829359ec-6709-54db-a3f2-4428470e1ae6","is_sensitive":false,"item_id":"00000000-0000-0000-0000-000000000001","occurrence_id":null,"external_block_id":null,"title":"Write schedule bridge","start":"2026-09-01T09:00:00+02:00","end":"2026-09-01T10:00:00+02:00","session_index":0,"kind":"planned","explanations":[{"code":"hard_deadline","message":"Placed within its hard deadline."},{"code":"priority","message":"Priority score is 48."},{"code":"preferred_window","message":"Matches a preferred work window."},{"code":"energy_match","message":"Matches the available energy level."},{"code":"earliest_available","message":"Uses the earliest best-scoring valid capacity."}]}],"unscheduled":[],"decisions":[{"item_id":"00000000-0000-0000-0000-000000000001","occurrence_id":null,"kind":"scheduled","message":"Reserved 60 minutes."}],"violations":[],"score":{"scheduled_minutes":60,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}"#;
        let item = canonical_item(Uuid::from_u128(1));
        let first = compose_items(vec![item.clone()], preview_request()).unwrap();
        let second = compose_items(vec![item], preview_request()).unwrap();
        assert_eq!(first.input_digest, second.input_digest);
        assert_eq!(first.input_digest, V5_DIGEST);
        assert_eq!(
            serde_json::to_string(&first).expect("migrated response encoding"),
            V5_RESPONSE
        );
        assert_eq!(first.accepted_item_count, 1);
        assert_eq!(
            first.source_item_revisions.get(&Uuid::from_u128(1)),
            Some(&3)
        );
        assert_eq!(first.plan.blocks.len(), 1);
        assert!(first.input_digest.starts_with("sha256:"));
        let public = serde_json::to_value(&first).expect("public response must serialize");
        assert!(public.get("planning_request").is_none());
        let (_, snapshot) =
            super::super::postgres::validate_publishable_compose_result("Europe/Madrid", &first)
                .expect("composed response must remain publishable");
        assert_eq!(snapshot.get("schema_version"), Some(&json!(5)));
        assert_eq!(
            snapshot.get("planning_request"),
            Some(
                &serde_json::to_value(&first.planning_request)
                    .expect("private planning request must serialize")
            )
        );

        let mut tampered = first;
        tampered.planning_request.config.stability_weight += 1;
        assert_eq!(
            super::super::postgres::validate_publishable_compose_result("Europe/Madrid", &tampered,),
            Err(super::super::postgres::SchedulePublicationError::InvalidPayload)
        );
    }

    #[test]
    fn manual_preview_binds_exact_content_free_conflicts_and_carry_forward() {
        let placement_id = Uuid::from_u128(30);
        let fixed_id = Uuid::from_u128(31);
        let mut item = canonical_item(Uuid::from_u128(32));
        item.is_sensitive = true;
        item.title = "SYNTHETIC-SENSITIVE-MANUAL-TARGET".to_owned();
        let mut request = preview_request();
        request.fixed_blocks = vec![FixedBlockInput {
            id: fixed_id,
            is_sensitive: true,
            title: "SYNTHETIC-SENSITIVE-MANUAL-CONFLICT".to_owned(),
            start: Utc.with_ymd_and_hms(2026, 9, 1, 13, 30, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 14, 30, 0).unwrap(),
            source: FixedBlockSourceInput::ProtectedTime,
        }];
        request.manual_placements = vec![ManualPlacementInput {
            id: placement_id,
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![PreviousBlockInput {
                    start: Utc.with_ymd_and_hms(2026, 9, 1, 13, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 9, 1, 14, 0, 0).unwrap(),
                    session_index: 0,
                }],
            }],
        }];

        let first = compose_items(vec![item.clone()], request.clone()).unwrap();
        let [assessment] = first.manual_placement_assessments.as_slice() else {
            panic!("one manual assessment");
        };
        assert!(assessment.approval_required);
        assert!(assessment.environment_digest.starts_with("sha256:"));
        assert!(assessment.approval_digest.starts_with("sha256:"));
        assert!(
            assessment
                .violations
                .iter()
                .any(|violation| { violation.code == ManualPlacementViolationCode::LatestFinish })
        );
        let overlap = assessment
            .violations
            .iter()
            .find(|violation| violation.code == ManualPlacementViolationCode::ImmutableOverlap)
            .expect("immutable conflict");
        assert_eq!(overlap.conflicting_block_ids, vec![fixed_id]);
        assert_eq!(overlap.conflicting_blocks[0].block_id, fixed_id);
        assert_eq!(
            overlap.conflicting_blocks[0].start,
            "2026-09-01T15:30:00+02:00"
        );
        let public_assessment = serde_json::to_string(&first.manual_placement_assessments).unwrap();
        assert!(!public_assessment.contains("SYNTHETIC-SENSITIVE"));

        let retained = super::super::postgres::PersistedManualPlacementState {
            placement: request.manual_placements[0].clone(),
            environment_digest: assessment.environment_digest.clone(),
            assessment_digest: assessment.approval_digest.clone(),
            authorized_violations: assessment.violations.clone(),
            authorization: super::super::postgres::ManualPlacementAuthorization::ExplicitApproval,
        };
        let evidence = AuthoritativePlanningEvidence {
            retained_manual_placements: vec![retained.clone()],
            ..AuthoritativePlanningEvidence::default()
        };
        let carried = compose_items_with_projection_for_schema(
            vec![item.clone()],
            request.clone(),
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .unwrap();
        assert!(!carried.manual_placement_assessments[0].approval_required);

        request.availability[0].location = Some("changed environment".to_owned());
        let changed = compose_items_with_projection_for_schema(
            vec![item],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            AuthoritativePlanningEvidence {
                retained_manual_placements: vec![retained],
                ..AuthoritativePlanningEvidence::default()
            },
            Vec::new(),
        )
        .unwrap();
        assert!(changed.manual_placement_assessments[0].approval_required);
        assert_ne!(
            carried.manual_placement_assessments[0].environment_digest,
            changed.manual_placement_assessments[0].environment_digest
        );
    }

    #[test]
    fn manual_assessment_and_approval_dtos_reject_unknown_nested_fields() {
        let assessment = json!({
            "placement_id": Uuid::from_u128(330),
            "environment_digest": format!("sha256:{}", "11".repeat(32)),
            "approval_digest": format!("sha256:{}", "22".repeat(32)),
            "approval_required": true,
            "violations": [{
                "code": "immutable_overlap",
                "item_ids": [Uuid::from_u128(331)],
                "occurrence_ids": [],
                "conflicting_block_ids": [Uuid::from_u128(332)],
                "conflicting_blocks": [{
                    "block_id": Uuid::from_u128(332),
                    "item_id": null,
                    "occurrence_id": null,
                    "external_block_id": Uuid::from_u128(333),
                    "kind": "external_fixed",
                    "start": "2026-09-01T09:00:00Z",
                    "end": "2026-09-01T10:00:00Z"
                }],
                "start": "2026-09-01T09:00:00Z",
                "end": "2026-09-01T10:00:00Z",
                "boundary_start": null,
                "boundary_end": null,
                "message": "Immutable overlap."
            }]
        });
        assert!(
            serde_json::from_value::<ManualPlacementAssessmentOutput>(assessment.clone()).is_ok()
        );
        for pointer in ["", "/violations/0", "/violations/0/conflicting_blocks/0"] {
            let mut hostile = assessment.clone();
            hostile
                .pointer_mut(pointer)
                .expect("assessment fixture path")["future"] = json!(true);
            assert!(
                serde_json::from_value::<ManualPlacementAssessmentOutput>(hostile).is_err(),
                "unknown field at {pointer} must be rejected"
            );
        }

        let mut approval = json!({
            "placement_id": Uuid::from_u128(330),
            "approval_digest": format!("sha256:{}", "22".repeat(32))
        });
        approval["future"] = json!(true);
        assert!(serde_json::from_value::<ManualPlacementApproval>(approval).is_err());
    }

    #[test]
    fn explicit_release_removes_the_complete_retained_pin() {
        let item = canonical_item(Uuid::from_u128(40));
        let published_revision_id = Uuid::from_u128(41);
        let placement_id = Uuid::from_u128(42);
        let block = PreviousBlockInput {
            start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            session_index: 0,
        };
        let retained = ManualPlacementInput {
            id: placement_id,
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![block.clone()],
            }],
        };
        let evidence = AuthoritativePlanningEvidence {
            published_revision_id: Some(published_revision_id),
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![block],
                pinned: true,
            }],
            retained_manual_placements: vec![retained_manual_state(retained)],
            ..AuthoritativePlanningEvidence::default()
        };
        let mut request = preview_request();
        request.manual_placement_releases = vec![ManualPlacementReleaseInput {
            id: Uuid::from_u128(43),
            placement_id,
            source_schedule_revision_id: published_revision_id,
        }];

        apply_manual_placement_lifecycle(&mut request, &evidence, std::slice::from_ref(&item))
            .expect("exact release");
        assert!(request.manual_placements.is_empty());
        assert!(request.previous_assignments.is_empty());

        let result = compose_items_with_projection_for_schema(
            vec![item],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .expect("released pin recomposes normally");
        assert!(result.manual_placement_assessments.is_empty());
        assert!(
            result
                .plan
                .blocks
                .iter()
                .all(|block| block.kind != ScheduleBlockKind::Pinned)
        );
    }

    #[test]
    fn changed_shape_requires_release_and_can_be_replaced_atomically() {
        let mut item = canonical_item(Uuid::from_u128(50));
        item.duration_seconds = Some(5_400);
        item.revision = 4;
        let published_revision_id = Uuid::from_u128(51);
        let retained_placement_id = Uuid::from_u128(52);
        let source = PreviousBlockInput {
            start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            session_index: 0,
        };
        let retained = ManualPlacementInput {
            id: retained_placement_id,
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: 3,
                occurrence_id: None,
                blocks: vec![source.clone()],
            }],
        };
        let evidence = AuthoritativePlanningEvidence {
            published_revision_id: Some(published_revision_id),
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: item.id,
                item_revision: 3,
                occurrence_id: None,
                blocks: vec![source],
                pinned: true,
            }],
            retained_manual_placements: vec![retained_manual_state(retained)],
            ..AuthoritativePlanningEvidence::default()
        };
        let replacement = ManualPlacementInput {
            id: Uuid::from_u128(53),
            source_schedule_revision_id: Some(published_revision_id),
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![PreviousBlockInput {
                    start: Utc.with_ymd_and_hms(2026, 9, 1, 11, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 9, 1, 12, 30, 0).unwrap(),
                    session_index: 0,
                }],
            }],
        };
        let mut without_release = preview_request();
        without_release.manual_placements = vec![replacement.clone()];
        let mut non_manual_source = evidence.clone();
        non_manual_source.retained_manual_placements.clear();
        validate_manual_placement_sources(&without_release, &non_manual_source)
            .expect("a superseded ordinary assignment uses the current canonical shape");
        assert!(matches!(
            validate_manual_placement_sources(&without_release, &evidence),
            Err(ComposeScheduleError::InvalidRequest(message))
                if message.contains("preserve every remaining source session")
        ));

        let mut request = preview_request();
        request.manual_placements = vec![replacement.clone()];
        request.manual_placement_releases = vec![ManualPlacementReleaseInput {
            id: Uuid::from_u128(54),
            placement_id: retained_placement_id,
            source_schedule_revision_id: published_revision_id,
        }];
        apply_manual_placement_lifecycle(&mut request, &evidence, std::slice::from_ref(&item))
            .expect("released complete group may use the current shape");
        assert!(request.previous_assignments.is_empty());
        assert_eq!(request.manual_placements, vec![replacement]);

        let result = compose_items_with_projection_for_schema(
            vec![item],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .expect("atomic release and replacement");
        assert_eq!(result.manual_placement_assessments.len(), 1);
        assert_eq!(
            result
                .plan
                .blocks
                .iter()
                .filter(|block| block.kind == ScheduleBlockKind::Pinned)
                .map(|block| (
                    rfc3339(block.start).expect("start format"),
                    rfc3339(block.end).expect("end format"),
                ))
                .collect::<Vec<_>>(),
            vec![(
                "2026-09-01T13:00:00+02:00".to_owned(),
                "2026-09-01T14:30:00+02:00".to_owned(),
            )]
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One scenario covers both under- and over-credit normalization.
    fn manual_move_normalizes_an_executed_split_session_to_remaining_demand() {
        let mut item = canonical_item(Uuid::from_u128(55));
        item.duration_seconds = Some(7_200);
        item.split_policy = SplitPolicy::Splittable {
            minimum_chunk_seconds: 1_800,
            maximum_chunk_seconds: 3_600,
        };
        let published_revision_id = Uuid::from_u128(56);
        let source_blocks = vec![
            PreviousBlockInput {
                start: Utc.with_ymd_and_hms(2026, 9, 1, 8, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
                session_index: 0,
            },
            PreviousBlockInput {
                start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
                session_index: 1,
            },
        ];
        let evidence = AuthoritativePlanningEvidence {
            execution: ExecutionPlanningContext {
                snapshot_revision: 1,
                work_units: vec![dayweave_core::ExecutionWorkUnit {
                    item_id: ItemId(item.id),
                    occurrence_id: None,
                    progress_epoch: 1,
                    credited_seconds: 1_800,
                    disposition: None,
                    used_session_indices: vec![0],
                    reservations: Vec::new(),
                }],
            },
            published_revision_id: Some(published_revision_id),
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: source_blocks,
                pinned: false,
            }],
            retained_manual_placements: Vec::new(),
        };
        let mut request = preview_request();
        request.manual_placements = vec![ManualPlacementInput {
            id: Uuid::from_u128(57),
            source_schedule_revision_id: Some(published_revision_id),
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![
                    PreviousBlockInput {
                        start: Utc.with_ymd_and_hms(2026, 9, 1, 11, 0, 0).unwrap(),
                        end: Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap(),
                        session_index: 0,
                    },
                    PreviousBlockInput {
                        start: Utc.with_ymd_and_hms(2026, 9, 1, 13, 0, 0).unwrap(),
                        end: Utc.with_ymd_and_hms(2026, 9, 1, 14, 0, 0).unwrap(),
                        session_index: 1,
                    },
                    PreviousBlockInput {
                        start: Utc.with_ymd_and_hms(2026, 9, 1, 14, 0, 0).unwrap(),
                        end: Utc.with_ymd_and_hms(2026, 9, 1, 14, 30, 0).unwrap(),
                        session_index: 2,
                    },
                ],
            }],
        }];
        validate_schedule_request(&request).expect("pre-normalization request shape");

        apply_manual_placement_lifecycle(&mut request, &evidence, std::slice::from_ref(&item))
            .expect("executed source session is removed authoritatively");
        assert_eq!(request.manual_placements[0].assignments[0].blocks.len(), 2);
        assert_eq!(
            request.manual_placements[0].assignments[0].blocks[0].session_index,
            1
        );

        let result = compose_items_with_projection_for_schema(
            vec![item],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .expect("remaining published session can be moved");
        let pinned = result
            .plan
            .blocks
            .iter()
            .filter(|block| block.kind == ScheduleBlockKind::Pinned)
            .collect::<Vec<_>>();
        assert_eq!(
            pinned
                .iter()
                .map(|block| block.session_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            pinned
                .iter()
                .map(|block| (block.end - block.start).whole_minutes())
                .sum::<i64>(),
            90
        );

        let mut over_credit_evidence = result.planning_evidence.clone();
        over_credit_evidence.execution.work_units[0].credited_seconds = 5_400;
        let mut over_credit_request = preview_request();
        over_credit_request.manual_placements = vec![ManualPlacementInput {
            id: Uuid::from_u128(58),
            source_schedule_revision_id: Some(published_revision_id),
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: Uuid::from_u128(55),
                item_revision: 3,
                occurrence_id: None,
                blocks: vec![PreviousBlockInput {
                    start: Utc.with_ymd_and_hms(2026, 9, 1, 15, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 9, 1, 15, 30, 0).unwrap(),
                    session_index: 1,
                }],
            }],
        }];
        let over_credit_item = canonical_item(Uuid::from_u128(55));
        let mut over_credit_item = over_credit_item;
        over_credit_item.duration_seconds = Some(7_200);
        over_credit_item.split_policy = SplitPolicy::Splittable {
            minimum_chunk_seconds: 1_800,
            maximum_chunk_seconds: 3_600,
        };
        apply_manual_placement_lifecycle(
            &mut over_credit_request,
            &over_credit_evidence,
            std::slice::from_ref(&over_credit_item),
        )
        .expect("over-credit may reshape the exact remaining demand");
        let over_credit = compose_items_with_projection_for_schema(
            vec![over_credit_item],
            over_credit_request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            over_credit_evidence,
            Vec::new(),
        )
        .expect("over-credit remaining session composes");
        let over_credit_minutes = over_credit
            .plan
            .blocks
            .iter()
            .filter(|block| block.kind == ScheduleBlockKind::Pinned)
            .map(|block| (block.end - block.start).whole_minutes())
            .sum::<i64>();
        assert_eq!(over_credit_minutes, 30);
    }

    #[test]
    fn manual_move_can_preserve_a_partial_published_split_and_add_fresh_capacity() {
        let mut item = canonical_item(Uuid::from_u128(551));
        item.duration_seconds = Some(7_200);
        item.split_policy = SplitPolicy::Splittable {
            minimum_chunk_seconds: 1_800,
            maximum_chunk_seconds: 3_600,
        };
        let published_revision_id = Uuid::from_u128(552);
        let evidence = AuthoritativePlanningEvidence {
            published_revision_id: Some(published_revision_id),
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![PreviousBlockInput {
                    start: Utc.with_ymd_and_hms(2026, 9, 1, 8, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
                    session_index: 0,
                }],
                pinned: false,
            }],
            ..AuthoritativePlanningEvidence::default()
        };
        let mut request = preview_request();
        request.manual_placements = vec![ManualPlacementInput {
            id: Uuid::from_u128(553),
            source_schedule_revision_id: Some(published_revision_id),
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![
                    PreviousBlockInput {
                        start: Utc.with_ymd_and_hms(2026, 9, 1, 11, 0, 0).unwrap(),
                        end: Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap(),
                        session_index: 0,
                    },
                    PreviousBlockInput {
                        start: Utc.with_ymd_and_hms(2026, 9, 1, 13, 0, 0).unwrap(),
                        end: Utc.with_ymd_and_hms(2026, 9, 1, 14, 0, 0).unwrap(),
                        session_index: 1,
                    },
                ],
            }],
        }];

        apply_manual_placement_lifecycle(&mut request, &evidence, std::slice::from_ref(&item))
            .expect("the published source is a subset of the complete requested demand");
        let result = compose_items_with_projection_for_schema(
            vec![item],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .expect("partial published capacity may be moved while fresh capacity is added");
        let pinned = result
            .plan
            .blocks
            .iter()
            .filter(|block| block.kind == ScheduleBlockKind::Pinned)
            .collect::<Vec<_>>();
        assert_eq!(
            pinned
                .iter()
                .map(|block| block.session_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            pinned
                .iter()
                .map(|block| (block.end - block.start).whole_minutes())
                .sum::<i64>(),
            120
        );
    }

    #[test]
    fn new_execution_reservation_makes_a_manual_move_authoritatively_stale() {
        let item_id = Uuid::from_u128(59);
        let mut request = preview_request();
        request.manual_placements = vec![ManualPlacementInput {
            id: Uuid::from_u128(590),
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id,
                item_revision: 3,
                occurrence_id: None,
                blocks: vec![PreviousBlockInput {
                    start: Utc.with_ymd_and_hms(2026, 9, 1, 11, 0, 0).unwrap(),
                    end: Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap(),
                    session_index: 1,
                }],
            }],
        }];
        let execution = ExecutionPlanningContext {
            snapshot_revision: 2,
            work_units: vec![dayweave_core::ExecutionWorkUnit {
                item_id: ItemId(item_id),
                occurrence_id: None,
                progress_epoch: 1,
                credited_seconds: 1_800,
                disposition: None,
                used_session_indices: vec![0],
                reservations: vec![dayweave_core::ExecutionReservation {
                    session_index: 0,
                    start: time::macros::datetime!(2026-09-01 10:00 UTC),
                    end: time::macros::datetime!(2026-09-01 10:30 UTC),
                    kind: dayweave_core::ExecutionReservationKind::InFlight,
                }],
            }],
        };

        assert!(matches!(
            normalize_manual_placements_for_execution(&mut request, &execution),
            Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(message))
                if message.contains("active execution reservation")
        ));
    }

    #[test]
    fn completed_item_prunes_retained_pin_without_a_release_command() {
        let mut item = canonical_item(Uuid::from_u128(60));
        item.status = ItemStatus::Completed;
        item.revision = 4;
        item.completed_at = Some(Utc.with_ymd_and_hms(2026, 9, 1, 7, 30, 0).unwrap());
        let source = PreviousBlockInput {
            start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            session_index: 0,
        };
        let retained = ManualPlacementInput {
            id: Uuid::from_u128(61),
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: 3,
                occurrence_id: None,
                blocks: vec![source.clone()],
            }],
        };
        let evidence = AuthoritativePlanningEvidence {
            published_revision_id: Some(Uuid::from_u128(62)),
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: item.id,
                item_revision: 3,
                occurrence_id: None,
                blocks: vec![source],
                pinned: true,
            }],
            retained_manual_placements: vec![retained_manual_state(retained)],
            ..AuthoritativePlanningEvidence::default()
        };
        let mut request = preview_request();
        apply_manual_placement_lifecycle(&mut request, &evidence, std::slice::from_ref(&item))
            .expect("terminal work automatically releases retained placement");
        assert!(request.previous_assignments.is_empty());
        assert!(request.manual_placements.is_empty());

        let result = compose_items_with_projection_for_schema(
            vec![item],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .expect("terminal item no longer bricks composition");
        assert!(result.plan.blocks.is_empty());
        assert!(result.manual_placement_assessments.is_empty());
    }

    #[test]
    fn inbox_ancestor_prunes_a_retained_leaf_pin() {
        let mut parent = canonical_item(Uuid::from_u128(70));
        parent.status = ItemStatus::Inbox;
        parent.duration_seconds = None;
        parent.is_executable = false;
        let mut leaf = canonical_item(Uuid::from_u128(71));
        leaf.parent_id = Some(parent.id);
        let block = PreviousBlockInput {
            start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            session_index: 0,
        };
        let retained = ManualPlacementInput {
            id: Uuid::from_u128(72),
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: leaf.id,
                item_revision: leaf.revision,
                occurrence_id: None,
                blocks: vec![block.clone()],
            }],
        };
        let evidence = AuthoritativePlanningEvidence {
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: leaf.id,
                item_revision: leaf.revision,
                occurrence_id: None,
                blocks: vec![block],
                pinned: true,
            }],
            retained_manual_placements: vec![retained_manual_state(retained)],
            ..AuthoritativePlanningEvidence::default()
        };
        let mut request = preview_request();

        apply_manual_placement_lifecycle(&mut request, &evidence, &[parent.clone(), leaf.clone()])
            .expect("Inbox subtree automatically releases retained placement");
        assert!(request.previous_assignments.is_empty());
        assert!(request.manual_placements.is_empty());

        let result = compose_items_with_projection_for_schema(
            vec![parent, leaf],
            request,
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            Vec::new(),
            evidence,
            Vec::new(),
        )
        .expect("Inbox subtree does not retain stale pinned demand");
        assert!(result.plan.blocks.is_empty());
    }

    #[test]
    fn future_retained_pin_outside_a_narrower_horizon_requires_release() {
        let item = canonical_item(Uuid::from_u128(73));
        let block = PreviousBlockInput {
            start: Utc.with_ymd_and_hms(2026, 9, 3, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 3, 10, 0, 0).unwrap(),
            session_index: 0,
        };
        let retained = ManualPlacementInput {
            id: Uuid::from_u128(74),
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![block.clone()],
            }],
        };
        let evidence = AuthoritativePlanningEvidence {
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: item.id,
                item_revision: item.revision,
                occurrence_id: None,
                blocks: vec![block],
                pinned: true,
            }],
            retained_manual_placements: vec![retained_manual_state(retained)],
            ..AuthoritativePlanningEvidence::default()
        };
        let mut request = preview_request();

        assert!(matches!(
            apply_manual_placement_lifecycle(
                &mut request,
                &evidence,
                std::slice::from_ref(&item)
            ),
            Err(ComposeScheduleError::AuthoritativeManualPlacementChanged(message))
                if message.contains("expand the horizon or explicitly release")
        ));
    }

    #[test]
    fn rejected_ancestor_prunes_a_retained_leaf_pin() {
        let mut parent = canonical_item(Uuid::from_u128(75));
        parent.flexible_constraints = json!({"unsupported_parent_metadata": true});
        let mut leaf = canonical_item(Uuid::from_u128(76));
        leaf.parent_id = Some(parent.id);
        let block = PreviousBlockInput {
            start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            session_index: 0,
        };
        let retained = ManualPlacementInput {
            id: Uuid::from_u128(77),
            source_schedule_revision_id: None,
            assignments: vec![ManualPlacementAssignmentInput {
                item_id: leaf.id,
                item_revision: leaf.revision,
                occurrence_id: None,
                blocks: vec![block.clone()],
            }],
        };
        let evidence = AuthoritativePlanningEvidence {
            previous_assignments: vec![PreviousAssignmentInput {
                item_id: leaf.id,
                item_revision: leaf.revision,
                occurrence_id: None,
                blocks: vec![block],
                pinned: true,
            }],
            retained_manual_placements: vec![retained_manual_state(retained)],
            ..AuthoritativePlanningEvidence::default()
        };
        let mut request = preview_request();

        apply_manual_placement_lifecycle(&mut request, &evidence, &[parent, leaf])
            .expect("rejected hierarchy automatically releases retained placement");
        assert!(request.previous_assignments.is_empty());
        assert!(request.manual_placements.is_empty());

        let mut orphan = canonical_item(Uuid::from_u128(78));
        orphan.parent_id = Some(Uuid::from_u128(79));
        assert!(
            canonical_plannable_item_ids(&preview_request(), std::slice::from_ref(&orphan))
                .expect("orphaned canonical snapshot remains isolatable")
                .is_empty(),
            "a leaf with a missing ancestor cannot retain pinned work"
        );
    }

    #[test]
    fn item_to_canonical_item_conversion_is_lossless_and_exhaustive() {
        let item = Item {
            id: Uuid::from_u128(900),
            is_sensitive: true,
            kind: ItemKind::Event,
            status: ItemStatus::Cancelled,
            title: "Lossless conversion".into(),
            notes: Some("Every canonical field crosses the crate boundary.".into()),
            timezone_name: "America/New_York".into(),
            duration_seconds: Some(7_201),
            deadline_at: Some(Utc.with_ymd_and_hms(2026, 9, 3, 18, 0, 0).unwrap()),
            earliest_start_at: Some(Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap()),
            recurrence: Some(json!({"type": "monthly", "times_per_month": 2})),
            flexible_constraints: json!({"tags": ["boundary"], "has_own_effort": true}),
            split_policy: SplitPolicy::Splittable {
                minimum_chunk_seconds: 601,
                maximum_chunk_seconds: 3_601,
            },
            importance: 91,
            urgency: 42,
            parent_id: Some(Uuid::from_u128(899)),
            sibling_order: 17,
            is_executable: false,
            revision: 23,
            created_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 2, 3).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 8, 2, 4, 5, 6).unwrap(),
            completed_at: Some(Utc.with_ymd_and_hms(2026, 8, 3, 7, 8, 9).unwrap()),
            deleted_at: Some(Utc.with_ymd_and_hms(2026, 8, 4, 10, 11, 12).unwrap()),
        };
        let expected = CanonicalItem {
            id: item.id,
            is_sensitive: item.is_sensitive,
            kind: CanonicalItemKind::Event,
            status: CanonicalItemStatus::Cancelled,
            title: item.title.clone(),
            notes: item.notes.clone(),
            timezone_name: item.timezone_name.clone(),
            duration_seconds: item.duration_seconds,
            deadline_at: item.deadline_at,
            earliest_start_at: item.earliest_start_at,
            recurrence: item.recurrence.clone(),
            flexible_constraints: item.flexible_constraints.clone(),
            split_policy: CanonicalSplitPolicy::Splittable {
                minimum_chunk_seconds: 601,
                maximum_chunk_seconds: 3_601,
            },
            importance: item.importance,
            urgency: item.urgency,
            parent_id: item.parent_id,
            sibling_order: item.sibling_order,
            is_executable: item.is_executable,
            revision: item.revision,
            created_at: item.created_at,
            updated_at: item.updated_at,
            completed_at: item.completed_at,
            deleted_at: item.deleted_at,
        };
        assert_eq!(into_canonical_item(item), expected);

        for (kind, expected) in [
            (ItemKind::Event, CanonicalItemKind::Event),
            (ItemKind::Task, CanonicalItemKind::Task),
            (ItemKind::Habit, CanonicalItemKind::Habit),
            (ItemKind::Routine, CanonicalItemKind::Routine),
            (ItemKind::Goal, CanonicalItemKind::Goal),
            (ItemKind::Break, CanonicalItemKind::Break),
        ] {
            let mut item = canonical_item(Uuid::new_v4());
            item.kind = kind;
            assert_eq!(into_canonical_item(item).kind, expected);
        }
        for (status, expected) in [
            (ItemStatus::Inbox, CanonicalItemStatus::Inbox),
            (ItemStatus::Planned, CanonicalItemStatus::Planned),
            (ItemStatus::Scheduled, CanonicalItemStatus::Scheduled),
            (ItemStatus::InProgress, CanonicalItemStatus::InProgress),
            (ItemStatus::Paused, CanonicalItemStatus::Paused),
            (ItemStatus::Completed, CanonicalItemStatus::Completed),
            (ItemStatus::Skipped, CanonicalItemStatus::Skipped),
            (ItemStatus::Cancelled, CanonicalItemStatus::Cancelled),
        ] {
            let mut item = canonical_item(Uuid::new_v4());
            item.status = status;
            assert_eq!(into_canonical_item(item).status, expected);
        }
        let mut indivisible = canonical_item(Uuid::new_v4());
        indivisible.split_policy = SplitPolicy::Indivisible;
        assert_eq!(
            into_canonical_item(indivisible).split_policy,
            CanonicalSplitPolicy::Indivisible
        );
    }

    #[test]
    fn preparation_errors_preserve_the_server_error_contract() {
        assert!(matches!(
            map_prepare_error(PrepareScheduleError::InvalidRequest("fixture".into())),
            ComposeScheduleError::InvalidRequest(message) if message == "fixture"
        ));
        assert!(matches!(
            map_prepare_error(PrepareScheduleError::TooManyItems),
            ComposeScheduleError::TooManyItems
        ));
        for error in [
            PrepareScheduleError::DuplicateCanonicalItem(Uuid::from_u128(1)),
            PrepareScheduleError::InvalidCanonicalItem(Uuid::from_u128(2)),
            PrepareScheduleError::AccountingOverflow,
        ] {
            assert!(matches!(
                map_prepare_error(error),
                ComposeScheduleError::Encoding
            ));
        }
    }

    #[test]
    fn previous_assignment_order_remains_digest_and_response_significant() {
        let high = canonical_item(Uuid::from_u128(920));
        let low = canonical_item(Uuid::from_u128(910));
        let assignment = |item_id, item_revision, occurrence_id| PreviousAssignmentInput {
            item_id,
            item_revision,
            occurrence_id,
            blocks: Vec::new(),
            pinned: false,
        };
        let mut request = preview_request();
        request.previous_assignments = vec![
            assignment(high.id, high.revision, None),
            assignment(low.id, low.revision, None),
            assignment(high.id, high.revision - 1, Some(Uuid::from_u128(921))),
            assignment(low.id, low.revision - 1, Some(Uuid::from_u128(911))),
        ];

        let first = compose_items(vec![low.clone(), high.clone()], request.clone()).unwrap();
        assert_eq!(
            first
                .ignored_previous_assignments
                .iter()
                .map(|assignment| assignment.item_id)
                .collect::<Vec<_>>(),
            vec![high.id, low.id]
        );

        request.previous_assignments.swap(0, 1);
        let reversed = compose_items(vec![high, low], request).unwrap();
        assert_ne!(first.input_digest, reversed.input_digest);
        assert_eq!(
            serde_json::to_value(&first.plan).unwrap(),
            serde_json::to_value(&reversed.plan).unwrap()
        );
        assert_eq!(
            reversed
                .ignored_previous_assignments
                .iter()
                .map(|assignment| assignment.item_id)
                .collect::<Vec<_>>(),
            first
                .ignored_previous_assignments
                .iter()
                .map(|assignment| assignment.item_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn duration_bearing_inbox_root_is_accepted_without_entering_the_plan() {
        let item_id = Uuid::from_u128(100);
        let mut item = canonical_item(item_id);
        item.status = ItemStatus::Inbox;
        let mut request = preview_request();
        request.recurrence_context.completion_anchors.insert(
            ItemId(item_id),
            OffsetDateTime::from_unix_timestamp(item.updated_at.timestamp()).unwrap(),
        );

        let first = compose_items(vec![item.clone()], request.clone()).unwrap();
        let repeated = compose_items(vec![item.clone()], request.clone()).unwrap();
        item.revision += 1;
        let revised = compose_items(vec![item], request).unwrap();

        assert_eq!(first.input_digest, repeated.input_digest);
        assert_ne!(first.input_digest, revised.input_digest);
        assert_eq!(first.source_item_count, 1);
        assert_eq!(first.accepted_item_count, 1);
        assert_eq!(first.source_item_revisions.get(&item_id), Some(&3));
        assert!(first.rejected_items.is_empty());
        assert!(first.plan.blocks.is_empty());
        assert!(first.plan.unscheduled.is_empty());
        assert!(first.plan.decisions.is_empty());
        assert!(first.plan.occurrences.is_empty());
    }

    #[test]
    fn every_descendant_of_an_inbox_item_is_accepted_without_orphan_rejection() {
        let root_id = Uuid::from_u128(101);
        let child_id = Uuid::from_u128(102);
        let grandchild_id = Uuid::from_u128(103);
        let mut root = canonical_item(root_id);
        root.status = ItemStatus::Inbox;
        let mut child = canonical_item(child_id);
        child.parent_id = Some(root_id);
        let mut grandchild = canonical_item(grandchild_id);
        grandchild.parent_id = Some(child_id);
        grandchild.flexible_constraints = json!({"unsupported_descendant_metadata": true});

        let result = compose_items(vec![grandchild, root, child], preview_request()).unwrap();

        assert_eq!(result.source_item_count, 3);
        assert_eq!(result.accepted_item_count, 3);
        assert_eq!(result.source_item_revisions.len(), 3);
        assert!(result.rejected_items.is_empty());
        assert!(result.plan.blocks.is_empty());
        assert!(result.plan.unscheduled.is_empty());
        assert_eq!(
            result.accepted_item_count + result.rejected_items.len(),
            result.source_item_count
        );
    }

    #[test]
    fn planned_sibling_outside_an_inbox_subtree_remains_schedulable() {
        let root_id = Uuid::from_u128(104);
        let child_id = Uuid::from_u128(105);
        let sibling_id = Uuid::from_u128(106);
        let mut root = canonical_item(root_id);
        root.status = ItemStatus::Inbox;
        let mut child = canonical_item(child_id);
        child.parent_id = Some(root_id);
        let sibling = canonical_item(sibling_id);

        let result = compose_items(vec![child, sibling, root], preview_request()).unwrap();

        assert_eq!(result.source_item_count, 3);
        assert_eq!(result.accepted_item_count, 3);
        assert!(result.rejected_items.is_empty());
        assert!(result.plan.unscheduled.is_empty());
        assert_eq!(result.plan.blocks.len(), 1);
        assert_eq!(result.plan.blocks[0].item_id, Some(ItemId(sibling_id)));
    }

    #[test]
    fn changing_an_inbox_item_to_planned_makes_it_schedulable() {
        let item_id = Uuid::from_u128(107);
        let mut inbox = canonical_item(item_id);
        inbox.status = ItemStatus::Inbox;
        let inbox_result = compose_items(vec![inbox.clone()], preview_request()).unwrap();

        inbox.status = ItemStatus::Planned;
        inbox.revision += 1;
        let planned = compose_items(vec![inbox.clone()], preview_request()).unwrap();
        let repeated = compose_items(vec![inbox], preview_request()).unwrap();

        assert!(inbox_result.plan.blocks.is_empty());
        assert_ne!(inbox_result.input_digest, planned.input_digest);
        assert_eq!(planned.input_digest, repeated.input_digest);
        assert_eq!(planned.source_item_count, 1);
        assert_eq!(planned.accepted_item_count, 1);
        assert!(planned.rejected_items.is_empty());
        assert_eq!(planned.plan.blocks.len(), 1);
        assert_eq!(planned.plan.blocks[0].item_id, Some(ItemId(item_id)));
    }

    #[test]
    fn preview_digest_is_bound_to_the_scheduler_publication_schema() {
        let item = canonical_item(Uuid::from_u128(11));
        let current = compose_items(vec![item.clone()], preview_request()).unwrap();
        let upgraded = compose_items_for_schema(
            vec![item],
            preview_request(),
            "dayweave-scheduler-publication/test-upgrade",
        )
        .unwrap();
        assert_ne!(current.input_digest, upgraded.input_digest);
    }

    #[test]
    fn preview_digest_binds_hidden_calendar_generation_and_rejects_duplicate_fixed_input() {
        let item = canonical_item(Uuid::from_u128(12));
        let stamp = CalendarProjectionStamp {
            collection_id: Uuid::from_u128(13),
            collection_revision: 4,
            generation: 9,
            window_start: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            window_end: Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap(),
            refreshed_at: Utc.with_ymd_and_hms(2026, 8, 29, 8, 0, 0).unwrap(),
        };
        let without_projection = compose_items(vec![item.clone()], preview_request()).unwrap();
        let with_projection = compose_items_with_projection_for_schema(
            vec![item.clone()],
            preview_request(),
            super::super::SCHEDULER_PUBLICATION_SCHEMA,
            vec![stamp.clone()],
            AuthoritativePlanningEvidence::default(),
            Vec::new(),
        )
        .unwrap();

        assert_ne!(
            without_projection.input_digest,
            with_projection.input_digest
        );
        let encoded = serde_json::to_value(&with_projection).unwrap();
        assert!(encoded.get("calendar_projection_stamps").is_none());

        let mut duplicate = preview_request();
        duplicate.fixed_blocks.push(FixedBlockInput {
            id: Uuid::from_u128(14),
            is_sensitive: false,
            title: "Synthetic provider duplicate".to_owned(),
            start: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 11, 0, 0).unwrap(),
            source: FixedBlockSourceInput::GoogleCalendar,
        });
        assert!(matches!(
            compose_items_with_projection_for_schema(
                vec![item],
                duplicate,
                super::super::SCHEDULER_PUBLICATION_SCHEMA,
                vec![stamp],
                AuthoritativePlanningEvidence::default(),
                Vec::new(),
            ),
            Err(ComposeScheduleError::InvalidRequest(_))
        ));
    }

    #[test]
    fn rejects_unknown_metadata_and_ignores_stale_assignment() {
        let mut invalid_item = canonical_item(Uuid::from_u128(2));
        invalid_item.flexible_constraints = json!({"surprise": true});
        let valid_item = canonical_item(Uuid::from_u128(3));
        let mut request = preview_request();
        request.previous_assignments.push(PreviousAssignmentInput {
            item_id: valid_item.id,
            item_revision: 2,
            occurrence_id: None,
            blocks: Vec::new(),
            pinned: false,
        });
        let result = compose_items(vec![invalid_item, valid_item], request).unwrap();
        assert_eq!(result.rejected_items.len(), 1);
        assert_eq!(result.ignored_previous_assignments.len(), 1);
        assert_eq!(result.accepted_item_count, 1);
    }

    #[test]
    fn generated_calendar_preserves_dst_day_boundaries() {
        let mut request = preview_request();
        request.horizon_start = Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap();
        request.horizon_end = Utc.with_ymd_and_hms(2026, 10, 27, 0, 0, 0).unwrap();
        let result = compose_items(Vec::new(), request).unwrap();
        let elapsed: Vec<_> = result
            .plan
            .occurrences
            .iter()
            .map(|occurrence| occurrence.nominal_end - occurrence.nominal_start)
            .collect();
        assert!(elapsed.is_empty());

        let mut preparation_request = preview_request();
        preparation_request.horizon_start = Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap();
        preparation_request.horizon_end = Utc.with_ymd_and_hms(2026, 10, 27, 0, 0, 0).unwrap();
        let prepared = prepare_canonical_schedule(Vec::new(), preparation_request).unwrap();
        assert!(
            prepared
                .plan_request
                .recurrence_context
                .calendar
                .days
                .iter()
                .any(|day| { (day.end - day.start).whole_hours() == 25 })
        );
    }

    #[test]
    fn calendar_event_metadata_accepts_rfc3339_instants() {
        let mut item = canonical_item(Uuid::from_u128(4));
        item.kind = ItemKind::Event;
        item.duration_seconds = None;
        item.deadline_at = None;
        item.flexible_constraints = json!({
            "calendar_event": {
                "start": "2026-09-01T10:00:00+02:00",
                "end": "2026-09-01T11:00:00+02:00",
                "immutable": true,
                "all_day": false,
                "source_calendar_id": "primary"
            }
        });
        let mapped = map_plannable(&item);
        let PlanningItemKind::CalendarEvent(event) = mapped.kind else {
            panic!("expected calendar event");
        };
        assert_eq!((event.end - event.start).whole_minutes(), 60);
    }

    #[test]
    fn exact_calendar_event_reserves_capacity_without_provider_identifiers() {
        let event_id = Uuid::from_u128(40);
        let mut item = canonical_item(event_id);
        item.kind = ItemKind::Event;
        item.duration_seconds = None;
        item.deadline_at = None;
        item.flexible_constraints = json!({
            "calendar_event": {
                "start": "2026-09-01T10:00:00+02:00",
                "end": "2026-09-01T11:00:00+02:00",
                "immutable": true,
                "all_day": false,
                "source_calendar_id": null
            }
        });
        let mut unexpanded = item.clone();
        unexpanded.id = Uuid::from_u128(400);
        unexpanded.recurrence = Some(json!({"type": "daily", "times_per_day": 1}));

        let result = compose_items(vec![item], preview_request()).unwrap();

        assert_eq!(result.source_item_count, 1);
        assert_eq!(result.accepted_item_count, 1);
        assert!(result.rejected_items.is_empty());
        let block = result.plan.blocks.first().expect("calendar event block");
        assert_eq!(block.item_id, Some(ItemId(event_id)));
        assert_eq!(block.kind, dayweave_core::ScheduleBlockKind::CalendarEvent);
        assert_eq!((block.end - block.start).whole_minutes(), 60);

        let rejected = compose_items(vec![unexpanded], preview_request()).unwrap();
        assert_eq!(rejected.accepted_item_count, 0);
        assert_eq!(rejected.rejected_items.len(), 1);
    }

    #[test]
    fn legacy_owned_google_block_reserves_capacity_exactly_once() {
        let event_id = Uuid::from_u128(401);
        let mut owned = canonical_item(event_id);
        owned.kind = ItemKind::Event;
        owned.duration_seconds = None;
        owned.deadline_at = None;
        owned.flexible_constraints = json!({
            "dayweave_firm_block": {
                "owned": true,
                "starts_at": "2026-09-01T08:00:00Z",
                "ends_at": "2026-09-01T09:00:00Z",
                "all_day": false,
                "tentative": false,
                "busy": true
            }
        });

        let result = compose_items(vec![owned.clone()], preview_request()).unwrap();

        assert_eq!(result.accepted_item_count, 1);
        assert!(result.rejected_items.is_empty());
        let blocks: Vec<_> = result
            .plan
            .blocks
            .iter()
            .filter(|block| block.item_id == Some(ItemId(event_id)))
            .collect();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].kind,
            dayweave_core::ScheduleBlockKind::CalendarEvent
        );

        owned.flexible_constraints["dayweave_firm_block"]["owned"] = json!(false);
        let rejected = compose_items(vec![owned], preview_request()).unwrap();
        assert_eq!(rejected.accepted_item_count, 0);
        assert_eq!(rejected.rejected_items.len(), 1);
    }

    #[test]
    fn calendar_context_counts_as_accepted_without_reserving_capacity() {
        let context_id = Uuid::from_u128(41);
        let mut context = canonical_item(context_id);
        context.kind = ItemKind::Event;
        context.duration_seconds = None;
        context.deadline_at = None;
        context.flexible_constraints = json!({
            "calendar_context": {
                "start": "2026-09-01T10:00:00+02:00",
                "end": "2026-09-01T11:00:00+02:00",
                "all_day": false
            }
        });
        let task_id = Uuid::from_u128(42);
        let task = canonical_item(task_id);

        let result = compose_items(vec![context, task], preview_request()).unwrap();

        assert_eq!(result.source_item_count, 2);
        assert_eq!(result.accepted_item_count, 2);
        assert!(result.rejected_items.is_empty());
        assert!(
            result
                .plan
                .blocks
                .iter()
                .all(|block| block.item_id != Some(ItemId(context_id)))
        );
        assert!(
            result
                .plan
                .blocks
                .iter()
                .any(|block| block.item_id == Some(ItemId(task_id)))
        );
    }

    #[test]
    fn malformed_provider_constraints_are_rejected_without_leaking_values() {
        const RAW_PROVIDER_ID: &str = "SYNTHETIC-REMOTE-ID-MUST-NOT-LEAK";
        let mut malformed = canonical_item(Uuid::from_u128(43));
        malformed.kind = ItemKind::Event;
        malformed.duration_seconds = None;
        malformed.deadline_at = None;
        malformed.flexible_constraints = json!({
            "calendar_context": {
                "start": "2026-09-01T10:00:00+02:00",
                "end": "2026-09-01T11:00:00+02:00",
                "all_day": false,
                "remote_id": RAW_PROVIDER_ID
            }
        });
        let task = canonical_item(Uuid::from_u128(44));

        let result = compose_items(vec![malformed, task], preview_request()).unwrap();

        assert_eq!(result.source_item_count, 2);
        assert_eq!(result.accepted_item_count, 1);
        assert_eq!(result.rejected_items.len(), 1);
        assert_eq!(
            result.accepted_item_count + result.rejected_items.len(),
            result.source_item_count
        );
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains(RAW_PROVIDER_ID));
    }

    #[test]
    fn calendar_context_requires_one_valid_root_occurrence() {
        fn context_item(id: u128) -> Item {
            let mut item = canonical_item(Uuid::from_u128(id));
            item.kind = ItemKind::Event;
            item.duration_seconds = None;
            item.deadline_at = None;
            item.flexible_constraints = json!({
                "calendar_context": {
                    "start": "2026-09-01T10:00:00+02:00",
                    "end": "2026-09-01T11:00:00+02:00",
                    "all_day": false
                }
            });
            item
        }

        let mut recurring = context_item(45);
        recurring.recurrence = Some(json!({"type": "daily", "times_per_day": 1}));
        let mut child = context_item(46);
        child.parent_id = Some(Uuid::from_u128(1));
        let mut reversed = context_item(47);
        reversed.flexible_constraints["calendar_context"]["end"] =
            json!("2026-09-01T09:00:00+02:00");
        let mut ambiguous = context_item(48);
        ambiguous.flexible_constraints["calendar_event"] = json!({
            "start": "2026-09-01T10:00:00+02:00",
            "end": "2026-09-01T11:00:00+02:00",
            "immutable": true,
            "all_day": false,
            "source_calendar_id": null
        });

        for item in [recurring, child, reversed, ambiguous] {
            let result = compose_items(vec![item], preview_request()).unwrap();
            assert_eq!(result.source_item_count, 1);
            assert_eq!(result.accepted_item_count, 0);
            assert_eq!(result.rejected_items.len(), 1);
        }
    }

    #[test]
    fn nested_constraints_and_recurrence_context_use_strict_rfc3339() {
        let mut item = canonical_item(Uuid::from_u128(5));
        item.deadline_at = None;
        item.flexible_constraints = json!({
            "constraints": {
                "earliest_start": {
                    "value": "2026-09-01T08:00:00+02:00",
                    "strength": {"level": "hard"}
                },
                "preferred_absolute_windows": [{
                    "value": {
                        "start": "2026-09-01T09:00:00+02:00",
                        "end": "2026-09-01T11:00:00+02:00"
                    },
                    "strength": {"level": "soft", "weight": 25}
                }]
            }
        });
        let mapped = map_plannable(&item);
        assert_eq!(mapped.constraints.earliest_start.unwrap().value.hour(), 8);

        let item_id = item.id.to_string();
        let mut request = serde_json::to_value(preview_request()).unwrap();
        request["recurrence_context"] = json!({
            "completion_anchors": {(item_id): "2026-08-31T18:00:00Z"}
        });
        let decoded: ComposeScheduleRequest = serde_json::from_value(request).unwrap();
        assert_eq!(decoded.recurrence_context.completion_anchors.len(), 1);

        let mut invalid = item;
        invalid.flexible_constraints = json!({
            "constraints": {
                "preferred_absolute_windows": [{
                    "value": {
                        "start": "2026-09-01T09:00:00+02:00",
                        "end": "2026-09-01T11:00:00+02:00",
                        "unexpected": true
                    },
                    "strength": {"level": "hard"}
                }]
            }
        });
        let result = compose_items(vec![invalid], preview_request()).unwrap();
        assert_eq!(result.accepted_item_count, 0);
        assert_eq!(result.rejected_items.len(), 1);
    }

    #[test]
    fn public_occurrence_output_includes_stable_recurrence_identity() {
        let date = time::Date::from_calendar_date(2026, time::Month::September, 1).unwrap();
        let nominal_start = time::OffsetDateTime::from_unix_timestamp(1_788_236_400).unwrap();
        let occurrence = dayweave_core::Occurrence {
            id: dayweave_core::OccurrenceId(Uuid::from_u128(201)),
            series_item_id: dayweave_core::ItemId(Uuid::from_u128(202)),
            identity: dayweave_core::RecurrenceOccurrenceIdentity::CalendarDay {
                date,
                bucket_ordinal: 2,
            },
            nominal_start,
            nominal_end: nominal_start + time::Duration::hours(1),
            window_start: nominal_start,
            window_end: nominal_start + time::Duration::hours(1),
            local_date: Some(date),
            ordinal: 2,
            state: dayweave_core::OccurrenceState::Generated,
        };

        let output = serde_json::to_value(
            OccurrenceOutput::try_from(&occurrence).expect("occurrence must encode"),
        )
        .unwrap();
        assert_eq!(
            output["identity"],
            json!({
                "type": "calendar_day",
                "date": "2026-09-01",
                "bucket_ordinal": 2
            })
        );
    }
}
