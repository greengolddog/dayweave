use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    AvailabilityWindow, ConstraintStrength, DayOfWeek, Dependency, DependencyRelation,
    FixedBlockSource, ItemId, ItemKind, Minutes, PlanRequest, PreviousBlock, SchedulingConstraints,
    SplitPolicy, WorkItem, roll_up_expected_durations,
};

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
    pub item_id: Option<ItemId>,
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
    pub start: Option<OffsetDateTime>,
    pub end: Option<OffsetDateTime>,
    pub penalty: u64,
    pub message: String,
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
        validate_request(request)?;

        let items: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
        let children = child_map(&request.items);
        let mut state = PlanningState::new(request);

        state.add_external_fixed_blocks(request);
        state.add_calendar_events(request);
        state.add_pinned_assignments(request, &items);
        state.detect_immutable_overlaps();

        let mut eligible = Vec::new();
        for item in &request.items {
            if matches!(item.kind, ItemKind::CalendarEvent(_)) {
                continue;
            }
            if item.status.is_terminal() {
                state.decisions.push(PlanDecision {
                    item_id: item.id,
                    kind: DecisionKind::TerminalItemIgnored,
                    message: "Completed, skipped, or canceled work does not reserve future time."
                        .to_owned(),
                });
                continue;
            }
            if item.status == crate::WorkStatus::Blocked {
                state.unscheduled.push(UnscheduledWork {
                    item_id: item.id,
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
                remaining,
                reason: UnscheduledReason::DependencyCycle,
                message: "Hard dependencies form a cycle; edit or soften one dependency."
                    .to_owned(),
            });
        }

        Ok(state.finish(request))
    }
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

#[derive(Debug, Clone)]
struct BusyBlock {
    interval: Interval,
    item_id: Option<ItemId>,
    pinned: bool,
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
}

impl PlanningState {
    fn new(request: &PlanRequest) -> Self {
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
        Self {
            blocks: Vec::new(),
            busy: Vec::new(),
            unscheduled: Vec::new(),
            decisions: Vec::new(),
            violations: Vec::new(),
            score: PlanScore::default(),
            previous,
            pinned_minutes: BTreeMap::new(),
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
            });
            self.blocks.push(ScheduleBlock {
                id: fixed.id,
                item_id: None,
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
            });
            self.blocks.push(ScheduleBlock {
                id: block_id(item.id, 0, event.start),
                item_id: Some(item.id),
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
                kind: DecisionKind::FixedEventRetained,
                message: "The calendar event remains at its source time.".to_owned(),
            });
        }
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
                });
                self.blocks.push(ScheduleBlock {
                    id: block_id(item.id, block.session_index, block.start),
                    item_id: Some(item.id),
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
                kind: DecisionKind::KeptPinned,
                message: "Pinned sessions were preserved exactly.".to_owned(),
            });
        }
    }

    fn detect_immutable_overlaps(&mut self) {
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
                        start: Some(left.interval.start.max(right.interval.start)),
                        end: Some(left.interval.end.min(right.interval.end)),
                        penalty: 0,
                        message: "Immutable blocks overlap; both remain visible for resolution."
                            .to_owned(),
                    });
                }
            }
        }
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
                remaining: Minutes::ZERO,
                reason: UnscheduledReason::MissingDuration,
                message: "Add or accept a duration estimate before scheduling.".to_owned(),
            });
            return false;
        };

        let required = duration.planning_minutes().get();
        let pinned = self.pinned_minutes.get(&item.id).copied().unwrap_or(0);
        let mut remaining = required.saturating_sub(pinned);
        if remaining == 0 {
            self.score.scheduled_minutes = self.score.scheduled_minutes.saturating_add(required);
            return true;
        }

        let existing_blocks: Vec<_> = self
            .blocks
            .iter()
            .filter(|block| block.item_id == Some(item.id))
            .collect();
        let mut session_index = existing_blocks
            .iter()
            .map(|block| block.session_index)
            .max()
            .map_or(0, |index| index.saturating_add(1));
        let existing_session_count = u16::try_from(existing_blocks.len()).unwrap_or(u16::MAX);
        let mut sessions_added = 0_u16;
        let mut previous_session_end = existing_blocks.iter().map(|block| block.end).max();
        let mut used_days: BTreeSet<_> = existing_blocks
            .iter()
            .map(|block| block.start.date())
            .collect();

        match &item.split_policy {
            SplitPolicy::Indivisible => {
                if let Some(candidate) = self.best_candidate(
                    request,
                    item,
                    dependencies,
                    all_items,
                    Minutes(remaining),
                    session_index,
                    None,
                    &used_days,
                    None,
                ) {
                    self.accept_candidate(item, candidate, session_index, false);
                    remaining = 0;
                }
            }
            SplitPolicy::Splittable {
                minimum_session,
                maximum_session,
                maximum_sessions,
                minimum_gap,
                maximum_days,
            } => {
                while remaining > 0
                    && existing_session_count.saturating_add(sessions_added) < *maximum_sessions
                {
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
                                session_index,
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
                    self.accept_candidate(item, candidate, session_index, true);
                    remaining = remaining.saturating_sub(size);
                    session_index = session_index.saturating_add(1);
                    sessions_added = sessions_added.saturating_add(1);
                }
            }
        }

        let scheduled_now = required.saturating_sub(remaining);
        self.score.scheduled_minutes = self.score.scheduled_minutes.saturating_add(scheduled_now);
        if remaining == 0 {
            self.decisions.push(PlanDecision {
                item_id: item.id,
                kind: DecisionKind::Scheduled,
                message: format!("Reserved {required} minutes."),
            });
            true
        } else {
            let reason = if matches!(item.split_policy, SplitPolicy::Splittable { .. })
                && sessions_added > 0
            {
                UnscheduledReason::SessionLimit
            } else {
                UnscheduledReason::NoCapacity
            };
            self.score.unscheduled_minutes =
                self.score.unscheduled_minutes.saturating_add(remaining);
            self.unscheduled.push(UnscheduledWork {
                item_id: item.id,
                remaining: Minutes(remaining),
                reason,
                message: format!(
                    "No valid capacity for the remaining {remaining} minutes inside the horizon."
                ),
            });
            self.violations.push(PlanViolation {
                kind: ViolationKind::Capacity,
                severity: ViolationSeverity::Error,
                item_ids: vec![item.id],
                start: None,
                end: None,
                penalty: 0,
                message: format!("{remaining} minutes remain unscheduled."),
            });
            if scheduled_now > 0 {
                self.decisions.push(PlanDecision {
                    item_id: item.id,
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
        if matches!(item.kind, ItemKind::Habit(_) | ItemKind::Routine(_)) {
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
                format!("Session {} of split work.", session_index + 1),
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
        });
        self.blocks.push(ScheduleBlock {
            id: block_id(item.id, session_index, candidate.interval.start),
            item_id: Some(item.id),
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
    for assignment in &request.previous_assignments {
        if !ids.contains(&assignment.item_id) {
            return Err(ScheduleError::MissingPreviousItem(assignment.item_id));
        }
        for block in &assignment.blocks {
            if block.start >= block.end {
                return Err(ScheduleError::InvalidWindow {
                    owner: format!("previous assignment for {}", assignment.item_id),
                });
            }
        }
    }
    Ok(())
}

fn validate_constraints(item: &WorkItem) -> Result<(), ScheduleError> {
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
        ItemKind::Habit(_) | ItemKind::Routine(_) => 1,
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
