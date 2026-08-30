use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    AvailabilityWindow, ConstraintStrength, DayOfWeek, Dependency, DependencyRelation,
    ExecutionDisposition, ExecutionPlanningContext, ExecutionReservation, ExecutionReservationKind,
    ExecutionWorkUnit, FixedBlockSource, ItemId, ItemKind, MaterializedIdentity, Minutes,
    Occurrence, OccurrenceId, PlanRequest, PreviousAssignment, PreviousBlock,
    SchedulingConstraints, SplitPolicy, WorkItem, materialize_recurrences,
    roll_up_expected_durations,
};

const MAX_MANUAL_ASSESSMENT_VIOLATIONS: usize = 4_096;
const MAX_MANUAL_ASSESSMENT_CONFLICT_FACTS: usize = 4_096;
const MAX_IMMUTABLE_OVERLAP_VIOLATIONS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulePlan {
    pub as_of: OffsetDateTime,
    pub horizon_start: OffsetDateTime,
    pub horizon_end: OffsetDateTime,
    pub blocks: Vec<ScheduleBlock>,
    pub unscheduled: Vec<UnscheduledWork>,
    pub decisions: Vec<PlanDecision>,
    pub violations: Vec<PlanViolation>,
    pub score: PlanScore,
    #[serde(default)]
    pub occurrences: Vec<Occurrence>,
    /// Exact hard-rule and immutable-block conflicts caused by explicit
    /// caller-requested pinned placements. Automatic scheduling never creates
    /// entries here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_placement_assessments: Vec<ManualPlacementAssessment>,
}

impl SchedulePlan {
    pub fn blocks_for(&self, item_id: ItemId) -> impl Iterator<Item = &ScheduleBlock> {
        self.blocks
            .iter()
            .filter(move |block| block.item_id == Some(item_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleBlock {
    pub id: Uuid,
    /// Effective source sensitivity. This is output metadata only.
    pub is_sensitive: bool,
    pub item_id: Option<ItemId>,
    #[serde(default)]
    pub occurrence_id: Option<OccurrenceId>,
    pub external_block_id: Option<Uuid>,
    pub title: String,
    pub start: OffsetDateTime,
    pub end: OffsetDateTime,
    pub session_index: u16,
    pub kind: ScheduleBlockKind,
    pub explanations: Vec<PlacementExplanation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleBlockKind {
    Planned,
    Pinned,
    CalendarEvent,
    ExternalFixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementExplanation {
    pub code: ExplanationCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplanationCode {
    FixedEvent,
    Pinned,
    HardDeadline,
    GoalProgress,
    HabitOrRoutine,
    Priority,
    PreferredWindow,
    ContextMatch,
    EnergyMatch,
    Dependency,
    StableTime,
    EarliestAvailable,
    SplitSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnscheduledWork {
    pub item_id: ItemId,
    #[serde(default)]
    pub occurrence_id: Option<OccurrenceId>,
    pub remaining: Minutes,
    pub reason: UnscheduledReason,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnscheduledReason {
    MissingDuration,
    NoCapacity,
    HardConstraint,
    Blocked,
    DependencyUnavailable,
    DependencyCycle,
    SessionLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDecision {
    pub item_id: ItemId,
    #[serde(default)]
    pub occurrence_id: Option<OccurrenceId>,
    pub kind: DecisionKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    ContainerRolledUp,
    TerminalItemIgnored,
    FixedEventRetained,
    Scheduled,
    PartiallyScheduled,
    KeptPinned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanViolation {
    pub kind: ViolationKind,
    pub severity: ViolationSeverity,
    pub item_ids: Vec<ItemId>,
    #[serde(default)]
    pub occurrence_ids: Vec<OccurrenceId>,
    pub start: Option<OffsetDateTime>,
    pub end: Option<OffsetDateTime>,
    pub penalty: u64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualPlacementAssessment {
    pub placement_id: Uuid,
    pub environment_digest: [u8; 32],
    pub violations: Vec<ManualPlacementViolation>,
}

/// Stable, content-free evidence the UI can show and bind to an approval.
///
/// Titles and notes are intentionally excluded. Clients may resolve visible
/// labels from their sensitivity-aware cache while the server hashes these
/// machine facts into a content-bound publication approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualPlacementViolation {
    pub code: ManualPlacementViolationCode,
    pub item_ids: Vec<ItemId>,
    #[serde(default)]
    pub occurrence_ids: Vec<OccurrenceId>,
    #[serde(default)]
    pub conflicting_block_ids: Vec<Uuid>,
    #[serde(default)]
    pub conflicting_blocks: Vec<ManualPlacementConflict>,
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub boundary_start: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub boundary_end: Option<OffsetDateTime>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualPlacementConflict {
    pub block_id: Uuid,
    pub item_id: Option<ItemId>,
    pub occurrence_id: Option<OccurrenceId>,
    pub external_block_id: Option<Uuid>,
    pub kind: ScheduleBlockKind,
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualPlacementViolationCode {
    OutsideAvailability,
    EarliestStart,
    LatestFinish,
    MinimumNotice,
    AllowedWeekday,
    PreferredDailyWindow,
    PreferredAbsoluteWindow,
    ForbiddenWindow,
    RequiredContext,
    RequiredLocation,
    RequiredCapabilities,
    Energy,
    Dependency,
    MaximumDailyWork,
    MaximumWeeklyWork,
    BufferCompressed,
    ImmutableOverlap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    SoftConstraint,
    FixedOverlap,
    PinnedConflict,
    DeadlineRisk,
    Dependency,
    BufferCompressed,
    Capacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationSeverity {
    Warning,
    Error,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanScore {
    pub scheduled_minutes: u32,
    pub unscheduled_minutes: u32,
    pub soft_penalty: u64,
    pub moved_minutes: u32,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("planning horizon must have a positive duration")]
    InvalidHorizon,
    #[error("slot granularity must be greater than zero")]
    InvalidGranularity,
    #[error("duplicate item id {0}")]
    DuplicateItem(ItemId),
    #[error("invalid item {item_id}: {message}")]
    InvalidItem { item_id: ItemId, message: String },
    #[error("invalid {owner} window: end must be after start")]
    InvalidWindow { owner: String },
    #[error("previous assignment references missing item {0}")]
    MissingPreviousItem(ItemId),
    #[error("invalid hierarchy: {0}")]
    InvalidHierarchy(String),
    #[error("invalid recurrence: {0}")]
    InvalidRecurrence(String),
    #[error("schedule conflict evidence exceeds the supported limit")]
    ConflictEvidenceLimit,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Scheduler;

impl Scheduler {
    /// Computes a plan with no I/O or implicit time source.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when the request is structurally invalid.
    /// Capacity and constraint conflicts are represented in the returned plan,
    /// not as errors.
    pub fn plan(&self, request: &PlanRequest) -> Result<SchedulePlan, ScheduleError> {
        self.plan_with_execution(request, &ExecutionPlanningContext::default())
    }

    /// Computes a plan using a normalized, server-authoritative execution
    /// snapshot without adding execution fields to [`PlanRequest`].
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when either input is structurally invalid or
    /// an execution reservation is only partly covered by the horizon.
    pub fn plan_with_execution(
        &self,
        request: &PlanRequest,
        execution: &ExecutionPlanningContext,
    ) -> Result<SchedulePlan, ScheduleError> {
        validate_request(request)?;
        validate_execution_context(request, execution)?;
        let materialized = materialize_recurrences(request)
            .map_err(|error| ScheduleError::InvalidRecurrence(error.to_string()))?;
        let (materialized_request, materialized_execution) =
            apply_execution_context(&materialized.request, &materialized.identities, execution)?;
        let mut plan = Self::plan_materialized(&materialized_request, &materialized_execution)?;
        plan.manual_placement_assessments = assess_manual_placements(&materialized_request, &plan)?;
        for violation in plan
            .manual_placement_assessments
            .iter()
            .flat_map(|assessment| &assessment.violations)
        {
            plan.violations.push(violation.as_plan_violation());
        }
        remap_occurrence_outputs(&mut plan, &materialized.identities);
        plan.occurrences = materialized.occurrences;
        Ok(plan)
    }

    fn plan_materialized(
        request: &PlanRequest,
        execution: &MaterializedExecutionContext,
    ) -> Result<SchedulePlan, ScheduleError> {
        validate_request(request)?;
        validate_manual_placement_demand(request, execution)?;

        let items: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
        let children = child_map(&request.items);
        let mut state = PlanningState::new(request, execution);

        state.add_external_fixed_blocks(request);
        state.add_calendar_events(request);
        state.add_execution_reservations(request, &items, execution)?;
        state.add_pinned_assignments(request, &items);
        state.detect_immutable_overlaps()?;

        let mut eligible = Vec::new();
        for item in &request.items {
            if matches!(item.kind, ItemKind::CalendarEvent(_)) {
                continue;
            }
            if item.status.is_terminal() {
                state.decisions.push(PlanDecision {
                    item_id: item.id,
                    occurrence_id: None,
                    kind: DecisionKind::TerminalItemIgnored,
                    message: "Completed, skipped, or canceled work does not reserve future time."
                        .to_owned(),
                });
                continue;
            }
            if item.status == crate::WorkStatus::Blocked {
                state.unscheduled.push(UnscheduledWork {
                    item_id: item.id,
                    occurrence_id: None,
                    remaining: item
                        .duration
                        .map_or(Minutes::ZERO, crate::DurationEstimate::planning_minutes),
                    reason: UnscheduledReason::Blocked,
                    message: "Blocked work waits in the plan until its blocker is resolved."
                        .to_owned(),
                });
                continue;
            }
            let has_children = children
                .get(&item.id)
                .is_some_and(|value| !value.is_empty());
            if !item.occupies_time(has_children) {
                state.decisions.push(PlanDecision {
                    item_id: item.id,
                    occurrence_id: None,
                    kind: DecisionKind::ContainerRolledUp,
                    message: "This parent is represented by its schedulable leaf descendants."
                        .to_owned(),
                });
                continue;
            }
            eligible.push(item.id);
        }

        let dependencies = dependencies_with_routine_order(&request.items, &children);
        let (ordered, cyclic) = dependency_order(eligible, &items, &dependencies);

        let mut outcomes = BTreeMap::<ItemId, bool>::new();
        for item_id in ordered {
            let item = items[&item_id];
            let item_dependencies = dependencies.get(&item_id).map_or(&[][..], Vec::as_slice);
            if hard_dependency_unavailable(item_dependencies, &items, &outcomes) {
                let remaining = item
                    .duration
                    .map_or(Minutes::ZERO, crate::DurationEstimate::planning_minutes);
                state.unscheduled.push(UnscheduledWork {
                    item_id,
                    occurrence_id: None,
                    remaining,
                    reason: UnscheduledReason::DependencyUnavailable,
                    message: "A hard predecessor could not be placed in this plan.".to_owned(),
                });
                outcomes.insert(item_id, false);
                continue;
            }

            let outcome = state.schedule_item(request, item, item_dependencies, &items);
            outcomes.insert(item_id, outcome);
        }

        for item_id in cyclic {
            let item = items[&item_id];
            let remaining = item
                .duration
                .map_or(Minutes::ZERO, crate::DurationEstimate::planning_minutes);
            state.unscheduled.push(UnscheduledWork {
                item_id,
                occurrence_id: None,
                remaining,
                reason: UnscheduledReason::DependencyCycle,
                message: "Hard dependencies form a cycle; edit or soften one dependency."
                    .to_owned(),
            });
        }

        Ok(state.finish(request))
    }
}

fn remap_occurrence_outputs(
    plan: &mut SchedulePlan,
    identities: &BTreeMap<ItemId, MaterializedIdentity>,
) {
    for block in &mut plan.blocks {
        let Some(internal_id) = block.item_id else {
            continue;
        };
        if let Some(identity) = identities.get(&internal_id) {
            block.item_id = Some(identity.series_item_id);
            block.occurrence_id = Some(identity.occurrence_id);
        }
    }
    for work in &mut plan.unscheduled {
        if let Some(identity) = identities.get(&work.item_id) {
            work.item_id = identity.series_item_id;
            work.occurrence_id = Some(identity.occurrence_id);
        }
    }
    for decision in &mut plan.decisions {
        if let Some(identity) = identities.get(&decision.item_id) {
            decision.item_id = identity.series_item_id;
            decision.occurrence_id = Some(identity.occurrence_id);
        }
    }
    for violation in &mut plan.violations {
        for item_id in &mut violation.item_ids {
            if let Some(identity) = identities.get(item_id) {
                *item_id = identity.series_item_id;
                violation.occurrence_ids.push(identity.occurrence_id);
            }
        }
        violation.item_ids.sort_unstable();
        violation.item_ids.dedup();
        violation.occurrence_ids.sort_unstable();
        violation.occurrence_ids.dedup();
    }
    for assessment in &mut plan.manual_placement_assessments {
        for violation in &mut assessment.violations {
            for item_id in &mut violation.item_ids {
                if let Some(identity) = identities.get(item_id) {
                    *item_id = identity.series_item_id;
                    violation.occurrence_ids.push(identity.occurrence_id);
                }
            }
            for conflict in &mut violation.conflicting_blocks {
                let Some(item_id) = conflict.item_id else {
                    continue;
                };
                if let Some(identity) = identities.get(&item_id) {
                    conflict.item_id = Some(identity.series_item_id);
                    conflict.occurrence_id = Some(identity.occurrence_id);
                }
            }
            violation.item_ids.sort_unstable();
            violation.item_ids.dedup();
            violation.occurrence_ids.sort_unstable();
            violation.occurrence_ids.dedup();
        }
    }
}

#[derive(Debug, Clone, Default)]
struct MaterializedExecutionContext {
    work_units: BTreeMap<ItemId, MaterializedExecutionWorkUnit>,
}

#[derive(Debug, Clone)]
struct MaterializedExecutionWorkUnit {
    skipped: bool,
    used_session_indices: BTreeSet<u16>,
    reservations: Vec<ExecutionReservation>,
}

impl MaterializedExecutionWorkUnit {
    fn high_water(&self) -> Option<u16> {
        self.used_session_indices
            .iter()
            .copied()
            .chain(
                self.reservations
                    .iter()
                    .map(|reservation| reservation.session_index),
            )
            .max()
    }
}

fn validate_execution_context(
    request: &PlanRequest,
    execution: &ExecutionPlanningContext,
) -> Result<(), ScheduleError> {
    if !execution.work_units.is_empty() && execution.snapshot_revision == 0 {
        return Err(invalid_execution(
            execution.work_units[0].item_id,
            "snapshot revision must be positive when work-unit evidence is present",
        ));
    }
    let item_ids: BTreeSet<_> = request.items.iter().map(|item| item.id).collect();
    let mut identities = BTreeSet::new();
    for unit in &execution.work_units {
        if !item_ids.contains(&unit.item_id) {
            return Err(invalid_execution(
                unit.item_id,
                format!("work unit references missing item {}", unit.item_id),
            ));
        }
        if !identities.insert((unit.item_id, unit.occurrence_id)) {
            return Err(invalid_execution(
                unit.item_id,
                format!(
                    "duplicate work unit for item {} and occurrence {:?}",
                    unit.item_id, unit.occurrence_id
                ),
            ));
        }
        validate_execution_work_unit(unit)?;
    }
    Ok(())
}

fn validate_execution_work_unit(unit: &ExecutionWorkUnit) -> Result<(), ScheduleError> {
    if unit.progress_epoch == 0 {
        return Err(invalid_execution(
            unit.item_id,
            format!(
                "work unit for item {} has a zero progress epoch",
                unit.item_id
            ),
        ));
    }
    if unit.disposition == Some(ExecutionDisposition::Skipped) && !unit.reservations.is_empty() {
        return Err(invalid_execution(
            unit.item_id,
            format!(
                "skipped work unit for item {} cannot retain reservations",
                unit.item_id
            ),
        ));
    }

    let mut used = BTreeSet::new();
    for index in &unit.used_session_indices {
        if !used.insert(*index) {
            return Err(invalid_execution(
                unit.item_id,
                format!(
                    "work unit for item {} repeats historical session index {index}",
                    unit.item_id
                ),
            ));
        }
    }
    let historical_high_water = used.iter().next_back().copied();
    let mut reservation_indices = BTreeSet::new();
    for reservation in &unit.reservations {
        validate_execution_reservation(unit.item_id, reservation, &used, historical_high_water)?;
        if !reservation_indices.insert(reservation.session_index) {
            return Err(invalid_execution(
                unit.item_id,
                format!(
                    "work unit for item {} repeats reservation index {}",
                    unit.item_id, reservation.session_index
                ),
            ));
        }
    }
    Ok(())
}

fn validate_execution_reservation(
    item_id: ItemId,
    reservation: &ExecutionReservation,
    used: &BTreeSet<u16>,
    historical_high_water: Option<u16>,
) -> Result<(), ScheduleError> {
    if reservation.start >= reservation.end {
        return Err(invalid_execution(
            item_id,
            format!(
                "reservation {} for item {item_id} has an empty window",
                reservation.session_index
            ),
        ));
    }
    match reservation.kind {
        ExecutionReservationKind::InFlight if !used.contains(&reservation.session_index) => {
            Err(invalid_execution(
                item_id,
                format!(
                    "in-flight reservation {} for item {item_id} is not historical",
                    reservation.session_index
                ),
            ))
        }
        ExecutionReservationKind::DeferredReplacement {
            source_session_index,
        } if !used.contains(&source_session_index) => Err(invalid_execution(
            item_id,
            format!(
                "deferred source index {source_session_index} for item {item_id} is not historical"
            ),
        )),
        ExecutionReservationKind::DeferredReplacement { .. }
            if used.contains(&reservation.session_index)
                || historical_high_water.is_some_and(|high| reservation.session_index <= high) =>
        {
            Err(invalid_execution(
                item_id,
                format!(
                    "deferred replacement index {} for item {item_id} is not fresh and monotonic",
                    reservation.session_index
                ),
            ))
        }
        ExecutionReservationKind::InFlight
        | ExecutionReservationKind::DeferredReplacement { .. } => Ok(()),
    }
}

fn invalid_execution(item_id: ItemId, message: impl Into<String>) -> ScheduleError {
    invalid_item(
        item_id,
        format!("invalid execution planning context: {}", message.into()),
    )
}

fn apply_execution_context(
    materialized_request: &PlanRequest,
    identities: &BTreeMap<ItemId, MaterializedIdentity>,
    execution: &ExecutionPlanningContext,
) -> Result<(PlanRequest, MaterializedExecutionContext), ScheduleError> {
    let source: BTreeMap<_, _> = execution
        .work_units
        .iter()
        .map(|unit| ((unit.item_id, unit.occurrence_id), unit))
        .collect();
    let mut request = materialized_request.clone();
    let mut materialized = MaterializedExecutionContext::default();
    let mut matched = BTreeSet::new();

    for item in &mut request.items {
        let identity = identities
            .get(&item.id)
            .map_or((item.id, None), |identity| {
                (identity.series_item_id, Some(identity.occurrence_id))
            });
        let Some(unit) = source.get(&identity) else {
            continue;
        };
        matched.insert(identity);
        if unit.disposition == Some(ExecutionDisposition::Skipped) {
            item.status = crate::WorkStatus::Skipped;
        } else if let Some(duration) = &mut item.duration {
            let credited_minutes = ceil_seconds_to_minutes(unit.credited_seconds);
            duration.remaining = Some(Minutes(
                duration.expected.get().saturating_sub(credited_minutes),
            ));
        }
        materialized.work_units.insert(
            item.id,
            MaterializedExecutionWorkUnit {
                skipped: unit.disposition == Some(ExecutionDisposition::Skipped),
                used_session_indices: unit.used_session_indices.iter().copied().collect(),
                reservations: unit.reservations.clone(),
            },
        );
    }

    let horizon = Interval {
        start: request.horizon_start,
        end: request.horizon_end,
    };
    for (identity, unit) in source {
        if matched.contains(&identity) {
            continue;
        }
        if let Some(reservation) = unit.reservations.iter().find(|reservation| {
            horizon.overlaps(Interval {
                start: reservation.start,
                end: reservation.end,
            })
        }) {
            return Err(invalid_execution(
                identity.0,
                format!(
                    "reservation for occurrence {:?}, session {} does not map to materialized work",
                    identity.1, reservation.session_index
                ),
            ));
        }
    }

    request.previous_assignments.retain_mut(|assignment| {
        if let Some(unit) = materialized.work_units.get(&assignment.item_id) {
            if unit.skipped {
                assignment.blocks.clear();
            } else if let Some(high_water) = unit.high_water() {
                assignment
                    .blocks
                    .retain(|block| block.session_index > high_water);
            }
        }
        !assignment.blocks.is_empty()
    });

    Ok((request, materialized))
}

fn ceil_seconds_to_minutes(seconds: u64) -> u32 {
    let minutes = seconds.saturating_add(59) / 60;
    u32::try_from(minutes).unwrap_or(u32::MAX)
}

#[derive(Debug, Clone, Copy)]
struct Interval {
    start: OffsetDateTime,
    end: OffsetDateTime,
}

impl Interval {
    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    fn clipped(self, bounds: Self) -> Option<Self> {
        let value = Self {
            start: self.start.max(bounds.start),
            end: self.end.min(bounds.end),
        };
        (value.start < value.end).then_some(value)
    }

    fn minutes(self) -> u32 {
        u32::try_from((self.end - self.start).whole_minutes().max(0)).unwrap_or(u32::MAX)
    }
}

impl ManualPlacementViolation {
    fn as_plan_violation(&self) -> PlanViolation {
        let kind = match self.code {
            ManualPlacementViolationCode::LatestFinish => ViolationKind::DeadlineRisk,
            ManualPlacementViolationCode::Dependency => ViolationKind::Dependency,
            ManualPlacementViolationCode::BufferCompressed => ViolationKind::BufferCompressed,
            ManualPlacementViolationCode::OutsideAvailability
            | ManualPlacementViolationCode::MaximumDailyWork
            | ManualPlacementViolationCode::MaximumWeeklyWork => ViolationKind::Capacity,
            ManualPlacementViolationCode::ImmutableOverlap => ViolationKind::PinnedConflict,
            ManualPlacementViolationCode::EarliestStart
            | ManualPlacementViolationCode::MinimumNotice
            | ManualPlacementViolationCode::AllowedWeekday
            | ManualPlacementViolationCode::PreferredDailyWindow
            | ManualPlacementViolationCode::PreferredAbsoluteWindow
            | ManualPlacementViolationCode::ForbiddenWindow
            | ManualPlacementViolationCode::RequiredContext
            | ManualPlacementViolationCode::RequiredLocation
            | ManualPlacementViolationCode::RequiredCapabilities
            | ManualPlacementViolationCode::Energy => ViolationKind::SoftConstraint,
        };
        let mut item_ids = self.item_ids.clone();
        item_ids.extend(
            self.conflicting_blocks
                .iter()
                .filter_map(|block| block.item_id),
        );
        item_ids.sort_unstable();
        item_ids.dedup();
        let mut occurrence_ids = self.occurrence_ids.clone();
        occurrence_ids.extend(
            self.conflicting_blocks
                .iter()
                .filter_map(|block| block.occurrence_id),
        );
        occurrence_ids.sort_unstable();
        occurrence_ids.dedup();
        PlanViolation {
            kind,
            severity: ViolationSeverity::Error,
            item_ids,
            occurrence_ids,
            start: Some(self.start),
            end: Some(self.end),
            penalty: 0,
            message: self.message.clone(),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn assess_manual_placements(
    request: &PlanRequest,
    plan: &SchedulePlan,
) -> Result<Vec<ManualPlacementAssessment>, ScheduleError> {
    let items: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
    let children = child_map(&request.items);
    let dependencies = dependencies_with_routine_order(&request.items, &children);
    let mut assessments = BTreeMap::<Uuid, Vec<ManualPlacementViolation>>::new();
    let mut violation_count = 0_usize;
    let mut conflict_fact_count = 0_usize;
    let mut overflow_item_id = None;

    for assignment in request
        .previous_assignments
        .iter()
        .filter(|assignment| assignment.pinned && assignment.manual_placement_id.is_some())
    {
        let placement_id = assignment
            .manual_placement_id
            .expect("filtered manual placement identifier");
        assessments.entry(placement_id).or_default();
        let Some(item) = items.get(&assignment.item_id).copied() else {
            continue;
        };
        let item_dependencies = dependencies.get(&item.id).map_or(&[][..], Vec::as_slice);

        for source_block in &assignment.blocks {
            if overflow_item_id.is_some() {
                break;
            }
            let interval = Interval {
                start: source_block.start,
                end: source_block.end,
            };
            let occurrence_ids = assignment.occurrence_id.into_iter().collect::<Vec<_>>();
            let mut push = |code: ManualPlacementViolationCode,
                            conflicting_blocks: Vec<&ScheduleBlock>,
                            boundary_start: Option<OffsetDateTime>,
                            boundary_end: Option<OffsetDateTime>,
                            message: &'static str| {
                if overflow_item_id.is_some()
                    || violation_count >= MAX_MANUAL_ASSESSMENT_VIOLATIONS
                    || conflict_fact_count
                        .checked_add(conflicting_blocks.len())
                        .is_none_or(|count| count > MAX_MANUAL_ASSESSMENT_CONFLICT_FACTS)
                {
                    overflow_item_id = Some(item.id);
                    return;
                }
                violation_count += 1;
                conflict_fact_count += conflicting_blocks.len();
                let mut conflicting_blocks = conflicting_blocks
                    .into_iter()
                    .map(manual_placement_conflict)
                    .collect::<Vec<_>>();
                conflicting_blocks.sort_by_key(|block| (block.start, block.end, block.block_id));
                conflicting_blocks.dedup();
                let conflicting_block_ids = conflicting_blocks
                    .iter()
                    .map(|block| block.block_id)
                    .collect();
                assessments
                    .entry(placement_id)
                    .or_default()
                    .push(ManualPlacementViolation {
                        code,
                        item_ids: vec![item.id],
                        occurrence_ids: occurrence_ids.clone(),
                        conflicting_block_ids,
                        conflicting_blocks,
                        start: interval.start,
                        end: interval.end,
                        boundary_start,
                        boundary_end,
                        message: message.to_owned(),
                    });
            };

            let containing_availability: Vec<_> = request
                .availability
                .iter()
                .filter(|availability| {
                    Interval {
                        start: availability.start,
                        end: availability.end,
                    }
                    .contains(interval)
                })
                .collect();
            if containing_availability.is_empty() {
                push(
                    ManualPlacementViolationCode::OutsideAvailability,
                    Vec::new(),
                    None,
                    None,
                    "Manual placement is outside configured availability.",
                );
            }

            let constraints = &item.constraints;
            if let Some(boundary) = &constraints.earliest_start
                && boundary.strength.is_hard()
                && interval.start < boundary.value
            {
                push(
                    ManualPlacementViolationCode::EarliestStart,
                    Vec::new(),
                    Some(boundary.value),
                    None,
                    "Manual placement starts before a hard earliest-start boundary.",
                );
            }
            if let Some(boundary) = &constraints.latest_finish
                && boundary.strength.is_hard()
                && interval.end > boundary.value
            {
                push(
                    ManualPlacementViolationCode::LatestFinish,
                    Vec::new(),
                    None,
                    Some(boundary.value),
                    "Manual placement ends after a hard latest-finish boundary.",
                );
            }
            if let Some(notice) = &constraints.minimum_notice
                && notice.strength.is_hard()
            {
                let required = request.as_of + Duration::minutes(i64::from(notice.value.get()));
                if interval.start < required {
                    push(
                        ManualPlacementViolationCode::MinimumNotice,
                        Vec::new(),
                        Some(required),
                        None,
                        "Manual placement compresses a hard minimum-notice boundary.",
                    );
                }
            }
            if let Some(weekdays) = &constraints.allowed_weekdays
                && weekdays.strength.is_hard()
                && !weekdays
                    .value
                    .contains(&DayOfWeek::from_time(interval.start.weekday()))
            {
                push(
                    ManualPlacementViolationCode::AllowedWeekday,
                    Vec::new(),
                    None,
                    None,
                    "Manual placement is on a disallowed weekday.",
                );
            }

            let hard_daily: Vec<_> = constraints
                .preferred_daily_windows
                .iter()
                .filter(|window| window.strength.is_hard())
                .collect();
            if !hard_daily.is_empty()
                && !hard_daily
                    .iter()
                    .any(|window| window.value.contains(interval.start, interval.end))
            {
                push(
                    ManualPlacementViolationCode::PreferredDailyWindow,
                    Vec::new(),
                    None,
                    None,
                    "Manual placement is outside every allowed daily window.",
                );
            }
            let hard_absolute: Vec<_> = constraints
                .preferred_absolute_windows
                .iter()
                .filter(|window| window.strength.is_hard())
                .collect();
            if !hard_absolute.is_empty()
                && !hard_absolute.iter().any(|window| {
                    Interval {
                        start: window.value.start,
                        end: window.value.end,
                    }
                    .contains(interval)
                })
            {
                push(
                    ManualPlacementViolationCode::PreferredAbsoluteWindow,
                    Vec::new(),
                    hard_absolute.iter().map(|window| window.value.start).min(),
                    hard_absolute.iter().map(|window| window.value.end).max(),
                    "Manual placement is outside every allowed absolute window.",
                );
            }
            for forbidden in constraints
                .forbidden_windows
                .iter()
                .filter(|window| window.strength.is_hard())
            {
                let forbidden_interval = Interval {
                    start: forbidden.value.start,
                    end: forbidden.value.end,
                };
                if interval.overlaps(forbidden_interval) {
                    push(
                        ManualPlacementViolationCode::ForbiddenWindow,
                        Vec::new(),
                        Some(forbidden.value.start),
                        Some(forbidden.value.end),
                        "Manual placement overlaps a hard forbidden window.",
                    );
                }
            }

            for required in constraints
                .required_contexts
                .iter()
                .filter(|required| required.strength.is_hard())
            {
                if !containing_availability
                    .iter()
                    .any(|availability| availability.contexts.contains(&required.value))
                {
                    push(
                        ManualPlacementViolationCode::RequiredContext,
                        Vec::new(),
                        None,
                        None,
                        "Manual placement lacks a required context.",
                    );
                }
            }
            if let Some(required) = &constraints.required_location
                && required.strength.is_hard()
                && !containing_availability
                    .iter()
                    .any(|availability| availability.location.as_ref() == Some(&required.value))
            {
                push(
                    ManualPlacementViolationCode::RequiredLocation,
                    Vec::new(),
                    None,
                    None,
                    "Manual placement lacks the required location.",
                );
            }
            if let Some(required) = &item.energy
                && required.strength.is_hard()
                && !containing_availability
                    .iter()
                    .any(|availability| availability.energy.satisfies(required.value))
            {
                push(
                    ManualPlacementViolationCode::Energy,
                    Vec::new(),
                    None,
                    None,
                    "Manual placement exceeds the available energy level.",
                );
            }
            let hard_contexts = constraints
                .required_contexts
                .iter()
                .filter(|required| required.strength.is_hard())
                .map(|required| &required.value)
                .collect::<Vec<_>>();
            let hard_location = constraints
                .required_location
                .as_ref()
                .filter(|required| required.strength.is_hard())
                .map(|required| &required.value);
            let hard_energy = item
                .energy
                .as_ref()
                .filter(|required| required.strength.is_hard())
                .map(|required| required.value);
            let has_hard_capabilities =
                !hard_contexts.is_empty() || hard_location.is_some() || hard_energy.is_some();
            if has_hard_capabilities
                && !containing_availability.iter().any(|availability| {
                    hard_contexts
                        .iter()
                        .all(|context| availability.contexts.contains(*context))
                        && hard_location
                            .is_none_or(|location| availability.location.as_ref() == Some(location))
                        && hard_energy.is_none_or(|energy| availability.energy.satisfies(energy))
                })
            {
                push(
                    ManualPlacementViolationCode::RequiredCapabilities,
                    Vec::new(),
                    None,
                    None,
                    "No single availability window satisfies every required work capability.",
                );
            }

            for dependency in item_dependencies
                .iter()
                .filter(|dependency| dependency.strength.is_hard())
            {
                if items
                    .get(&dependency.item_id)
                    .is_some_and(|predecessor| predecessor.status.is_terminal())
                {
                    continue;
                }
                let predecessor_blocks: Vec<_> = plan
                    .blocks
                    .iter()
                    .filter(|block| block.item_id == Some(dependency.item_id))
                    .collect();
                let predecessor_has_remaining_work = plan
                    .unscheduled
                    .iter()
                    .any(|work| work.item_id == dependency.item_id && !work.remaining.is_zero());
                let satisfied = if predecessor_blocks.is_empty() || predecessor_has_remaining_work {
                    false
                } else {
                    let predecessor_start = predecessor_blocks
                        .iter()
                        .map(|block| block.start)
                        .min()
                        .expect("non-empty predecessor blocks");
                    let predecessor_end = predecessor_blocks
                        .iter()
                        .map(|block| block.end)
                        .max()
                        .expect("non-empty predecessor blocks");
                    let lag = Duration::minutes(i64::from(dependency.minimum_lag.get()));
                    match dependency.relation {
                        DependencyRelation::FinishToStart => {
                            interval.start >= predecessor_end + lag
                        }
                        DependencyRelation::StartToStart => {
                            interval.start >= predecessor_start + lag
                        }
                        DependencyRelation::FinishToFinish => interval.end >= predecessor_end + lag,
                        DependencyRelation::StartToFinish => {
                            interval.end >= predecessor_start + lag
                        }
                    }
                };
                if !satisfied {
                    push(
                        ManualPlacementViolationCode::Dependency,
                        predecessor_blocks.clone(),
                        None,
                        None,
                        "Manual placement violates a hard dependency.",
                    );
                }
            }

            if let Some(limit) = &constraints.maximum_daily_work
                && limit.strength.is_hard()
            {
                let relevant_blocks = plan
                    .blocks
                    .iter()
                    .filter(|block| {
                        block.item_id == Some(item.id)
                            && block.start.date() == interval.start.date()
                    })
                    .collect::<Vec<_>>();
                let total = relevant_blocks.iter().fold(0_u32, |total, block| {
                    total.saturating_add(
                        Interval {
                            start: block.start,
                            end: block.end,
                        }
                        .minutes(),
                    )
                });
                if total > limit.value.get() {
                    push(
                        ManualPlacementViolationCode::MaximumDailyWork,
                        relevant_blocks,
                        None,
                        None,
                        "Manual placement exceeds a hard daily work limit.",
                    );
                }
            }
            if let Some(limit) = &constraints.maximum_weekly_work
                && limit.strength.is_hard()
            {
                let week = monday_of(interval.start);
                let relevant_blocks = plan
                    .blocks
                    .iter()
                    .filter(|block| {
                        block.item_id == Some(item.id) && monday_of(block.start) == week
                    })
                    .collect::<Vec<_>>();
                let total = relevant_blocks.iter().fold(0_u32, |total, block| {
                    total.saturating_add(
                        Interval {
                            start: block.start,
                            end: block.end,
                        }
                        .minutes(),
                    )
                });
                if total > limit.value.get() {
                    push(
                        ManualPlacementViolationCode::MaximumWeeklyWork,
                        relevant_blocks,
                        None,
                        None,
                        "Manual placement exceeds a hard weekly work limit.",
                    );
                }
            }

            if constraints
                .buffers
                .strength
                .is_some_and(ConstraintStrength::is_hard)
            {
                let expanded = Interval {
                    start: interval.start
                        - Duration::minutes(i64::from(constraints.buffers.before.get())),
                    end: interval.end
                        + Duration::minutes(i64::from(constraints.buffers.after.get())),
                };
                let conflicting: Vec<_> = plan
                    .blocks
                    .iter()
                    .filter(|block| {
                        !manual_block_matches(block, item.id, source_block)
                            && expanded.overlaps(Interval {
                                start: block.start,
                                end: block.end,
                            })
                    })
                    .collect();
                let fits_availability = request.availability.iter().any(|availability| {
                    Interval {
                        start: availability.start,
                        end: availability.end,
                    }
                    .contains(expanded)
                });
                if !fits_availability || !conflicting.is_empty() {
                    push(
                        ManualPlacementViolationCode::BufferCompressed,
                        conflicting,
                        Some(expanded.start),
                        Some(expanded.end),
                        "Manual placement compresses a hard preparation or decompression buffer.",
                    );
                }
            }

            let conflicting: Vec<_> = plan
                .blocks
                .iter()
                .filter(|block| {
                    matches!(
                        block.kind,
                        ScheduleBlockKind::Pinned
                            | ScheduleBlockKind::CalendarEvent
                            | ScheduleBlockKind::ExternalFixed
                    ) && !manual_block_matches(block, item.id, source_block)
                        && interval.overlaps(Interval {
                            start: block.start,
                            end: block.end,
                        })
                })
                .collect();
            if !conflicting.is_empty() {
                push(
                    ManualPlacementViolationCode::ImmutableOverlap,
                    conflicting,
                    None,
                    None,
                    "Manual placement overlaps immutable scheduled time.",
                );
            }
        }
    }

    if let Some(item_id) = overflow_item_id {
        return Err(invalid_item(
            item_id,
            "manual placement assessment exceeds the supported evidence limit",
        ));
    }
    if assessments.is_empty() {
        return Ok(Vec::new());
    }
    let environment_base_digest = manual_placement_environment_base_digest(request);
    Ok(assessments
        .into_iter()
        .map(|(placement_id, mut violations)| {
            for violation in &mut violations {
                violation.conflicting_block_ids.sort_unstable();
                violation.conflicting_block_ids.dedup();
                violation
                    .conflicting_blocks
                    .sort_by_key(|block| (block.start, block.end, block.block_id));
                violation.conflicting_blocks.dedup();
            }
            violations.sort_by_key(|violation| {
                (
                    violation.start,
                    violation.end,
                    violation.code,
                    violation.item_ids.clone(),
                    violation.conflicting_block_ids.clone(),
                )
            });
            violations.dedup();
            ManualPlacementAssessment {
                placement_id,
                environment_digest: manual_placement_environment_digest(
                    placement_id,
                    &environment_base_digest,
                ),
                violations,
            }
        })
        .collect())
}

fn manual_placement_conflict(block: &ScheduleBlock) -> ManualPlacementConflict {
    ManualPlacementConflict {
        block_id: block.id,
        item_id: block.item_id,
        occurrence_id: block.occurrence_id,
        external_block_id: block.external_block_id,
        kind: block.kind,
        start: block.start,
        end: block.end,
    }
}

fn manual_placement_environment_base_digest(request: &PlanRequest) -> [u8; 32] {
    #[derive(Serialize)]
    struct ItemEnvironment<'a> {
        id: ItemId,
        revision: u64,
        kind: &'a ItemKind,
        status: crate::WorkStatus,
        parent_id: Option<ItemId>,
        sibling_order: Option<u32>,
        has_own_effort: bool,
        duration: Option<crate::DurationEstimate>,
        constraints: &'a SchedulingConstraints,
        split_policy: &'a SplitPolicy,
        energy: &'a Option<crate::Qualified<crate::EnergyLevel>>,
    }
    #[derive(Serialize)]
    struct Environment<'a> {
        schema: &'static str,
        items: Vec<ItemEnvironment<'a>>,
        availability: &'a [AvailabilityWindow],
    }

    let mut items = request
        .items
        .iter()
        .map(|item| ItemEnvironment {
            id: item.id,
            revision: item.revision,
            kind: &item.kind,
            status: item.status,
            parent_id: item.parent_id,
            sibling_order: item.sibling_order,
            has_own_effort: item.has_own_effort,
            duration: item.duration,
            constraints: &item.constraints,
            split_policy: &item.split_policy,
            energy: &item.energy,
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.id);
    let encoded = serde_json::to_vec(&Environment {
        schema: "dayweave-manual-placement-environment-base/2",
        items,
        availability: &request.availability,
    })
    .expect("manual placement environment contains only serializable domain values");
    Sha256::digest(encoded).into()
}

fn manual_placement_environment_digest(
    placement_id: Uuid,
    environment_base_digest: &[u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"dayweave-manual-placement-environment/2\0");
    digest.update(environment_base_digest);
    digest.update(placement_id.as_bytes());
    digest.finalize().into()
}

fn manual_block_matches(block: &ScheduleBlock, item_id: ItemId, source: &PreviousBlock) -> bool {
    block.kind == ScheduleBlockKind::Pinned
        && block.item_id == Some(item_id)
        && block.session_index == source.session_index
        && block.start == source.start
        && block.end == source.end
}

#[derive(Debug, Clone)]
struct BusyBlock {
    interval: Interval,
    item_id: Option<ItemId>,
    pinned: bool,
    manual_placement: bool,
}

#[derive(Debug)]
struct Candidate {
    interval: Interval,
    penalty: u64,
    moved_minutes: u32,
    explanations: Vec<PlacementExplanation>,
    violations: Vec<PlanViolation>,
}

#[derive(Debug)]
struct PlanningState {
    blocks: Vec<ScheduleBlock>,
    busy: Vec<BusyBlock>,
    unscheduled: Vec<UnscheduledWork>,
    decisions: Vec<PlanDecision>,
    violations: Vec<PlanViolation>,
    score: PlanScore,
    previous: BTreeMap<(ItemId, u16), PreviousBlock>,
    pinned_minutes: BTreeMap<ItemId, u32>,
    execution_reserved_minutes: BTreeMap<ItemId, u32>,
    session_high_water: BTreeMap<ItemId, u16>,
    live_session_indices: BTreeMap<ItemId, BTreeSet<u16>>,
}

impl PlanningState {
    fn new(request: &PlanRequest, execution: &MaterializedExecutionContext) -> Self {
        let previous = request
            .previous_assignments
            .iter()
            .flat_map(|assignment| {
                assignment
                    .blocks
                    .iter()
                    .map(move |block| ((assignment.item_id, block.session_index), *block))
            })
            .collect();
        let session_high_water = execution
            .work_units
            .iter()
            .filter_map(|(item_id, unit)| unit.high_water().map(|index| (*item_id, index)))
            .collect();
        Self {
            blocks: Vec::new(),
            busy: Vec::new(),
            unscheduled: Vec::new(),
            decisions: Vec::new(),
            violations: Vec::new(),
            score: PlanScore::default(),
            previous,
            pinned_minutes: BTreeMap::new(),
            execution_reserved_minutes: BTreeMap::new(),
            session_high_water,
            live_session_indices: BTreeMap::new(),
        }
    }

    fn add_external_fixed_blocks(&mut self, request: &PlanRequest) {
        let horizon = Interval {
            start: request.horizon_start,
            end: request.horizon_end,
        };
        for fixed in &request.fixed_blocks {
            let interval = Interval {
                start: fixed.start,
                end: fixed.end,
            };
            if interval.clipped(horizon).is_none() {
                continue;
            }
            self.busy.push(BusyBlock {
                interval,
                item_id: None,
                pinned: true,
                manual_placement: false,
            });
            self.blocks.push(ScheduleBlock {
                id: fixed.id,
                is_sensitive: fixed.is_sensitive,
                item_id: None,
                occurrence_id: None,
                external_block_id: Some(fixed.id),
                title: fixed.title.clone(),
                start: fixed.start,
                end: fixed.end,
                session_index: 0,
                kind: ScheduleBlockKind::ExternalFixed,
                explanations: vec![PlacementExplanation {
                    code: ExplanationCode::FixedEvent,
                    message: match fixed.source {
                        FixedBlockSource::Sleep => "Protected sleep is immutable.".to_owned(),
                        _ => "External fixed time is retained.".to_owned(),
                    },
                }],
            });
        }
    }

    fn add_calendar_events(&mut self, request: &PlanRequest) {
        let horizon = Interval {
            start: request.horizon_start,
            end: request.horizon_end,
        };
        for item in &request.items {
            let ItemKind::CalendarEvent(event) = &item.kind else {
                continue;
            };
            let interval = Interval {
                start: event.start,
                end: event.end,
            };
            if interval.clipped(horizon).is_none() {
                continue;
            }
            self.busy.push(BusyBlock {
                interval,
                item_id: Some(item.id),
                pinned: event.immutable,
                manual_placement: false,
            });
            self.blocks.push(ScheduleBlock {
                id: block_id(item.id, 0, event.start),
                is_sensitive: item.is_sensitive,
                item_id: Some(item.id),
                occurrence_id: None,
                external_block_id: None,
                title: item.title.clone(),
                start: event.start,
                end: event.end,
                session_index: 0,
                kind: ScheduleBlockKind::CalendarEvent,
                explanations: vec![PlacementExplanation {
                    code: ExplanationCode::FixedEvent,
                    message: "Calendar time is retained before flexible work is composed."
                        .to_owned(),
                }],
            });
            self.decisions.push(PlanDecision {
                item_id: item.id,
                occurrence_id: None,
                kind: DecisionKind::FixedEventRetained,
                message: "The calendar event remains at its source time.".to_owned(),
            });
        }
    }

    fn add_execution_reservations(
        &mut self,
        request: &PlanRequest,
        items: &BTreeMap<ItemId, &WorkItem>,
        execution: &MaterializedExecutionContext,
    ) -> Result<(), ScheduleError> {
        let horizon = Interval {
            start: request.horizon_start,
            end: request.horizon_end,
        };
        for (item_id, unit) in &execution.work_units {
            let item = items[item_id];
            for reservation in &unit.reservations {
                let interval = Interval {
                    start: reservation.start,
                    end: reservation.end,
                };
                *self.execution_reserved_minutes.entry(*item_id).or_default() = self
                    .execution_reserved_minutes
                    .get(item_id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(ceil_duration_minutes(reservation.end - reservation.start));
                self.live_session_indices
                    .entry(*item_id)
                    .or_default()
                    .insert(reservation.session_index);

                let contained = horizon.contains(interval);
                let disjoint = !horizon.overlaps(interval);
                if disjoint {
                    continue;
                }
                if !contained {
                    return Err(invalid_execution(
                        *item_id,
                        format!(
                            "reservation session {} is only partly covered by the planning horizon",
                            reservation.session_index
                        ),
                    ));
                }
                self.busy.push(BusyBlock {
                    interval,
                    item_id: Some(*item_id),
                    pinned: true,
                    manual_placement: false,
                });
                self.blocks.push(ScheduleBlock {
                    id: block_id(*item_id, reservation.session_index, reservation.start),
                    is_sensitive: item.is_sensitive,
                    item_id: Some(*item_id),
                    occurrence_id: None,
                    external_block_id: None,
                    title: item.title.clone(),
                    start: reservation.start,
                    end: reservation.end,
                    session_index: reservation.session_index,
                    kind: ScheduleBlockKind::Pinned,
                    explanations: vec![PlacementExplanation {
                        code: ExplanationCode::Pinned,
                        message: "Reserved by authoritative execution state.".to_owned(),
                    }],
                });
            }
            if !unit.reservations.is_empty() {
                self.decisions.push(PlanDecision {
                    item_id: *item_id,
                    occurrence_id: None,
                    kind: DecisionKind::KeptPinned,
                    message: "Authoritative execution reservations were preserved exactly."
                        .to_owned(),
                });
            }
        }
        Ok(())
    }

    fn add_pinned_assignments(
        &mut self,
        request: &PlanRequest,
        items: &BTreeMap<ItemId, &WorkItem>,
    ) {
        let horizon = Interval {
            start: request.horizon_start,
            end: request.horizon_end,
        };
        for assignment in &request.previous_assignments {
            if !assignment.pinned {
                continue;
            }
            let item = items[&assignment.item_id];
            if matches!(item.kind, ItemKind::CalendarEvent(_)) {
                continue;
            }
            for block in &assignment.blocks {
                self.session_high_water
                    .entry(item.id)
                    .and_modify(|current| *current = (*current).max(block.session_index))
                    .or_insert(block.session_index);
                let interval = Interval {
                    start: block.start,
                    end: block.end,
                };
                if interval.clipped(horizon).is_none() {
                    continue;
                }
                *self.pinned_minutes.entry(item.id).or_default() = self
                    .pinned_minutes
                    .get(&item.id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(interval.minutes());
                self.busy.push(BusyBlock {
                    interval,
                    item_id: Some(item.id),
                    pinned: true,
                    manual_placement: assignment.manual_placement_id.is_some(),
                });
                self.blocks.push(ScheduleBlock {
                    id: block_id(item.id, block.session_index, block.start),
                    is_sensitive: item.is_sensitive,
                    item_id: Some(item.id),
                    occurrence_id: None,
                    external_block_id: None,
                    title: item.title.clone(),
                    start: block.start,
                    end: block.end,
                    session_index: block.session_index,
                    kind: ScheduleBlockKind::Pinned,
                    explanations: vec![PlacementExplanation {
                        code: ExplanationCode::Pinned,
                        message: "Pinned by the user and excluded from recomposition.".to_owned(),
                    }],
                });
            }
            self.decisions.push(PlanDecision {
                item_id: item.id,
                occurrence_id: None,
                kind: DecisionKind::KeptPinned,
                message: "Pinned sessions were preserved exactly.".to_owned(),
            });
        }
    }

    fn detect_immutable_overlaps(&mut self) -> Result<(), ScheduleError> {
        self.busy.sort_by_key(|busy| {
            (
                busy.interval.start,
                busy.interval.end,
                busy.item_id,
                busy.pinned,
            )
        });
        for left_index in 0..self.busy.len() {
            for right_index in (left_index + 1)..self.busy.len() {
                let left = &self.busy[left_index];
                let right = &self.busy[right_index];
                if right.interval.start >= left.interval.end {
                    break;
                }
                if left.interval.overlaps(right.interval) {
                    if left.manual_placement || right.manual_placement {
                        continue;
                    }
                    if self.violations.len() >= MAX_IMMUTABLE_OVERLAP_VIOLATIONS {
                        return Err(ScheduleError::ConflictEvidenceLimit);
                    }
                    let mut item_ids: Vec<_> = [left.item_id, right.item_id]
                        .into_iter()
                        .flatten()
                        .collect();
                    item_ids.sort_unstable();
                    item_ids.dedup();
                    self.violations.push(PlanViolation {
                        kind: if left.pinned || right.pinned {
                            ViolationKind::PinnedConflict
                        } else {
                            ViolationKind::FixedOverlap
                        },
                        severity: ViolationSeverity::Error,
                        item_ids,
                        occurrence_ids: Vec::new(),
                        start: Some(left.interval.start.max(right.interval.start)),
                        end: Some(left.interval.end.min(right.interval.end)),
                        penalty: 0,
                        message: "Immutable blocks overlap; both remain visible for resolution."
                            .to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn schedule_item(
        &mut self,
        request: &PlanRequest,
        item: &WorkItem,
        dependencies: &[Dependency],
        all_items: &BTreeMap<ItemId, &WorkItem>,
    ) -> bool {
        let Some(duration) = item.duration else {
            self.unscheduled.push(UnscheduledWork {
                item_id: item.id,
                occurrence_id: None,
                remaining: Minutes::ZERO,
                reason: UnscheduledReason::MissingDuration,
                message: "Add or accept a duration estimate before scheduling.".to_owned(),
            });
            return false;
        };

        let required = duration.planning_minutes().get();
        let pinned = self.pinned_minutes.get(&item.id).copied().unwrap_or(0);
        let execution_reserved = self
            .execution_reserved_minutes
            .get(&item.id)
            .copied()
            .unwrap_or(0);
        let mut remaining = required.saturating_sub(pinned.saturating_add(execution_reserved));
        if remaining == 0 {
            self.score.scheduled_minutes = self.score.scheduled_minutes.saturating_add(required);
            return true;
        }

        let existing_blocks: Vec<_> = self
            .blocks
            .iter()
            .filter(|block| block.item_id == Some(item.id))
            .collect();
        let highest_existing = existing_blocks
            .iter()
            .map(|block| block.session_index)
            .max();
        let highest_allocated = self
            .session_high_water
            .get(&item.id)
            .copied()
            .into_iter()
            .chain(highest_existing)
            .max();
        let mut session_index = highest_allocated.map_or(Some(0), |index| index.checked_add(1));
        let mut live_session_indices = self
            .live_session_indices
            .get(&item.id)
            .cloned()
            .unwrap_or_default();
        live_session_indices.extend(existing_blocks.iter().map(|block| block.session_index));
        let existing_session_count = live_session_indices.len();
        let mut sessions_added = 0_usize;
        let mut session_limit_hit = false;
        let mut previous_session_end = existing_blocks.iter().map(|block| block.end).max();
        let mut used_days: BTreeSet<_> = existing_blocks
            .iter()
            .map(|block| block.start.date())
            .collect();

        match &item.split_policy {
            SplitPolicy::Indivisible => {
                if let Some(index) = session_index {
                    if let Some(candidate) = self.best_candidate(
                        request,
                        item,
                        dependencies,
                        all_items,
                        Minutes(remaining),
                        index,
                        None,
                        &used_days,
                        None,
                    ) {
                        self.accept_candidate(item, candidate, index, false);
                        remaining = 0;
                    }
                } else {
                    session_limit_hit = true;
                }
            }
            SplitPolicy::Splittable {
                minimum_session,
                maximum_session,
                maximum_sessions,
                minimum_gap,
                maximum_days,
            } => {
                while remaining > 0 {
                    if existing_session_count.saturating_add(sessions_added)
                        >= usize::from(*maximum_sessions)
                    {
                        session_limit_hit = true;
                        break;
                    }
                    let Some(index) = session_index else {
                        session_limit_hit = true;
                        break;
                    };
                    let mut size = remaining.min(maximum_session.get());
                    let granularity = request.config.slot_granularity.get();
                    let minimum = minimum_session.get().min(remaining);
                    let mut accepted = None;

                    loop {
                        let remainder_after = remaining.saturating_sub(size);
                        if (remainder_after == 0 || remainder_after >= minimum_session.get())
                            && size >= minimum
                        {
                            accepted = self.best_candidate(
                                request,
                                item,
                                dependencies,
                                all_items,
                                Minutes(size),
                                index,
                                previous_session_end.map(|end| {
                                    end + Duration::minutes(i64::from(minimum_gap.get()))
                                }),
                                &used_days,
                                *maximum_days,
                            );
                            if accepted.is_some() {
                                break;
                            }
                        }
                        if size <= minimum {
                            break;
                        }
                        size = size.saturating_sub(granularity).max(minimum);
                    }

                    let Some(candidate) = accepted else {
                        break;
                    };
                    previous_session_end = Some(candidate.interval.end);
                    used_days.insert(candidate.interval.start.date());
                    self.accept_candidate(item, candidate, index, true);
                    remaining = remaining.saturating_sub(size);
                    session_index = index.checked_add(1);
                    sessions_added = sessions_added.saturating_add(1);
                }
            }
        }

        let scheduled_now = required.saturating_sub(remaining);
        self.score.scheduled_minutes = self.score.scheduled_minutes.saturating_add(scheduled_now);
        if remaining == 0 {
            self.decisions.push(PlanDecision {
                item_id: item.id,
                occurrence_id: None,
                kind: DecisionKind::Scheduled,
                message: format!("Reserved {required} minutes."),
            });
            true
        } else {
            let reason = if session_limit_hit {
                UnscheduledReason::SessionLimit
            } else {
                UnscheduledReason::NoCapacity
            };
            self.score.unscheduled_minutes =
                self.score.unscheduled_minutes.saturating_add(remaining);
            self.unscheduled.push(UnscheduledWork {
                item_id: item.id,
                occurrence_id: None,
                remaining: Minutes(remaining),
                reason,
                message: if session_limit_hit {
                    format!(
                        "The session limit or semantic index space leaves {remaining} minutes unscheduled."
                    )
                } else {
                    format!(
                        "No valid capacity for the remaining {remaining} minutes inside the horizon."
                    )
                },
            });
            self.violations.push(PlanViolation {
                kind: ViolationKind::Capacity,
                severity: ViolationSeverity::Error,
                item_ids: vec![item.id],
                occurrence_ids: Vec::new(),
                start: None,
                end: None,
                penalty: 0,
                message: format!("{remaining} minutes remain unscheduled."),
            });
            if scheduled_now > 0 {
                self.decisions.push(PlanDecision {
                    item_id: item.id,
                    occurrence_id: None,
                    kind: DecisionKind::PartiallyScheduled,
                    message: format!(
                        "Reserved {scheduled_now} of {required} minutes; overload remains visible."
                    ),
                });
            }
            false
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn best_candidate(
        &self,
        request: &PlanRequest,
        item: &WorkItem,
        dependencies: &[Dependency],
        all_items: &BTreeMap<ItemId, &WorkItem>,
        duration: Minutes,
        session_index: u16,
        session_earliest: Option<OffsetDateTime>,
        used_days: &BTreeSet<time::Date>,
        maximum_days: Option<u16>,
    ) -> Option<Candidate> {
        let mut best: Option<Candidate> = None;
        let planning_horizon = Interval {
            start: request.horizon_start.max(request.as_of),
            end: request.horizon_end,
        };
        let duration_delta = Duration::minutes(i64::from(duration.get()));

        for availability in &request.availability {
            let availability_interval = Interval {
                start: availability.start,
                end: availability.end,
            };
            let Some(available) = availability_interval.clipped(planning_horizon) else {
                continue;
            };
            for free in free_segments(available, &self.busy) {
                let mut start = align_up(free.start, request.config.slot_granularity);
                if let Some(earliest) = session_earliest {
                    start = align_up(start.max(earliest), request.config.slot_granularity);
                }
                while start + duration_delta <= free.end {
                    let interval = Interval {
                        start,
                        end: start + duration_delta,
                    };
                    if maximum_days.is_some_and(|limit| {
                        !used_days.contains(&interval.start.date())
                            && used_days.len() >= usize::from(limit)
                    }) {
                        start +=
                            Duration::minutes(i64::from(request.config.slot_granularity.get()));
                        continue;
                    }
                    if let Some(candidate) = self.evaluate_candidate(
                        request,
                        item,
                        dependencies,
                        all_items,
                        availability,
                        interval,
                        session_index,
                        used_days,
                    ) {
                        let replace = best.as_ref().is_none_or(|current| {
                            (
                                candidate.penalty,
                                candidate.interval.start,
                                candidate.interval.end,
                            ) < (
                                current.penalty,
                                current.interval.start,
                                current.interval.end,
                            )
                        });
                        if replace {
                            best = Some(candidate);
                        }
                    }
                    start += Duration::minutes(i64::from(request.config.slot_granularity.get()));
                }
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn evaluate_candidate(
        &self,
        request: &PlanRequest,
        item: &WorkItem,
        dependencies: &[Dependency],
        all_items: &BTreeMap<ItemId, &WorkItem>,
        availability: &AvailabilityWindow,
        interval: Interval,
        session_index: u16,
        used_days: &BTreeSet<time::Date>,
    ) -> Option<Candidate> {
        let mut penalty = 0_u64;
        let mut violations = Vec::new();
        let mut explanations = Vec::new();
        let constraints = &item.constraints;

        if let Some(window) = constraints.occurrence_window {
            let recurrence_window = Interval {
                start: window.start,
                end: window.end,
            };
            if !recurrence_window.contains(interval) {
                return None;
            }
        }

        let mut test = |satisfied: bool,
                        strength: ConstraintStrength,
                        kind: ViolationKind,
                        magnitude: u32,
                        message: String|
         -> bool {
            if satisfied {
                return true;
            }
            if strength.is_hard() {
                return false;
            }
            let item_penalty = u64::from(soft_weight(request, strength))
                .saturating_mul(u64::from(magnitude.max(1)));
            penalty = penalty.saturating_add(item_penalty);
            violations.push(PlanViolation {
                kind,
                severity: ViolationSeverity::Warning,
                item_ids: vec![item.id],
                occurrence_ids: Vec::new(),
                start: Some(interval.start),
                end: Some(interval.end),
                penalty: item_penalty,
                message,
            });
            true
        };

        if let Some(boundary) = &constraints.earliest_start {
            let early = positive_minutes(boundary.value - interval.start);
            if !test(
                interval.start >= boundary.value,
                boundary.strength,
                ViolationKind::SoftConstraint,
                early,
                "Scheduled before the preferred earliest start.".to_owned(),
            ) {
                return None;
            }
        }
        if let Some(boundary) = &constraints.latest_finish {
            let late = positive_minutes(interval.end - boundary.value);
            if !test(
                interval.end <= boundary.value,
                boundary.strength,
                ViolationKind::DeadlineRisk,
                late,
                "Scheduled after the preferred deadline.".to_owned(),
            ) {
                return None;
            }
            if boundary.strength.is_hard() {
                explanations.push(explanation(
                    ExplanationCode::HardDeadline,
                    "Placed within its hard deadline.",
                ));
            }
        }
        if let Some(notice) = &constraints.minimum_notice {
            let required_start = request.as_of + Duration::minutes(i64::from(notice.value.get()));
            let shortfall = positive_minutes(required_start - interval.start);
            if !test(
                interval.start >= required_start,
                notice.strength,
                ViolationKind::SoftConstraint,
                shortfall,
                "Minimum notice was compressed.".to_owned(),
            ) {
                return None;
            }
        }

        let weekday = DayOfWeek::from_time(interval.start.weekday());
        if let Some(allowed) = &constraints.allowed_weekdays
            && !test(
                allowed.value.contains(&weekday),
                allowed.strength,
                ViolationKind::SoftConstraint,
                1,
                "Placed on a non-preferred weekday.".to_owned(),
            )
        {
            return None;
        }

        for forbidden in &constraints.forbidden_windows {
            let overlap = overlap_minutes(
                interval,
                Interval {
                    start: forbidden.value.start,
                    end: forbidden.value.end,
                },
            );
            if !test(
                overlap == 0,
                forbidden.strength,
                ViolationKind::SoftConstraint,
                overlap,
                "Placed partly inside a forbidden window.".to_owned(),
            ) {
                return None;
            }
        }

        for required in &constraints.required_contexts {
            let matched = availability.contexts.contains(&required.value);
            if !test(
                matched,
                required.strength,
                ViolationKind::SoftConstraint,
                interval.minutes(),
                format!("Required context '{}' is unavailable.", required.value),
            ) {
                return None;
            }
            if matched {
                explanations.push(explanation(
                    ExplanationCode::ContextMatch,
                    format!("Matches the '{}' context.", required.value),
                ));
            }
        }
        if let Some(required) = &constraints.required_location
            && !test(
                availability.location.as_ref() == Some(&required.value),
                required.strength,
                ViolationKind::SoftConstraint,
                interval.minutes(),
                format!("Required location '{}' is unavailable.", required.value),
            )
        {
            return None;
        }
        if let Some(required) = &item.energy {
            let matched = availability.energy.satisfies(required.value);
            if !test(
                matched,
                required.strength,
                ViolationKind::SoftConstraint,
                interval.minutes(),
                "Available energy is below this work's requirement.".to_owned(),
            ) {
                return None;
            }
            if matched {
                explanations.push(explanation(
                    ExplanationCode::EnergyMatch,
                    "Matches the available energy level.",
                ));
            }
        }

        // Window groups use OR semantics (for example, "morning or evening"),
        // so they are evaluated together after the scalar restrictions above.
        if !evaluate_preferred_windows(
            request,
            item.id,
            constraints,
            interval,
            &mut penalty,
            &mut violations,
            &mut explanations,
        ) {
            return None;
        }

        if !self.evaluate_dependencies(
            request,
            item,
            dependencies,
            all_items,
            interval,
            &mut penalty,
            &mut violations,
            &mut explanations,
        ) {
            return None;
        }

        if !self.evaluate_limits(
            request,
            item,
            constraints,
            interval,
            used_days,
            &mut penalty,
            &mut violations,
        ) {
            return None;
        }

        if !self.evaluate_buffers(
            request,
            item,
            constraints,
            availability,
            interval,
            &mut penalty,
            &mut violations,
        ) {
            return None;
        }

        let previous = self.previous.get(&(item.id, session_index));
        let moved_minutes = previous.map_or(0, |old| {
            positive_minutes((interval.start - old.start).abs())
        });
        if let Some(old) = previous {
            penalty = penalty.saturating_add(
                u64::from(moved_minutes).saturating_mul(u64::from(request.config.stability_weight)),
            );
            if old.start == interval.start {
                explanations.push(explanation(
                    ExplanationCode::StableTime,
                    "Preserves the previous schedule time.",
                ));
            }
        }

        if !item.goal_ids.is_empty() {
            explanations.push(explanation(
                ExplanationCode::GoalProgress,
                "Advances linked goal work.",
            ));
        }
        if matches!(
            item.kind,
            ItemKind::Habit(_) | ItemKind::Routine(_) | ItemKind::RecurringTask(_)
        ) {
            explanations.push(explanation(
                ExplanationCode::HabitOrRoutine,
                "Maintains a habit or routine cadence.",
            ));
        }
        if item.priority.score() > 0 {
            explanations.push(explanation(
                ExplanationCode::Priority,
                format!("Priority score is {}.", item.priority.score()),
            ));
        }
        if previous.is_none() {
            explanations.push(explanation(
                ExplanationCode::EarliestAvailable,
                "Uses the earliest best-scoring valid capacity.",
            ));
        }

        explanations.sort_by_key(|value| value.code);
        explanations
            .dedup_by(|left, right| left.code == right.code && left.message == right.message);
        Some(Candidate {
            interval,
            penalty,
            moved_minutes,
            explanations,
            violations,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_dependencies(
        &self,
        request: &PlanRequest,
        item: &WorkItem,
        dependencies: &[Dependency],
        all_items: &BTreeMap<ItemId, &WorkItem>,
        interval: Interval,
        penalty: &mut u64,
        violations: &mut Vec<PlanViolation>,
        explanations: &mut Vec<PlacementExplanation>,
    ) -> bool {
        for dependency in dependencies {
            let predecessor = all_items.get(&dependency.item_id).copied();
            if predecessor.is_some_and(|value| value.status.is_terminal()) {
                continue;
            }
            let mut blocks = self
                .blocks
                .iter()
                .filter(|block| block.item_id == Some(dependency.item_id));
            let first = blocks.next();
            let Some(first) = first else {
                if dependency.strength.is_hard() {
                    return false;
                }
                add_penalty_violation(
                    request,
                    item.id,
                    interval,
                    dependency.strength,
                    ViolationKind::Dependency,
                    1,
                    "A preferred predecessor is not in this plan.",
                    penalty,
                    violations,
                );
                continue;
            };
            let mut predecessor_start = first.start;
            let mut predecessor_end = first.end;
            for block in blocks {
                predecessor_start = predecessor_start.min(block.start);
                predecessor_end = predecessor_end.max(block.end);
            }
            let lag = Duration::minutes(i64::from(dependency.minimum_lag.get()));
            let (satisfied, shortfall) = match dependency.relation {
                DependencyRelation::FinishToStart => (
                    interval.start >= predecessor_end + lag,
                    positive_minutes(predecessor_end + lag - interval.start),
                ),
                DependencyRelation::StartToStart => (
                    interval.start >= predecessor_start + lag,
                    positive_minutes(predecessor_start + lag - interval.start),
                ),
                DependencyRelation::FinishToFinish => (
                    interval.end >= predecessor_end + lag,
                    positive_minutes(predecessor_end + lag - interval.end),
                ),
                DependencyRelation::StartToFinish => (
                    interval.end >= predecessor_start + lag,
                    positive_minutes(predecessor_start + lag - interval.end),
                ),
            };
            if !satisfied && dependency.strength.is_hard() {
                return false;
            }
            if satisfied {
                explanations.push(explanation(
                    ExplanationCode::Dependency,
                    "Follows its predecessor dependency.",
                ));
            } else {
                add_penalty_violation(
                    request,
                    item.id,
                    interval,
                    dependency.strength,
                    ViolationKind::Dependency,
                    shortfall,
                    "A soft dependency order was compressed.",
                    penalty,
                    violations,
                );
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_limits(
        &self,
        request: &PlanRequest,
        item: &WorkItem,
        constraints: &SchedulingConstraints,
        interval: Interval,
        _used_days: &BTreeSet<time::Date>,
        penalty: &mut u64,
        violations: &mut Vec<PlanViolation>,
    ) -> bool {
        let item_blocks = self
            .blocks
            .iter()
            .filter(|block| block.item_id == Some(item.id));
        let existing: Vec<_> = item_blocks.collect();
        if let Some(limit) = &constraints.maximum_daily_work {
            let already = existing
                .iter()
                .filter(|block| block.start.date() == interval.start.date())
                .map(|block| {
                    u32::try_from((block.end - block.start).whole_minutes()).unwrap_or(u32::MAX)
                })
                .sum::<u32>();
            let total = already.saturating_add(interval.minutes());
            if total > limit.value.get() {
                if limit.strength.is_hard() {
                    return false;
                }
                add_penalty_violation(
                    request,
                    item.id,
                    interval,
                    limit.strength,
                    ViolationKind::SoftConstraint,
                    total - limit.value.get(),
                    "Daily work limit was exceeded.",
                    penalty,
                    violations,
                );
            }
        }
        if let Some(limit) = &constraints.maximum_weekly_work {
            let week_start = monday_of(interval.start);
            let already = existing
                .iter()
                .filter(|block| monday_of(block.start) == week_start)
                .map(|block| {
                    u32::try_from((block.end - block.start).whole_minutes()).unwrap_or(u32::MAX)
                })
                .sum::<u32>();
            let total = already.saturating_add(interval.minutes());
            if total > limit.value.get() {
                if limit.strength.is_hard() {
                    return false;
                }
                add_penalty_violation(
                    request,
                    item.id,
                    interval,
                    limit.strength,
                    ViolationKind::SoftConstraint,
                    total - limit.value.get(),
                    "Weekly work limit was exceeded.",
                    penalty,
                    violations,
                );
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_buffers(
        &self,
        request: &PlanRequest,
        item: &WorkItem,
        constraints: &SchedulingConstraints,
        availability: &AvailabilityWindow,
        interval: Interval,
        penalty: &mut u64,
        violations: &mut Vec<PlanViolation>,
    ) -> bool {
        let Some(strength) = constraints.buffers.strength else {
            return true;
        };
        let expanded = Interval {
            start: interval.start - Duration::minutes(i64::from(constraints.buffers.before.get())),
            end: interval.end + Duration::minutes(i64::from(constraints.buffers.after.get())),
        };
        let availability_interval = Interval {
            start: availability.start,
            end: availability.end,
        };
        let overlap = self
            .busy
            .iter()
            .map(|busy| overlap_minutes(expanded, busy.interval))
            .sum::<u32>();
        let outside = if availability_interval.contains(expanded) {
            0
        } else {
            expanded
                .minutes()
                .saturating_sub(overlap_minutes(expanded, availability_interval))
        };
        let compressed = overlap.saturating_add(outside);
        if compressed == 0 {
            return true;
        }
        if strength.is_hard() {
            return false;
        }
        add_penalty_violation(
            request,
            item.id,
            interval,
            strength,
            ViolationKind::BufferCompressed,
            compressed,
            "Preparation or decompression buffer was compressed.",
            penalty,
            violations,
        );
        true
    }

    fn accept_candidate(
        &mut self,
        item: &WorkItem,
        mut candidate: Candidate,
        session_index: u16,
        split: bool,
    ) {
        if split {
            candidate.explanations.push(explanation(
                ExplanationCode::SplitSession,
                format!("Session {} of split work.", u32::from(session_index) + 1),
            ));
        }
        self.score.soft_penalty = self.score.soft_penalty.saturating_add(candidate.penalty);
        self.score.moved_minutes = self
            .score
            .moved_minutes
            .saturating_add(candidate.moved_minutes);
        self.violations.append(&mut candidate.violations);
        self.busy.push(BusyBlock {
            interval: candidate.interval,
            item_id: Some(item.id),
            pinned: false,
            manual_placement: false,
        });
        self.blocks.push(ScheduleBlock {
            id: block_id(item.id, session_index, candidate.interval.start),
            is_sensitive: item.is_sensitive,
            item_id: Some(item.id),
            occurrence_id: None,
            external_block_id: None,
            title: item.title.clone(),
            start: candidate.interval.start,
            end: candidate.interval.end,
            session_index,
            kind: ScheduleBlockKind::Planned,
            explanations: candidate.explanations,
        });
    }

    fn finish(mut self, request: &PlanRequest) -> SchedulePlan {
        self.blocks.sort_by_key(|block| {
            (
                block.start,
                block.end,
                block.item_id,
                block.external_block_id,
                block.session_index,
            )
        });
        self.unscheduled.sort_by_key(|work| work.item_id);
        self.decisions
            .sort_by_key(|decision| (decision.item_id, decision.kind as u8));
        self.violations.sort_by_key(|violation| {
            (
                violation.start,
                violation.end,
                violation.item_ids.clone(),
                violation.kind as u8,
            )
        });
        self.score.unscheduled_minutes = self.unscheduled.iter().fold(0_u32, |total, work| {
            total.saturating_add(work.remaining.get())
        });
        SchedulePlan {
            as_of: request.as_of,
            horizon_start: request.horizon_start,
            horizon_end: request.horizon_end,
            blocks: self.blocks,
            unscheduled: self.unscheduled,
            decisions: self.decisions,
            violations: self.violations,
            score: self.score,
            occurrences: Vec::new(),
            manual_placement_assessments: Vec::new(),
        }
    }
}

fn validate_request(request: &PlanRequest) -> Result<(), ScheduleError> {
    if request.horizon_start >= request.horizon_end {
        return Err(ScheduleError::InvalidHorizon);
    }
    if request.config.slot_granularity.is_zero() {
        return Err(ScheduleError::InvalidGranularity);
    }

    let mut ids = BTreeSet::new();
    for item in &request.items {
        if !ids.insert(item.id) {
            return Err(ScheduleError::DuplicateItem(item.id));
        }
        item.priority
            .validate()
            .map_err(|message| invalid_item(item.id, message))?;
        item.split_policy
            .validate()
            .map_err(|message| invalid_item(item.id, message))?;
        if let Some(duration) = item.duration {
            duration
                .validate()
                .map_err(|message| invalid_item(item.id, message))?;
        }
        if item.title.trim().is_empty() {
            return Err(invalid_item(item.id, "title cannot be empty"));
        }
        if let ItemKind::CalendarEvent(event) = &item.kind
            && event.start >= event.end
        {
            return Err(invalid_item(
                item.id,
                "calendar event end must follow start",
            ));
        }
        validate_constraints(item)?;
    }
    roll_up_expected_durations(&request.items)
        .map_err(|error| ScheduleError::InvalidHierarchy(error.to_string()))?;

    for (index, availability) in request.availability.iter().enumerate() {
        if availability.start >= availability.end {
            return Err(ScheduleError::InvalidWindow {
                owner: format!("availability {index}"),
            });
        }
    }
    for fixed in &request.fixed_blocks {
        if fixed.start >= fixed.end {
            return Err(ScheduleError::InvalidWindow {
                owner: format!("fixed block {}", fixed.id),
            });
        }
    }
    validate_previous_assignments(request, &ids)?;
    Ok(())
}

fn validate_previous_assignments(
    request: &PlanRequest,
    item_ids: &BTreeSet<ItemId>,
) -> Result<(), ScheduleError> {
    let by_id: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
    let children = child_map(&request.items);
    let mut assignment_identities = BTreeSet::new();
    for assignment in &request.previous_assignments {
        if !item_ids.contains(&assignment.item_id) {
            return Err(ScheduleError::MissingPreviousItem(assignment.item_id));
        }
        if !assignment_identities.insert((assignment.item_id, assignment.occurrence_id)) {
            return Err(invalid_item(
                assignment.item_id,
                "previous assignment identity is duplicated",
            ));
        }
        if let Some(placement_id) = assignment.manual_placement_id {
            let item = by_id[&assignment.item_id];
            let has_children = children
                .get(&item.id)
                .is_some_and(|children| !children.is_empty());
            if placement_id.is_nil()
                || !assignment.pinned
                || assignment.blocks.is_empty()
                || matches!(item.kind, ItemKind::CalendarEvent(_))
                || item.status.is_terminal()
                || !item.occupies_time(has_children)
            {
                return Err(invalid_item(
                    assignment.item_id,
                    "manual placement must pin non-empty executable work",
                ));
            }
        }
        let mut session_indices = BTreeSet::new();
        for block in &assignment.blocks {
            if block.start >= block.end {
                return Err(ScheduleError::InvalidWindow {
                    owner: format!("previous assignment for {}", assignment.item_id),
                });
            }
            if !session_indices.insert(block.session_index) {
                return Err(invalid_item(
                    assignment.item_id,
                    "previous assignment repeats a session index",
                ));
            }
            if assignment.manual_placement_id.is_some()
                && (block.start < request.as_of
                    || block.start < request.horizon_start
                    || block.end > request.horizon_end
                    || by_id[&assignment.item_id]
                        .constraints
                        .occurrence_window
                        .is_some_and(|window| block.start < window.start || block.end > window.end))
            {
                return Err(invalid_item(
                    assignment.item_id,
                    "manual placement is outside its planning or occurrence window",
                ));
            }
        }
    }
    Ok(())
}

fn validate_manual_placement_demand(
    request: &PlanRequest,
    execution: &MaterializedExecutionContext,
) -> Result<(), ScheduleError> {
    let items: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
    for assignment in request
        .previous_assignments
        .iter()
        .filter(|assignment| assignment.manual_placement_id.is_some())
    {
        let item = items[&assignment.item_id];
        validate_manual_assignment_demand(request, execution, item, assignment)?;
    }
    Ok(())
}

fn validate_manual_assignment_demand(
    request: &PlanRequest,
    execution: &MaterializedExecutionContext,
    item: &WorkItem,
    assignment: &PreviousAssignment,
) -> Result<(), ScheduleError> {
    let expected = item.duration.ok_or_else(|| {
        invalid_item(
            item.id,
            "manual placement requires an authoritative duration",
        )
    })?;
    if execution
        .work_units
        .get(&item.id)
        .is_some_and(|unit| !unit.reservations.is_empty())
    {
        return Err(invalid_item(
            item.id,
            "manual placement cannot replace an active execution reservation",
        ));
    }
    let (chronological, total_minutes) =
        validate_manual_block_sequence(request, execution, item, assignment)?;
    if total_minutes != expected.planning_minutes().get() {
        return Err(invalid_item(
            item.id,
            "manual placement must bind the exact remaining work duration",
        ));
    }
    validate_manual_split_policy(item, &chronological, total_minutes)
}

fn validate_manual_block_sequence(
    request: &PlanRequest,
    execution: &MaterializedExecutionContext,
    item: &WorkItem,
    assignment: &PreviousAssignment,
) -> Result<(Vec<PreviousBlock>, u32), ScheduleError> {
    let mut total_minutes = 0_u32;
    let mut chronological = assignment.blocks.clone();
    chronological.sort_by_key(|block| (block.start, block.end, block.session_index));
    for block in &chronological {
        let seconds = (block.end - block.start).whole_seconds();
        let granularity_seconds = i64::from(request.config.slot_granularity.get()) * 60;
        if seconds <= 0
            || seconds % 60 != 0
            || block.start.nanosecond() != 0
            || block.end.nanosecond() != 0
            || block.start.unix_timestamp().rem_euclid(granularity_seconds) != 0
            || block.end.unix_timestamp().rem_euclid(granularity_seconds) != 0
        {
            return Err(invalid_item(
                item.id,
                "manual placement blocks must use whole-minute scheduler slots",
            ));
        }
        let minutes = u32::try_from(seconds / 60).map_err(|_| {
            invalid_item(
                item.id,
                "manual placement duration exceeds supported bounds",
            )
        })?;
        total_minutes = total_minutes.checked_add(minutes).ok_or_else(|| {
            invalid_item(
                item.id,
                "manual placement duration exceeds supported bounds",
            )
        })?;
    }
    let expected_first_index = execution
        .work_units
        .get(&item.id)
        .and_then(MaterializedExecutionWorkUnit::high_water)
        .map_or(Some(0), |index| index.checked_add(1))
        .ok_or_else(|| {
            invalid_item(item.id, "manual placement session index space is exhausted")
        })?;
    if chronological.first().map(|block| block.session_index) != Some(expected_first_index)
        || chronological
            .windows(2)
            .any(|pair| pair[0].session_index.checked_add(1) != Some(pair[1].session_index))
    {
        return Err(invalid_item(
            item.id,
            "manual placement session indices must be fresh, consecutive, and chronological",
        ));
    }
    Ok((chronological, total_minutes))
}

fn validate_manual_split_policy(
    item: &WorkItem,
    chronological: &[PreviousBlock],
    total_minutes: u32,
) -> Result<(), ScheduleError> {
    let SplitPolicy::Splittable {
        minimum_session,
        maximum_session,
        maximum_sessions,
        minimum_gap,
        maximum_days,
    } = item.split_policy
    else {
        if chronological.len() != 1 {
            return Err(invalid_item(
                item.id,
                "an indivisible manual placement must contain exactly one block",
            ));
        }
        return Ok(());
    };
    if chronological.len() > usize::from(maximum_sessions) {
        return Err(invalid_item(
            item.id,
            "manual placement exceeds the configured session limit",
        ));
    }
    let mut days = BTreeSet::new();
    for (index, block) in chronological.iter().enumerate() {
        let minutes = u32::try_from((block.end - block.start).whole_minutes()).map_err(|_| {
            invalid_item(
                item.id,
                "manual placement duration exceeds supported bounds",
            )
        })?;
        let only_subminimum_remainder = chronological.len() == 1
            && total_minutes < minimum_session.get()
            && minutes == total_minutes;
        if (!only_subminimum_remainder && minutes < minimum_session.get())
            || minutes > maximum_session.get()
        {
            return Err(invalid_item(
                item.id,
                "manual placement block violates configured session bounds",
            ));
        }
        days.insert(block.start.date());
        if let Some(previous) = index.checked_sub(1).map(|value| chronological[value])
            && block.start < previous.end + Duration::minutes(i64::from(minimum_gap.get()))
        {
            return Err(invalid_item(
                item.id,
                "manual placement blocks violate the configured minimum gap",
            ));
        }
    }
    if maximum_days.is_some_and(|limit| days.len() > usize::from(limit)) {
        return Err(invalid_item(
            item.id,
            "manual placement exceeds the configured day limit",
        ));
    }
    Ok(())
}

fn validate_constraints(item: &WorkItem) -> Result<(), ScheduleError> {
    if item
        .constraints
        .occurrence_window
        .is_some_and(|window| window.start >= window.end)
    {
        return Err(invalid_item(
            item.id,
            "recurrence occurrence window is empty",
        ));
    }
    for window in &item.constraints.preferred_absolute_windows {
        if window.value.start >= window.value.end {
            return Err(invalid_item(item.id, "preferred window is empty"));
        }
    }
    for window in &item.constraints.forbidden_windows {
        if window.value.start >= window.value.end {
            return Err(invalid_item(item.id, "forbidden window is empty"));
        }
    }
    for window in &item.constraints.preferred_daily_windows {
        if window.value.start_minute >= 1_440
            || window.value.end_minute > 1_440
            || window.value.start_minute == window.value.end_minute
        {
            return Err(invalid_item(
                item.id,
                "daily window minutes must describe a non-empty day interval",
            ));
        }
    }
    if item.constraints.buffers.strength.is_some()
        && item.constraints.buffers.before.is_zero()
        && item.constraints.buffers.after.is_zero()
    {
        return Err(invalid_item(
            item.id,
            "buffer strength requires a non-zero before or after buffer",
        ));
    }
    Ok(())
}

fn invalid_item(item_id: ItemId, message: impl Into<String>) -> ScheduleError {
    ScheduleError::InvalidItem {
        item_id,
        message: message.into(),
    }
}

fn child_map(items: &[WorkItem]) -> BTreeMap<ItemId, Vec<ItemId>> {
    let mut result: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
    for item in items {
        if let Some(parent) = item.parent_id {
            result.entry(parent).or_default().push(item.id);
        }
    }
    for children in result.values_mut() {
        children.sort_unstable();
    }
    result
}

fn dependencies_with_routine_order(
    items: &[WorkItem],
    children: &BTreeMap<ItemId, Vec<ItemId>>,
) -> BTreeMap<ItemId, Vec<Dependency>> {
    let by_id: BTreeMap<_, _> = items.iter().map(|item| (item.id, item)).collect();
    let mut result: BTreeMap<_, _> = items
        .iter()
        .map(|item| (item.id, item.constraints.dependencies.clone()))
        .collect();
    for parent in items {
        let ItemKind::Routine(spec) = &parent.kind else {
            continue;
        };
        if !spec.ordered {
            continue;
        }
        let mut ordered = children.get(&parent.id).cloned().unwrap_or_default();
        ordered.sort_by_key(|id| (by_id[id].sibling_order.unwrap_or(u32::MAX), *id));
        for pair in ordered.windows(2) {
            result.entry(pair[1]).or_default().push(Dependency {
                item_id: pair[0],
                relation: DependencyRelation::FinishToStart,
                minimum_lag: Minutes::ZERO,
                strength: ConstraintStrength::Hard,
            });
        }
    }
    for dependencies in result.values_mut() {
        dependencies.sort_by_key(|dependency| {
            (
                dependency.item_id,
                dependency.relation as u8,
                dependency.minimum_lag,
            )
        });
        dependencies.dedup_by(|left, right| {
            left.item_id == right.item_id
                && left.relation == right.relation
                && left.minimum_lag == right.minimum_lag
                && left.strength == right.strength
        });
    }
    result
}

fn dependency_order(
    eligible: Vec<ItemId>,
    items: &BTreeMap<ItemId, &WorkItem>,
    dependencies: &BTreeMap<ItemId, Vec<Dependency>>,
) -> (Vec<ItemId>, Vec<ItemId>) {
    let eligible_set: BTreeSet<_> = eligible.iter().copied().collect();
    let mut indegree: BTreeMap<_, _> = eligible.iter().map(|id| (*id, 0_u32)).collect();
    let mut successors: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
    for id in &eligible {
        for dependency in dependencies.get(id).map_or(&[][..], Vec::as_slice) {
            if dependency.strength.is_hard() && eligible_set.contains(&dependency.item_id) {
                *indegree.entry(*id).or_default() += 1;
                successors.entry(dependency.item_id).or_default().push(*id);
            }
        }
    }

    let mut ready: Vec<_> = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect();
    let mut ordered = Vec::with_capacity(eligible.len());
    while !ready.is_empty() {
        ready.sort_by(|left, right| schedule_order(items[left], items[right]));
        let id = ready.remove(0);
        ordered.push(id);
        for successor in successors.get(&id).map_or(&[][..], Vec::as_slice) {
            let degree = indegree
                .get_mut(successor)
                .expect("successors are eligible by construction");
            *degree -= 1;
            if *degree == 0 {
                ready.push(*successor);
            }
        }
    }
    let ordered_set: BTreeSet<_> = ordered.iter().copied().collect();
    let cyclic = eligible
        .into_iter()
        .filter(|id| !ordered_set.contains(id))
        .collect();
    (ordered, cyclic)
}

fn schedule_order(left: &WorkItem, right: &WorkItem) -> Ordering {
    let left_deadline = left
        .constraints
        .latest_finish
        .as_ref()
        .map(|value| (u8::from(!value.strength.is_hard()), value.value));
    let right_deadline = right
        .constraints
        .latest_finish
        .as_ref()
        .map(|value| (u8::from(!value.strength.is_hard()), value.value));
    let deadline_order = match (left_deadline, right_deadline) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    deadline_order
        .then_with(|| kind_rank(left).cmp(&kind_rank(right)))
        .then_with(|| right.priority.score().cmp(&left.priority.score()))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn kind_rank(item: &WorkItem) -> u8 {
    if !item.goal_ids.is_empty() {
        return 0;
    }
    match item.kind {
        ItemKind::Habit(_) | ItemKind::Routine(_) | ItemKind::RecurringTask(_) => 1,
        ItemKind::Break(_) => 2,
        ItemKind::Task | ItemKind::Goal(_) => 3,
        ItemKind::CalendarEvent(_) => 4,
    }
}

fn hard_dependency_unavailable(
    dependencies: &[Dependency],
    items: &BTreeMap<ItemId, &WorkItem>,
    outcomes: &BTreeMap<ItemId, bool>,
) -> bool {
    dependencies.iter().any(|dependency| {
        if !dependency.strength.is_hard() {
            return false;
        }
        match items.get(&dependency.item_id) {
            Some(item) if item.status.is_terminal() => false,
            Some(_) => outcomes
                .get(&dependency.item_id)
                .is_some_and(|success| !success),
            None => true,
        }
    })
}

fn free_segments(available: Interval, busy: &[BusyBlock]) -> Vec<Interval> {
    let mut intersections: Vec<_> = busy
        .iter()
        .filter_map(|block| block.interval.clipped(available))
        .collect();
    intersections.sort_by_key(|value| (value.start, value.end));
    let mut merged: Vec<Interval> = Vec::new();
    for interval in intersections {
        if let Some(last) = merged.last_mut()
            && interval.start <= last.end
        {
            last.end = last.end.max(interval.end);
            continue;
        }
        merged.push(interval);
    }
    let mut free = Vec::new();
    let mut cursor = available.start;
    for occupied in merged {
        if cursor < occupied.start {
            free.push(Interval {
                start: cursor,
                end: occupied.start,
            });
        }
        cursor = cursor.max(occupied.end);
    }
    if cursor < available.end {
        free.push(Interval {
            start: cursor,
            end: available.end,
        });
    }
    free
}

#[allow(clippy::too_many_arguments)]
fn evaluate_preferred_windows(
    request: &PlanRequest,
    item_id: ItemId,
    constraints: &SchedulingConstraints,
    interval: Interval,
    penalty: &mut u64,
    violations: &mut Vec<PlanViolation>,
    explanations: &mut Vec<PlacementExplanation>,
) -> bool {
    let hard_daily: Vec<_> = constraints
        .preferred_daily_windows
        .iter()
        .filter(|window| window.strength.is_hard())
        .collect();
    if !hard_daily.is_empty()
        && !hard_daily
            .iter()
            .any(|window| window.value.contains(interval.start, interval.end))
    {
        return false;
    }
    let hard_absolute: Vec<_> = constraints
        .preferred_absolute_windows
        .iter()
        .filter(|window| window.strength.is_hard())
        .collect();
    if !hard_absolute.is_empty()
        && !hard_absolute.iter().any(|window| {
            Interval {
                start: window.value.start,
                end: window.value.end,
            }
            .contains(interval)
        })
    {
        return false;
    }

    let daily_match = constraints
        .preferred_daily_windows
        .iter()
        .any(|window| window.value.contains(interval.start, interval.end));
    let absolute_match = constraints.preferred_absolute_windows.iter().any(|window| {
        Interval {
            start: window.value.start,
            end: window.value.end,
        }
        .contains(interval)
    });
    if daily_match || absolute_match {
        explanations.push(explanation(
            ExplanationCode::PreferredWindow,
            "Matches a preferred work window.",
        ));
    }

    let soft_daily: Vec<_> = constraints
        .preferred_daily_windows
        .iter()
        .filter(|window| !window.strength.is_hard())
        .collect();
    if !soft_daily.is_empty() && !daily_match {
        let strength = soft_daily
            .iter()
            .min_by_key(|window| window.strength.weight())
            .map_or(ConstraintStrength::DEFAULT_SOFT, |window| window.strength);
        add_penalty_violation(
            request,
            item_id,
            interval,
            strength,
            ViolationKind::SoftConstraint,
            interval.minutes(),
            "Placed outside preferred daily windows.",
            penalty,
            violations,
        );
    }
    let soft_absolute: Vec<_> = constraints
        .preferred_absolute_windows
        .iter()
        .filter(|window| !window.strength.is_hard())
        .collect();
    if !soft_absolute.is_empty() && !absolute_match {
        let strength = soft_absolute
            .iter()
            .min_by_key(|window| window.strength.weight())
            .map_or(ConstraintStrength::DEFAULT_SOFT, |window| window.strength);
        add_penalty_violation(
            request,
            item_id,
            interval,
            strength,
            ViolationKind::SoftConstraint,
            interval.minutes(),
            "Placed outside preferred absolute windows.",
            penalty,
            violations,
        );
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn add_penalty_violation(
    request: &PlanRequest,
    item_id: ItemId,
    interval: Interval,
    strength: ConstraintStrength,
    kind: ViolationKind,
    magnitude: u32,
    message: &str,
    penalty: &mut u64,
    violations: &mut Vec<PlanViolation>,
) {
    let value =
        u64::from(soft_weight(request, strength)).saturating_mul(u64::from(magnitude.max(1)));
    *penalty = penalty.saturating_add(value);
    violations.push(PlanViolation {
        kind,
        severity: ViolationSeverity::Warning,
        item_ids: vec![item_id],
        occurrence_ids: Vec::new(),
        start: Some(interval.start),
        end: Some(interval.end),
        penalty: value,
        message: message.to_owned(),
    });
}

fn soft_weight(request: &PlanRequest, strength: ConstraintStrength) -> u32 {
    let configured = strength.weight();
    if configured == 0 {
        request.config.default_soft_weight
    } else {
        configured
    }
}

fn align_up(value: OffsetDateTime, granularity: Minutes) -> OffsetDateTime {
    let step = i64::from(granularity.get()) * 60;
    let timestamp = value.unix_timestamp();
    let remainder = timestamp.rem_euclid(step);
    if remainder == 0 {
        value
    } else {
        value + Duration::seconds(step - remainder)
    }
}

fn positive_minutes(duration: Duration) -> u32 {
    u32::try_from(duration.whole_minutes().max(0)).unwrap_or(u32::MAX)
}

fn ceil_duration_minutes(duration: Duration) -> u32 {
    const NANOS_PER_MINUTE: i128 = 60_000_000_000;
    let nanoseconds = duration.whole_nanoseconds().max(0);
    let minutes = nanoseconds.saturating_add(NANOS_PER_MINUTE - 1) / NANOS_PER_MINUTE;
    u32::try_from(minutes).unwrap_or(u32::MAX)
}

fn overlap_minutes(left: Interval, right: Interval) -> u32 {
    if !left.overlaps(right) {
        return 0;
    }
    Interval {
        start: left.start.max(right.start),
        end: left.end.min(right.end),
    }
    .minutes()
}

fn monday_of(value: OffsetDateTime) -> time::Date {
    value.date() - Duration::days(i64::from(value.weekday().number_days_from_monday()))
}

fn explanation(code: ExplanationCode, message: impl Into<String>) -> PlacementExplanation {
    PlacementExplanation {
        code,
        message: message.into(),
    }
}

/// Deterministic `UUIDv5` derived from stable item identity, session, and time.
fn block_id(item_id: ItemId, session_index: u16, start: OffsetDateTime) -> Uuid {
    let mut name = [0_u8; 18];
    name[..16].copy_from_slice(&start.unix_timestamp_nanos().to_be_bytes());
    name[16..].copy_from_slice(&session_index.to_be_bytes());
    Uuid::new_v5(&item_id.0, &name)
}
