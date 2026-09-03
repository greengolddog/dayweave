use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use dayweave_compose::MAX_SCHEDULING_OFFSET_MINUTES;
use dayweave_core::{
    ConstraintStrength, ItemId, ItemKind, PlanRequest, Recurrence, RecurrenceExceptionAction,
    RecurrenceExceptionSelector, RecurrenceOccurrenceIdentity, RecurrencePeriod,
    RecurrenceSemantics, ScheduleError, SplitPolicy, WorkItem,
};
use time::{Duration, OffsetDateTime};

const MAX_ITEMS: usize = 10_000;
const MAX_AVAILABILITY_WINDOWS: usize = 10_000;
const MAX_FIXED_BLOCKS: usize = 10_000;
const MAX_PREVIOUS_ASSIGNMENTS: usize = 10_000;
const MAX_PREVIOUS_BLOCKS: usize = 50_000;
const MAX_RECURRENCE_CONTEXT_ENTRIES: usize = 10_000;
const MAX_RECURRENCE_CALENDAR_DAYS: usize = 92;
const MAX_CONSTRAINT_ENTRIES: usize = 50_000;
const MAX_HORIZON_DAYS: i64 = 90;
const MAX_WEIGHT: u32 = 1_000_000;
const MAX_TITLE_CHARACTERS: usize = 500;
const MAX_OCCURRENCES: usize = 10_000;
const MAX_MATERIALIZED_ITEMS: usize = 10_000;
const MAX_HIERARCHY_DEPTH: usize = 256;
const MAX_CANDIDATE_EVALUATIONS: usize = 10_000_000;
const MAX_IMMUTABLE_OVERLAP_VIOLATIONS: usize = 10_000;
const MAX_MATERIALIZED_COLLECTION_ENTRIES: usize = 100_000;
const MAX_MATERIALIZED_STRING_BYTES: usize = 16 * 1024 * 1024;
const MAX_CANDIDATE_STRING_WORK_BYTES: usize = 128 * 1024 * 1024;
const GENERATED_DEPENDENCY_ALLOWANCE: usize = 2;

const CONTEXT_UNAVAILABLE_OVERHEAD: usize = "Required context '' is unavailable.".len();
const CONTEXT_MATCH_OVERHEAD: usize = "Matches the '' context.".len();
const LOCATION_UNAVAILABLE_OVERHEAD: usize = "Required location '' is unavailable.".len();

#[derive(Debug)]
pub(crate) enum PreflightError {
    Schedule(ScheduleError),
    InvalidRequest,
    ResourceLimit,
}

pub(crate) fn validate(request: &PlanRequest) -> Result<(), PreflightError> {
    validate_shape(request)?;
    validate_items(request)?;
    validate_instants(request)?;
    validate_recurrence_references(request)?;
    validate_rolling_anchor_bounds(request)?;
    validate_complexity(request)
}

fn validate_shape(request: &PlanRequest) -> Result<(), PreflightError> {
    let horizon = request.horizon_end - request.horizon_start;
    if horizon <= Duration::ZERO
        || horizon > Duration::days(MAX_HORIZON_DAYS)
        || request.as_of > request.horizon_end
    {
        return Err(PreflightError::Schedule(ScheduleError::InvalidHorizon));
    }
    if !(1..=60).contains(&request.config.slot_granularity.get()) {
        return Err(PreflightError::Schedule(ScheduleError::InvalidGranularity));
    }
    if request.items.len() > MAX_ITEMS
        || request.availability.len() > MAX_AVAILABILITY_WINDOWS
        || request.fixed_blocks.len() > MAX_FIXED_BLOCKS
        || request.previous_assignments.len() > MAX_PREVIOUS_ASSIGNMENTS
        || request.recurrence_context.calendar.days.len() > MAX_RECURRENCE_CALENDAR_DAYS
        || request.config.stability_weight > MAX_WEIGHT
        || request.config.default_soft_weight > MAX_WEIGHT
    {
        return Err(PreflightError::ResourceLimit);
    }

    let previous_blocks = request
        .previous_assignments
        .iter()
        .try_fold(0_usize, |total, assignment| {
            total.checked_add(assignment.blocks.len())
        })
        .ok_or(PreflightError::ResourceLimit)?;
    if previous_blocks > MAX_PREVIOUS_BLOCKS {
        return Err(PreflightError::ResourceLimit);
    }

    let context = &request.recurrence_context;
    let context_entries = context
        .completion_anchors
        .len()
        .saturating_add(context.rolling_anchors.len())
        .saturating_add(context.minimum_spacing.len())
        .saturating_add(context.completed_occurrence_ids.len())
        .saturating_add(context.pauses.len())
        .saturating_add(context.exceptions.len());
    if context_entries > MAX_RECURRENCE_CONTEXT_ENTRIES {
        return Err(PreflightError::ResourceLimit);
    }
    if context
        .minimum_spacing
        .values()
        .any(|spacing| spacing.get() > MAX_SCHEDULING_OFFSET_MINUTES)
    {
        return Err(PreflightError::ResourceLimit);
    }

    let mut fixed_ids = BTreeSet::new();
    for block in &request.fixed_blocks {
        if !valid_title(&block.title) || !fixed_ids.insert(block.id) {
            return Err(PreflightError::InvalidRequest);
        }
    }
    Ok(())
}

fn validate_items(request: &PlanRequest) -> Result<(), PreflightError> {
    let mut ids = BTreeSet::new();
    let mut constraint_entries = 0_usize;
    for item in &request.items {
        if !ids.insert(item.id) {
            return Err(PreflightError::Schedule(ScheduleError::DuplicateItem(
                item.id,
            )));
        }
        if !valid_title(&item.title) {
            return Err(invalid_item(item.id));
        }
        if !item_weights_are_bounded(item) {
            return Err(PreflightError::ResourceLimit);
        }
        if !item_temporal_offsets_are_bounded(item) {
            return Err(PreflightError::ResourceLimit);
        }
        if matches!(recurrence_of(item), Some(Recurrence::Custom { .. })) {
            return Err(PreflightError::Schedule(ScheduleError::InvalidRecurrence(
                "custom RRULE recurrence has no bounded scheduler expansion".to_owned(),
            )));
        }
        constraint_entries = constraint_entries
            .checked_add(constraint_entry_count(item)?)
            .ok_or(PreflightError::ResourceLimit)?;
    }
    if constraint_entries > MAX_CONSTRAINT_ENTRIES {
        return Err(PreflightError::ResourceLimit);
    }
    validate_hierarchy(request, &ids)
}

fn validate_hierarchy(request: &PlanRequest, ids: &BTreeSet<ItemId>) -> Result<(), PreflightError> {
    let mut children = BTreeMap::<ItemId, Vec<ItemId>>::new();
    let mut queue = VecDeque::new();
    for item in &request.items {
        if let Some(parent) = item.parent_id {
            if !ids.contains(&parent) {
                return Err(PreflightError::Schedule(ScheduleError::InvalidHierarchy(
                    String::new(),
                )));
            }
            children.entry(parent).or_default().push(item.id);
        } else {
            queue.push_back((item.id, 1_usize));
        }
    }

    let mut visited = 0_usize;
    while let Some((id, depth)) = queue.pop_front() {
        if depth > MAX_HIERARCHY_DEPTH {
            return Err(PreflightError::ResourceLimit);
        }
        visited = visited
            .checked_add(1)
            .ok_or(PreflightError::ResourceLimit)?;
        if let Some(nested) = children.get(&id) {
            let child_depth = depth.checked_add(1).ok_or(PreflightError::ResourceLimit)?;
            queue.extend(nested.iter().copied().map(|child| (child, child_depth)));
        }
    }
    if visited != request.items.len() {
        return Err(PreflightError::Schedule(ScheduleError::InvalidHierarchy(
            String::new(),
        )));
    }
    Ok(())
}

fn validate_instants(request: &PlanRequest) -> Result<(), PreflightError> {
    let top_level = [request.as_of, request.horizon_start, request.horizon_end];
    if top_level.into_iter().any(|value| !is_microsecond(value))
        || request
            .availability
            .iter()
            .any(|window| !is_microsecond(window.start) || !is_microsecond(window.end))
        || request
            .fixed_blocks
            .iter()
            .any(|block| !is_microsecond(block.start) || !is_microsecond(block.end))
        || request.previous_assignments.iter().any(|assignment| {
            assignment
                .blocks
                .iter()
                .any(|block| !is_microsecond(block.start) || !is_microsecond(block.end))
        })
        || request.items.iter().any(item_has_imprecise_instant)
        || recurrence_context_has_imprecise_instant(request)
    {
        return Err(PreflightError::InvalidRequest);
    }
    Ok(())
}

fn item_has_imprecise_instant(item: &WorkItem) -> bool {
    let constraints = &item.constraints;
    !is_microsecond(item.created_at)
        || !is_microsecond(item.updated_at)
        || constraints
            .earliest_start
            .as_ref()
            .is_some_and(|value| !is_microsecond(value.value))
        || constraints
            .latest_finish
            .as_ref()
            .is_some_and(|value| !is_microsecond(value.value))
        || constraints
            .preferred_absolute_windows
            .iter()
            .chain(&constraints.forbidden_windows)
            .any(|window| !is_microsecond(window.value.start) || !is_microsecond(window.value.end))
        || constraints
            .occurrence_window
            .is_some_and(|window| !is_microsecond(window.start) || !is_microsecond(window.end))
        || match &item.kind {
            ItemKind::CalendarEvent(event) => {
                !is_microsecond(event.start) || !is_microsecond(event.end)
            }
            ItemKind::RecurringTask(spec) => recurrence_has_imprecise_anchor(&spec.recurrence),
            ItemKind::Habit(spec) => recurrence_has_imprecise_anchor(&spec.recurrence),
            ItemKind::Routine(spec) => spec
                .recurrence
                .as_ref()
                .is_some_and(recurrence_has_imprecise_anchor),
            ItemKind::Task | ItemKind::Goal(_) | ItemKind::Break(_) => false,
        }
}

fn recurrence_has_imprecise_anchor(recurrence: &Recurrence) -> bool {
    matches!(
        recurrence,
        Recurrence::Frequency {
            anchor: Some(anchor),
            ..
        } if !is_microsecond(*anchor)
    )
}

fn recurrence_context_has_imprecise_instant(request: &PlanRequest) -> bool {
    let context = &request.recurrence_context;
    context
        .completion_anchors
        .values()
        .chain(context.rolling_anchors.values())
        .any(|value| !is_microsecond(*value))
        || context
            .calendar
            .days
            .iter()
            .any(|day| !is_microsecond(day.start) || !is_microsecond(day.end))
        || context
            .pauses
            .iter()
            .any(|pause| !is_microsecond(pause.start) || !is_microsecond(pause.end))
        || context.exceptions.iter().any(|exception| {
            let selector = match exception.selector {
                RecurrenceExceptionSelector::NominalStart { at } => !is_microsecond(at),
                RecurrenceExceptionSelector::Occurrence { .. }
                | RecurrenceExceptionSelector::LocalDate { .. } => false,
            };
            let action = match exception.action {
                RecurrenceExceptionAction::Move { start, end, source } => {
                    !is_microsecond(start)
                        || !is_microsecond(end)
                        || !is_microsecond(source.nominal_start)
                        || !is_microsecond(source.nominal_end)
                        || matches!(
                            source.identity,
                            RecurrenceOccurrenceIdentity::AfterCompletion { anchor }
                                | RecurrenceOccurrenceIdentity::RollingMinutes {
                                    anchor,
                                    ..
                                }
                                | RecurrenceOccurrenceIdentity::RollingMonth {
                                    anchor,
                                    ..
                                }
                                if !is_microsecond(anchor)
                        )
                }
                RecurrenceExceptionAction::Skip => false,
            };
            selector || action
        })
}

fn validate_recurrence_references(request: &PlanRequest) -> Result<(), PreflightError> {
    let ids: BTreeSet<_> = request.items.iter().map(|item| item.id).collect();
    let context = &request.recurrence_context;
    let mut references = Vec::new();
    references.extend(context.completion_anchors.keys().copied());
    references.extend(context.rolling_anchors.keys().copied());
    references.extend(context.minimum_spacing.keys().copied());
    references.extend(context.pauses.iter().map(|pause| pause.item_id));
    references.extend(context.exceptions.iter().map(|exception| exception.item_id));
    if references
        .into_iter()
        .any(|item_id| !ids.contains(&item_id))
    {
        return Err(PreflightError::Schedule(ScheduleError::InvalidRecurrence(
            String::new(),
        )));
    }
    Ok(())
}

fn validate_rolling_anchor_bounds(request: &PlanRequest) -> Result<(), PreflightError> {
    for item in &request.items {
        let Some(recurrence) = recurrence_of(item) else {
            continue;
        };
        let (anchor, interval_minutes) = match recurrence {
            Recurrence::EveryInterval { interval } => (
                request
                    .recurrence_context
                    .rolling_anchors
                    .get(&item.id)
                    .copied()
                    .unwrap_or(item.created_at),
                interval.get(),
            ),
            Recurrence::Frequency {
                target,
                period,
                semantics: RecurrenceSemantics::Rolling,
                anchor,
                ..
            } if matches!(period, RecurrencePeriod::Day | RecurrencePeriod::Week)
                && *target > 0 =>
            {
                let period_minutes = match period {
                    RecurrencePeriod::Day => 24 * 60,
                    RecurrencePeriod::Week => 7 * 24 * 60,
                    RecurrencePeriod::Month => continue,
                };
                if u32::from(*target) > period_minutes {
                    continue;
                }
                (
                    anchor
                        .or_else(|| {
                            request
                                .recurrence_context
                                .rolling_anchors
                                .get(&item.id)
                                .copied()
                        })
                        .unwrap_or(item.created_at),
                    period_minutes / u32::from(*target),
                )
            }
            Recurrence::Daily { .. }
            | Recurrence::Weekly { .. }
            | Recurrence::Monthly { .. }
            | Recurrence::AfterCompletion { .. }
            | Recurrence::Frequency { .. }
            | Recurrence::Custom { .. } => continue,
        };
        if interval_minutes == 0 {
            continue;
        }
        let elapsed_minutes = (request.horizon_start - anchor).whole_minutes();
        if elapsed_minutes > 0
            && elapsed_minutes.div_euclid(i64::from(interval_minutes)) > i64::from(i32::MAX) - 2
        {
            return Err(PreflightError::ResourceLimit);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // One checked pass keeps correlated materialization bounds aligned.
fn validate_complexity(request: &PlanRequest) -> Result<(), PreflightError> {
    let by_id: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
    let mut children = BTreeMap::<ItemId, Vec<ItemId>>::new();
    let mut roots = Vec::new();
    for item in &request.items {
        if let Some(parent) = item.parent_id {
            children.entry(parent).or_default().push(item.id);
        } else {
            roots.push(item.id);
        }
    }

    let mut order = Vec::with_capacity(request.items.len());
    let mut recurrence_roots = Vec::new();
    let mut queue: VecDeque<_> = roots.into_iter().map(|id| (id, false)).collect();
    while let Some((id, has_recurring_ancestor)) = queue.pop_front() {
        order.push(id);
        let is_recurrence_root = !has_recurring_ancestor && recurrence_of(by_id[&id]).is_some();
        if is_recurrence_root {
            recurrence_roots.push(id);
        }
        let descendant_has_recurring_ancestor = has_recurring_ancestor || is_recurrence_root;
        if let Some(nested) = children.get(&id) {
            queue.extend(
                nested
                    .iter()
                    .copied()
                    .map(|child| (child, descendant_has_recurring_ancestor)),
            );
        }
    }

    let mut subtree_sizes: BTreeMap<_, usize> = request
        .items
        .iter()
        .map(|item| (item.id, 1_usize))
        .collect();
    let mut subtree_sessions: BTreeMap<_, usize> = request
        .items
        .iter()
        .map(|item| session_bound(item).map(|sessions| (item.id, sessions)))
        .collect::<Result<_, _>>()?;
    let mut subtree_attempts: BTreeMap<_, usize> = request
        .items
        .iter()
        .map(|item| {
            attempt_bound(item, request.config.slot_granularity.get())
                .map(|attempts| (item.id, attempts))
        })
        .collect::<Result<_, _>>()?;
    for id in order.into_iter().rev() {
        let size = subtree_sizes[&id];
        let sessions = subtree_sessions[&id];
        let attempts = subtree_attempts[&id];
        if let Some(parent) = by_id[&id].parent_id {
            let parent_size = subtree_sizes
                .get_mut(&parent)
                .ok_or(PreflightError::InvalidRequest)?;
            *parent_size = parent_size
                .checked_add(size)
                .ok_or(PreflightError::ResourceLimit)?;
            let parent_sessions = subtree_sessions
                .get_mut(&parent)
                .ok_or(PreflightError::InvalidRequest)?;
            *parent_sessions = parent_sessions
                .checked_add(sessions)
                .ok_or(PreflightError::ResourceLimit)?;
            let parent_attempts = subtree_attempts
                .get_mut(&parent)
                .ok_or(PreflightError::InvalidRequest)?;
            *parent_attempts = parent_attempts
                .checked_add(attempts)
                .ok_or(PreflightError::ResourceLimit)?;
        }
    }

    let day_count = recurrence_day_bound(request)?;
    let horizon_minutes = horizon_minute_bound(request)?;
    let source_sessions = request.items.iter().try_fold(0_usize, |total, item| {
        total
            .checked_add(session_bound(item)?)
            .ok_or(PreflightError::ResourceLimit)
    })?;
    let source_attempts = request.items.iter().try_fold(0_usize, |total, item| {
        total
            .checked_add(attempt_bound(item, request.config.slot_granularity.get())?)
            .ok_or(PreflightError::ResourceLimit)
    })?;
    let mut removed_items = 0_usize;
    let mut removed_sessions = 0_usize;
    let mut removed_attempts = 0_usize;
    let mut occurrence_count = 0_usize;
    let mut cloned_items = 0_usize;
    let mut cloned_sessions = 0_usize;
    let mut cloned_attempts = 0_usize;
    let mut root_occurrences = BTreeMap::new();
    let moved_occurrence_bounds = moved_occurrence_bounds(request);
    for root in recurrence_roots {
        let subtree_size = subtree_sizes[&root];
        let subtree_session_count = subtree_sessions[&root];
        let subtree_attempt_count = subtree_attempts[&root];
        let recurrence = recurrence_of(by_id[&root]).ok_or(PreflightError::InvalidRequest)?;
        // Core can restore an occurrence-ID move after its nominal bucket has rolled out of the
        // horizon. Count every distinct in-horizon destination conservatively; a selector that
        // also matches a native occurrence may overestimate by one, but may never bypass the
        // materialization and candidate-work ceilings.
        let occurrences = recurrence_bound(recurrence, day_count, horizon_minutes)?
            .checked_add(moved_occurrence_bounds.get(&root).copied().unwrap_or(0))
            .ok_or(PreflightError::ResourceLimit)?;
        root_occurrences.insert(root, occurrences);
        removed_items = removed_items
            .checked_add(subtree_size)
            .ok_or(PreflightError::ResourceLimit)?;
        removed_sessions = removed_sessions
            .checked_add(subtree_session_count)
            .ok_or(PreflightError::ResourceLimit)?;
        removed_attempts = removed_attempts
            .checked_add(subtree_attempt_count)
            .ok_or(PreflightError::ResourceLimit)?;
        occurrence_count = occurrence_count
            .checked_add(occurrences)
            .ok_or(PreflightError::ResourceLimit)?;
        cloned_items = cloned_items
            .checked_add(
                occurrences
                    .checked_mul(subtree_size)
                    .ok_or(PreflightError::ResourceLimit)?,
            )
            .ok_or(PreflightError::ResourceLimit)?;
        cloned_sessions = cloned_sessions
            .checked_add(
                occurrences
                    .checked_mul(subtree_session_count)
                    .ok_or(PreflightError::ResourceLimit)?,
            )
            .ok_or(PreflightError::ResourceLimit)?;
        cloned_attempts = cloned_attempts
            .checked_add(
                occurrences
                    .checked_mul(subtree_attempt_count)
                    .ok_or(PreflightError::ResourceLimit)?,
            )
            .ok_or(PreflightError::ResourceLimit)?;
        if occurrence_count > MAX_OCCURRENCES || cloned_items > MAX_MATERIALIZED_ITEMS {
            return Err(PreflightError::ResourceLimit);
        }
    }
    let materialized_items = request
        .items
        .len()
        .checked_sub(removed_items)
        .and_then(|value| value.checked_add(cloned_items))
        .ok_or(PreflightError::ResourceLimit)?;
    if materialized_items > MAX_MATERIALIZED_ITEMS {
        return Err(PreflightError::ResourceLimit);
    }
    let materialized_sessions = source_sessions
        .checked_sub(removed_sessions)
        .and_then(|value| value.checked_add(cloned_sessions))
        .ok_or(PreflightError::ResourceLimit)?;
    let materialized_attempts = source_attempts
        .checked_sub(removed_attempts)
        .and_then(|value| value.checked_add(cloned_attempts))
        .ok_or(PreflightError::ResourceLimit)?;
    validate_materialized_payload_budget(request, &by_id, &root_occurrences)?;
    validate_immutable_overlap_budget(request, &by_id, &root_occurrences)?;
    let candidate_slots = candidate_slot_bound(request)?;
    validate_candidate_string_work(request, &by_id, &root_occurrences, candidate_slots)?;

    validate_candidate_work(
        request,
        candidate_slots,
        materialized_items,
        materialized_sessions,
        materialized_attempts,
        occurrence_count,
        cloned_items,
    )
}

fn candidate_slot_bound(request: &PlanRequest) -> Result<usize, PreflightError> {
    let granularity_seconds = u64::from(request.config.slot_granularity.get()) * 60;
    let mut candidate_slots = 0_usize;
    for window in &request.availability {
        let start = window.start.max(request.horizon_start);
        let end = window.end.min(request.horizon_end);
        if start < end {
            let seconds = u64::try_from((end - start).whole_seconds())
                .map_err(|_| PreflightError::InvalidRequest)?;
            let slots = seconds
                .checked_div(granularity_seconds)
                .and_then(|value| value.checked_add(1))
                .ok_or(PreflightError::ResourceLimit)?;
            candidate_slots = candidate_slots
                .checked_add(usize::try_from(slots).map_err(|_| PreflightError::ResourceLimit)?)
                .ok_or(PreflightError::ResourceLimit)?;
        }
    }
    Ok(candidate_slots)
}

fn validate_candidate_string_work(
    request: &PlanRequest,
    by_id: &BTreeMap<ItemId, &WorkItem>,
    root_occurrences: &BTreeMap<ItemId, usize>,
    candidate_slots: usize,
) -> Result<(), PreflightError> {
    let candidate_string_work =
        request
            .items
            .iter()
            .try_fold(0_usize, |total, item| -> Result<usize, PreflightError> {
                let multiplier = recurrence_multiplier(item.id, by_id, root_occurrences)?;
                let item_work = candidate_string_allocation_bytes(item)?
                    .checked_mul(attempt_bound(item, request.config.slot_granularity.get())?)
                    .and_then(|value| value.checked_mul(multiplier))
                    .and_then(|value| value.checked_mul(candidate_slots))
                    .ok_or(PreflightError::ResourceLimit)?;
                total
                    .checked_add(item_work)
                    .ok_or(PreflightError::ResourceLimit)
            })?;
    if candidate_string_work > MAX_CANDIDATE_STRING_WORK_BYTES {
        return Err(PreflightError::ResourceLimit);
    }
    Ok(())
}

fn validate_candidate_work(
    request: &PlanRequest,
    candidate_slots: usize,
    materialized_items: usize,
    materialized_sessions: usize,
    materialized_attempts: usize,
    occurrence_count: usize,
    recurrence_identity_count: usize,
) -> Result<(), PreflightError> {
    let candidate_evaluations = materialized_attempts
        .checked_mul(candidate_slots)
        .ok_or(PreflightError::ResourceLimit)?;
    let maximum_item_constraints = request.items.iter().try_fold(0_usize, |maximum, item| {
        candidate_check_bound(item).map(|count| maximum.max(count))
    })?;
    let constraint_evaluations = candidate_evaluations
        .checked_mul(
            maximum_item_constraints
                .checked_add(1)
                .ok_or(PreflightError::ResourceLimit)?,
        )
        .ok_or(PreflightError::ResourceLimit)?;
    let pinned_blocks = request
        .previous_assignments
        .iter()
        .filter(|assignment| assignment.pinned)
        .try_fold(0_usize, |total, assignment| {
            total
                .checked_add(assignment.blocks.len())
                .ok_or(PreflightError::ResourceLimit)
        })?;
    let busy_bound = request
        .fixed_blocks
        .len()
        .checked_add(pinned_blocks)
        .and_then(|value| value.checked_add(materialized_sessions))
        .ok_or(PreflightError::ResourceLimit)?;
    let busy_scan_width = busy_bound
        .checked_add(1)
        .ok_or(PreflightError::ResourceLimit)?;
    let busy_scans = materialized_attempts
        .checked_mul(request.availability.len())
        .and_then(|value| value.checked_mul(busy_scan_width))
        .ok_or(PreflightError::ResourceLimit)?;
    let maximum_block_scan_factor = request.items.iter().try_fold(
        1_usize,
        |maximum, item| -> Result<usize, PreflightError> {
            let factor = block_scan_factor(item)?;
            Ok(maximum.max(factor))
        },
    )?;
    let block_evaluations = candidate_evaluations
        .checked_mul(busy_bound)
        .and_then(|value| value.checked_mul(maximum_block_scan_factor))
        .ok_or(PreflightError::ResourceLimit)?;
    let ordering_evaluations = materialized_items
        .checked_mul(materialized_items)
        .ok_or(PreflightError::ResourceLimit)?;
    let previous_mapping_evaluations = request
        .previous_assignments
        .len()
        .checked_mul(recurrence_identity_count)
        .and_then(|value| value.checked_mul(occurrence_count.saturating_add(1)))
        .ok_or(PreflightError::ResourceLimit)?;
    let recurrence_context_evaluations = request
        .recurrence_context
        .pauses
        .len()
        .checked_add(request.recurrence_context.exceptions.len())
        .and_then(|value| value.checked_mul(occurrence_count))
        .ok_or(PreflightError::ResourceLimit)?;
    let total_evaluations = constraint_evaluations
        .checked_add(busy_scans)
        .and_then(|value| value.checked_add(block_evaluations))
        .and_then(|value| value.checked_add(ordering_evaluations))
        .and_then(|value| value.checked_add(previous_mapping_evaluations))
        .and_then(|value| value.checked_add(recurrence_context_evaluations))
        .and_then(|value| value.checked_add(materialized_attempts))
        .ok_or(PreflightError::ResourceLimit)?;
    if total_evaluations > MAX_CANDIDATE_EVALUATIONS {
        return Err(PreflightError::ResourceLimit);
    }
    Ok(())
}

fn validate_immutable_overlap_budget(
    request: &PlanRequest,
    by_id: &BTreeMap<ItemId, &WorkItem>,
    root_occurrences: &BTreeMap<ItemId, usize>,
) -> Result<(), PreflightError> {
    let mut intervals = Vec::new();
    for block in &request.fixed_blocks {
        if interval_intersects_horizon(request, block.start, block.end) {
            intervals.push((block.start, block.end));
        }
    }
    for item in &request.items {
        let ItemKind::CalendarEvent(event) = &item.kind else {
            continue;
        };
        if !interval_intersects_horizon(request, event.start, event.end) {
            continue;
        }
        let copies = recurrence_multiplier(item.id, by_id, root_occurrences)?;
        intervals
            .try_reserve(copies)
            .map_err(|_| PreflightError::ResourceLimit)?;
        intervals.extend(std::iter::repeat_n((event.start, event.end), copies));
    }
    for assignment in &request.previous_assignments {
        if !assignment.pinned
            || by_id
                .get(&assignment.item_id)
                .is_none_or(|item| matches!(&item.kind, ItemKind::CalendarEvent(_)))
        {
            continue;
        }
        intervals.extend(
            assignment
                .blocks
                .iter()
                .filter(|block| interval_intersects_horizon(request, block.start, block.end))
                .map(|block| (block.start, block.end)),
        );
    }
    intervals.sort_unstable();

    let mut active_ends = BinaryHeap::new();
    let mut overlap_count = 0_usize;
    for (start, end) in intervals {
        while active_ends
            .peek()
            .is_some_and(|value: &Reverse<OffsetDateTime>| value.0 <= start)
        {
            active_ends.pop();
        }
        overlap_count = overlap_count
            .checked_add(active_ends.len())
            .ok_or(PreflightError::ResourceLimit)?;
        if overlap_count > MAX_IMMUTABLE_OVERLAP_VIOLATIONS {
            return Err(PreflightError::ResourceLimit);
        }
        active_ends.push(Reverse(end));
    }
    Ok(())
}

fn validate_materialized_payload_budget(
    request: &PlanRequest,
    by_id: &BTreeMap<ItemId, &WorkItem>,
    root_occurrences: &BTreeMap<ItemId, usize>,
) -> Result<(), PreflightError> {
    let mut collection_entries = 0_usize;
    let mut string_bytes = 0_usize;
    for item in &request.items {
        let multiplier = recurrence_multiplier(item.id, by_id, root_occurrences)?;
        collection_entries = collection_entries
            .checked_add(
                item_collection_entries(item)?
                    .checked_mul(multiplier)
                    .ok_or(PreflightError::ResourceLimit)?,
            )
            .ok_or(PreflightError::ResourceLimit)?;
        let cloned_strings = item_string_bytes(item)?
            .checked_mul(multiplier)
            .ok_or(PreflightError::ResourceLimit)?;
        let output_titles = item
            .title
            .len()
            .checked_mul(session_bound(item)?)
            .and_then(|value| value.checked_mul(multiplier))
            .ok_or(PreflightError::ResourceLimit)?;
        let output_candidate_messages = retained_candidate_string_bytes(item)?
            .checked_mul(session_bound(item)?)
            .and_then(|value| value.checked_mul(multiplier))
            .ok_or(PreflightError::ResourceLimit)?;
        string_bytes = string_bytes
            .checked_add(cloned_strings)
            .and_then(|value| value.checked_add(output_titles))
            .and_then(|value| value.checked_add(output_candidate_messages))
            .ok_or(PreflightError::ResourceLimit)?;
        if collection_entries > MAX_MATERIALIZED_COLLECTION_ENTRIES
            || string_bytes > MAX_MATERIALIZED_STRING_BYTES
        {
            return Err(PreflightError::ResourceLimit);
        }
    }

    for block in &request.fixed_blocks {
        if interval_intersects_horizon(request, block.start, block.end) {
            string_bytes = string_bytes
                .checked_add(block.title.len())
                .ok_or(PreflightError::ResourceLimit)?;
        }
    }
    for assignment in &request.previous_assignments {
        if !assignment.pinned {
            continue;
        }
        let title_bytes = by_id
            .get(&assignment.item_id)
            .ok_or(PreflightError::Schedule(
                ScheduleError::MissingPreviousItem(assignment.item_id),
            ))?
            .title
            .len();
        let intersecting_blocks = assignment
            .blocks
            .iter()
            .filter(|block| interval_intersects_horizon(request, block.start, block.end))
            .count();
        string_bytes = string_bytes
            .checked_add(
                title_bytes
                    .checked_mul(intersecting_blocks)
                    .ok_or(PreflightError::ResourceLimit)?,
            )
            .ok_or(PreflightError::ResourceLimit)?;
    }
    if string_bytes > MAX_MATERIALIZED_STRING_BYTES {
        return Err(PreflightError::ResourceLimit);
    }
    Ok(())
}

fn candidate_string_allocation_bytes(item: &WorkItem) -> Result<usize, PreflightError> {
    let mut total = 0_usize;
    for context in &item.constraints.required_contexts {
        let context_bytes = context
            .value
            .len()
            .checked_mul(2)
            .and_then(|value| value.checked_add(CONTEXT_UNAVAILABLE_OVERHEAD))
            .and_then(|value| value.checked_add(CONTEXT_MATCH_OVERHEAD))
            .ok_or(PreflightError::ResourceLimit)?;
        add_string_bytes(&mut total, context_bytes)?;
    }
    if let Some(location) = &item.constraints.required_location {
        let location_bytes = location
            .value
            .len()
            .checked_add(LOCATION_UNAVAILABLE_OVERHEAD)
            .ok_or(PreflightError::ResourceLimit)?;
        add_string_bytes(&mut total, location_bytes)?;
    }
    Ok(total)
}

fn retained_candidate_string_bytes(item: &WorkItem) -> Result<usize, PreflightError> {
    let mut total = 0_usize;
    for context in &item.constraints.required_contexts {
        let context_bytes = context
            .value
            .len()
            .checked_add(CONTEXT_UNAVAILABLE_OVERHEAD.max(CONTEXT_MATCH_OVERHEAD))
            .ok_or(PreflightError::ResourceLimit)?;
        add_string_bytes(&mut total, context_bytes)?;
    }
    if let Some(location) = &item.constraints.required_location {
        let location_bytes = location
            .value
            .len()
            .checked_add(LOCATION_UNAVAILABLE_OVERHEAD)
            .ok_or(PreflightError::ResourceLimit)?;
        add_string_bytes(&mut total, location_bytes)?;
    }
    Ok(total)
}

fn item_collection_entries(item: &WorkItem) -> Result<usize, PreflightError> {
    let constraints = constraint_entry_count(item)?;
    let mut total = item
        .goal_ids
        .len()
        .checked_add(item.tags.len())
        .and_then(|value| value.checked_add(constraints))
        .ok_or(PreflightError::ResourceLimit)?;
    if let Some(weekdays) = &item.constraints.allowed_weekdays {
        total = total
            .checked_add(weekdays.value.len())
            .ok_or(PreflightError::ResourceLimit)?;
    }
    for window in &item.constraints.preferred_daily_windows {
        total = total
            .checked_add(window.value.weekdays.len())
            .ok_or(PreflightError::ResourceLimit)?;
    }
    match &item.kind {
        ItemKind::RecurringTask(spec) => {
            add_recurrence_entries(&spec.recurrence, &mut total)?;
        }
        ItemKind::Habit(spec) => {
            add_recurrence_entries(&spec.recurrence, &mut total)?;
        }
        ItemKind::Routine(spec) => {
            if let Some(recurrence) = &spec.recurrence {
                add_recurrence_entries(recurrence, &mut total)?;
            }
        }
        ItemKind::Goal(spec) => {
            total = total
                .checked_add(spec.measures.len())
                .ok_or(PreflightError::ResourceLimit)?;
        }
        ItemKind::Task | ItemKind::Break(_) | ItemKind::CalendarEvent(_) => {}
    }
    Ok(total)
}

fn add_recurrence_entries(
    recurrence: &Recurrence,
    total: &mut usize,
) -> Result<(), PreflightError> {
    let weekday_count = match recurrence {
        Recurrence::Weekly { weekdays, .. } | Recurrence::Frequency { weekdays, .. } => {
            weekdays.len()
        }
        Recurrence::Daily { .. }
        | Recurrence::Monthly { .. }
        | Recurrence::EveryInterval { .. }
        | Recurrence::AfterCompletion { .. }
        | Recurrence::Custom { .. } => 0,
    };
    *total = (*total)
        .checked_add(weekday_count)
        .ok_or(PreflightError::ResourceLimit)?;
    Ok(())
}

fn item_string_bytes(item: &WorkItem) -> Result<usize, PreflightError> {
    let mut total = item.title.len();
    for tag in &item.tags {
        add_string_bytes(&mut total, tag.len())?;
    }
    for context in &item.constraints.required_contexts {
        add_string_bytes(&mut total, context.value.len())?;
    }
    if let Some(location) = &item.constraints.required_location {
        add_string_bytes(&mut total, location.value.len())?;
    }
    match &item.kind {
        ItemKind::RecurringTask(spec) => add_recurrence_string_bytes(&spec.recurrence, &mut total)?,
        ItemKind::Habit(spec) => {
            add_recurrence_string_bytes(&spec.recurrence, &mut total)?;
            if let Some(target) = &spec.target {
                add_string_bytes(&mut total, target.unit.len())?;
            }
        }
        ItemKind::Routine(spec) => {
            if let Some(recurrence) = &spec.recurrence {
                add_recurrence_string_bytes(recurrence, &mut total)?;
            }
        }
        ItemKind::Goal(spec) => {
            for measure in &spec.measures {
                add_string_bytes(&mut total, measure.name.len())?;
                add_string_bytes(&mut total, measure.unit.len())?;
            }
        }
        ItemKind::CalendarEvent(event) => {
            if let Some(calendar_id) = &event.source_calendar_id {
                add_string_bytes(&mut total, calendar_id.len())?;
            }
        }
        ItemKind::Task | ItemKind::Break(_) => {}
    }
    Ok(total)
}

fn add_recurrence_string_bytes(
    recurrence: &Recurrence,
    total: &mut usize,
) -> Result<(), PreflightError> {
    if let Recurrence::Custom { rrule } = recurrence {
        add_string_bytes(total, rrule.len())?;
    }
    Ok(())
}

fn add_string_bytes(total: &mut usize, value: usize) -> Result<(), PreflightError> {
    *total = (*total)
        .checked_add(value)
        .ok_or(PreflightError::ResourceLimit)?;
    Ok(())
}

fn interval_intersects_horizon(
    request: &PlanRequest,
    start: OffsetDateTime,
    end: OffsetDateTime,
) -> bool {
    start < end && start < request.horizon_end && request.horizon_start < end
}

fn recurrence_multiplier(
    item_id: ItemId,
    by_id: &BTreeMap<ItemId, &WorkItem>,
    root_occurrences: &BTreeMap<ItemId, usize>,
) -> Result<usize, PreflightError> {
    let mut current = Some(item_id);
    for _ in 0..=MAX_HIERARCHY_DEPTH {
        let Some(id) = current else {
            return Ok(1);
        };
        if let Some(copies) = root_occurrences.get(&id) {
            return Ok(*copies);
        }
        current = by_id
            .get(&id)
            .ok_or(PreflightError::InvalidRequest)?
            .parent_id;
    }
    Err(PreflightError::ResourceLimit)
}

fn recurrence_day_bound(request: &PlanRequest) -> Result<usize, PreflightError> {
    if !request.recurrence_context.calendar.days.is_empty() {
        return Ok(request.recurrence_context.calendar.days.len());
    }
    let seconds = u64::try_from((request.horizon_end - request.horizon_start).whole_seconds())
        .map_err(|_| PreflightError::InvalidRequest)?;
    let days = seconds
        .checked_div(24 * 60 * 60)
        .and_then(|value| value.checked_add(2))
        .ok_or(PreflightError::ResourceLimit)?;
    usize::try_from(days).map_err(|_| PreflightError::ResourceLimit)
}

fn horizon_minute_bound(request: &PlanRequest) -> Result<usize, PreflightError> {
    let seconds = u64::try_from((request.horizon_end - request.horizon_start).whole_seconds())
        .map_err(|_| PreflightError::InvalidRequest)?;
    let minutes = seconds
        .checked_div(60)
        .and_then(|value| value.checked_add(1))
        .ok_or(PreflightError::ResourceLimit)?;
    usize::try_from(minutes).map_err(|_| PreflightError::ResourceLimit)
}

fn recurrence_bound(
    recurrence: &Recurrence,
    day_count: usize,
    horizon_minutes: usize,
) -> Result<usize, PreflightError> {
    let calendar_bound = |target: u16, bucket_count: usize| {
        usize::from(target)
            .checked_mul(bucket_count)
            .ok_or(PreflightError::ResourceLimit)
    };
    let rolling_bound = |interval: u32| {
        if interval == 0 {
            return Ok(0);
        }
        horizon_minutes
            .checked_div(interval as usize)
            .and_then(|value| value.checked_add(2))
            .ok_or(PreflightError::ResourceLimit)
    };
    let week_count = day_count
        .checked_add(6)
        .and_then(|value| value.checked_div(7))
        .and_then(|value| value.checked_add(1))
        .ok_or(PreflightError::ResourceLimit)?;
    let month_count = day_count
        .checked_add(27)
        .and_then(|value| value.checked_div(28))
        .and_then(|value| value.checked_add(1))
        .ok_or(PreflightError::ResourceLimit)?;

    match recurrence {
        Recurrence::Daily { times_per_day } => calendar_bound(*times_per_day, day_count),
        Recurrence::Weekly { times_per_week, .. } => calendar_bound(*times_per_week, week_count),
        Recurrence::Monthly { times_per_month } => calendar_bound(*times_per_month, month_count),
        Recurrence::EveryInterval { interval } => rolling_bound(interval.get()),
        Recurrence::AfterCompletion { .. } | Recurrence::Custom { .. } => Ok(1),
        Recurrence::Frequency {
            target,
            period,
            semantics,
            ..
        } => match semantics {
            RecurrenceSemantics::Calendar => match period {
                RecurrencePeriod::Day => calendar_bound(*target, day_count),
                RecurrencePeriod::Week => calendar_bound(*target, week_count),
                RecurrencePeriod::Month => calendar_bound(*target, month_count),
            },
            RecurrenceSemantics::Rolling => match period {
                RecurrencePeriod::Day => rolling_frequency_bound(*target, 24 * 60, horizon_minutes),
                RecurrencePeriod::Week => {
                    rolling_frequency_bound(*target, 7 * 24 * 60, horizon_minutes)
                }
                RecurrencePeriod::Month => calendar_bound(*target, month_count + 1),
            },
        },
    }
}

fn moved_occurrence_bounds(request: &PlanRequest) -> BTreeMap<ItemId, usize> {
    let mut ids_by_item = BTreeMap::<ItemId, BTreeSet<_>>::new();
    for exception in &request.recurrence_context.exceptions {
        let RecurrenceExceptionSelector::Occurrence { id } = exception.selector else {
            continue;
        };
        let RecurrenceExceptionAction::Move { start, end, .. } = exception.action else {
            continue;
        };
        if request.horizon_start <= start && end <= request.horizon_end {
            ids_by_item.entry(exception.item_id).or_default().insert(id);
        }
    }
    ids_by_item
        .into_iter()
        .map(|(item_id, ids)| (item_id, ids.len()))
        .collect()
}

fn rolling_frequency_bound(
    target: u16,
    period_minutes: u32,
    horizon_minutes: usize,
) -> Result<usize, PreflightError> {
    if target == 0 {
        return Ok(0);
    }
    if u32::from(target) > period_minutes {
        return Err(PreflightError::Schedule(ScheduleError::InvalidRecurrence(
            String::new(),
        )));
    }
    let interval = period_minutes / u32::from(target);
    horizon_minutes
        .checked_div(interval as usize)
        .and_then(|value| value.checked_add(2))
        .ok_or(PreflightError::ResourceLimit)
}

fn recurrence_of(item: &WorkItem) -> Option<&Recurrence> {
    match &item.kind {
        ItemKind::RecurringTask(spec) => Some(&spec.recurrence),
        ItemKind::Habit(spec) => Some(&spec.recurrence),
        ItemKind::Routine(spec) => spec.recurrence.as_ref(),
        ItemKind::Task | ItemKind::Goal(_) | ItemKind::Break(_) | ItemKind::CalendarEvent(_) => {
            None
        }
    }
}

fn constraint_entry_count(item: &WorkItem) -> Result<usize, PreflightError> {
    let constraints = &item.constraints;
    [
        constraints.preferred_daily_windows.len(),
        constraints.preferred_absolute_windows.len(),
        constraints.forbidden_windows.len(),
        constraints.required_contexts.len(),
        constraints.dependencies.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, count| {
        total
            .checked_add(count)
            .ok_or(PreflightError::ResourceLimit)
    })
}

fn candidate_check_bound(item: &WorkItem) -> Result<usize, PreflightError> {
    let constraints = &item.constraints;
    let optional_scalar_checks = [
        constraints.earliest_start.is_some(),
        constraints.latest_finish.is_some(),
        constraints.minimum_notice.is_some(),
        constraints.allowed_weekdays.is_some(),
        constraints.required_location.is_some(),
        constraints.maximum_daily_work.is_some(),
        constraints.maximum_weekly_work.is_some(),
        constraints.buffers.strength.is_some(),
        item.energy.is_some(),
    ]
    .into_iter()
    .map(usize::from)
    .sum::<usize>();
    // Preferred-window groups are each traversed up to five times: hard
    // collection/match, overall match, and soft collection/minimum.
    let repeated_window_checks = constraints
        .preferred_daily_windows
        .len()
        .checked_add(constraints.preferred_absolute_windows.len())
        .and_then(|value| value.checked_mul(4))
        .ok_or(PreflightError::ResourceLimit)?;
    constraint_entry_count(item)?
        .checked_add(optional_scalar_checks)
        // Recurrence materialization adds an occurrence window to every clone.
        .and_then(|value| value.checked_add(1))
        .and_then(|value| value.checked_add(repeated_window_checks))
        .and_then(|value| value.checked_add(GENERATED_DEPENDENCY_ALLOWANCE))
        .ok_or(PreflightError::ResourceLimit)
}

fn block_scan_factor(item: &WorkItem) -> Result<usize, PreflightError> {
    let constraints = &item.constraints;
    constraints
        .dependencies
        .len()
        .checked_add(GENERATED_DEPENDENCY_ALLOWANCE)
        // `evaluate_limits` always collects this item's existing blocks.
        .and_then(|value| value.checked_add(1))
        .and_then(|value| value.checked_add(usize::from(constraints.maximum_daily_work.is_some())))
        .and_then(|value| value.checked_add(usize::from(constraints.maximum_weekly_work.is_some())))
        .and_then(|value| value.checked_add(usize::from(constraints.buffers.strength.is_some())))
        .ok_or(PreflightError::ResourceLimit)
}

fn session_bound(item: &WorkItem) -> Result<usize, PreflightError> {
    if matches!(&item.kind, ItemKind::CalendarEvent(_)) {
        return Ok(1);
    }
    let Some(duration) = item.duration else {
        return Ok(0);
    };
    match &item.split_policy {
        SplitPolicy::Indivisible => Ok(1),
        SplitPolicy::Splittable {
            minimum_session,
            maximum_sessions,
            ..
        } => {
            if minimum_session.is_zero() || *maximum_sessions == 0 {
                return Ok(0);
            }
            let duration_sessions = duration.maximum.get().div_ceil(minimum_session.get());
            usize::try_from(duration_sessions.min(u32::from(*maximum_sessions)))
                .map_err(|_| PreflightError::ResourceLimit)
        }
    }
}

fn attempt_bound(item: &WorkItem, granularity: u32) -> Result<usize, PreflightError> {
    let sessions = session_bound(item)?;
    if sessions == 0 || matches!(&item.kind, ItemKind::CalendarEvent(_)) {
        return Ok(0);
    }
    match &item.split_policy {
        SplitPolicy::Indivisible => Ok(1),
        SplitPolicy::Splittable {
            maximum_session, ..
        } => {
            let maximum_duration = item.duration.map_or(0, |duration| duration.maximum.get());
            let largest_attempt = maximum_session.get().min(maximum_duration);
            let shrink_attempts = shrink_attempt_bound(largest_attempt, granularity)?;
            sessions
                .checked_mul(
                    usize::try_from(shrink_attempts).map_err(|_| PreflightError::ResourceLimit)?,
                )
                .ok_or(PreflightError::ResourceLimit)
        }
    }
}

fn shrink_attempt_bound(largest_attempt: u32, granularity: u32) -> Result<u32, PreflightError> {
    largest_attempt
        .saturating_sub(1)
        .div_ceil(granularity)
        .checked_add(1)
        .ok_or(PreflightError::ResourceLimit)
}

fn item_weights_are_bounded(item: &WorkItem) -> bool {
    let constraints = &item.constraints;
    let mut strengths = Vec::new();
    strengths.extend(
        constraints
            .earliest_start
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(constraints.latest_finish.iter().map(|value| value.strength));
    strengths.extend(
        constraints
            .minimum_notice
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .allowed_weekdays
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .preferred_daily_windows
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .preferred_absolute_windows
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .forbidden_windows
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .required_contexts
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .required_location
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .maximum_daily_work
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(
        constraints
            .maximum_weekly_work
            .iter()
            .map(|value| value.strength),
    );
    strengths.extend(constraints.dependencies.iter().map(|value| value.strength));
    strengths.extend(constraints.buffers.strength);
    strengths.extend(item.energy.iter().map(|value| value.strength));
    strengths.into_iter().all(strength_is_bounded)
}

fn item_temporal_offsets_are_bounded(item: &WorkItem) -> bool {
    let constraints = &item.constraints;
    let scalar_offsets_are_bounded = constraints
        .minimum_notice
        .as_ref()
        .is_none_or(|value| value.value.get() <= MAX_SCHEDULING_OFFSET_MINUTES)
        && constraints
            .dependencies
            .iter()
            .all(|dependency| dependency.minimum_lag.get() <= MAX_SCHEDULING_OFFSET_MINUTES)
        && constraints.buffers.before.get() <= MAX_SCHEDULING_OFFSET_MINUTES
        && constraints.buffers.after.get() <= MAX_SCHEDULING_OFFSET_MINUTES;
    let split_offset_is_bounded = match item.split_policy {
        SplitPolicy::Indivisible => true,
        SplitPolicy::Splittable { minimum_gap, .. } => {
            minimum_gap.get() <= MAX_SCHEDULING_OFFSET_MINUTES
        }
    };
    let recurrence_offsets_are_bounded =
        recurrence_of(item).is_none_or(|recurrence| match recurrence {
            Recurrence::EveryInterval { interval } | Recurrence::AfterCompletion { interval } => {
                interval.get() <= MAX_SCHEDULING_OFFSET_MINUTES
            }
            Recurrence::Frequency {
                minimum_spacing, ..
            } => minimum_spacing.get() <= MAX_SCHEDULING_OFFSET_MINUTES,
            Recurrence::Daily { .. }
            | Recurrence::Weekly { .. }
            | Recurrence::Monthly { .. }
            | Recurrence::Custom { .. } => true,
        });
    scalar_offsets_are_bounded && split_offset_is_bounded && recurrence_offsets_are_bounded
}

const fn strength_is_bounded(strength: ConstraintStrength) -> bool {
    match strength {
        ConstraintStrength::Hard => true,
        ConstraintStrength::Soft { weight } => weight <= MAX_WEIGHT,
    }
}

fn valid_title(title: &str) -> bool {
    !title.trim().is_empty()
        && title.chars().count() <= MAX_TITLE_CHARACTERS
        && !title.chars().any(char::is_control)
}

const fn is_microsecond(value: OffsetDateTime) -> bool {
    value.nanosecond().is_multiple_of(1_000)
}

fn invalid_item(item_id: ItemId) -> PreflightError {
    PreflightError::Schedule(ScheduleError::InvalidItem {
        item_id,
        message: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use dayweave_core::{
        ItemId, OccurrenceId, PlanRequest, RecurrenceContext, RecurrenceException,
        RecurrenceExceptionAction, RecurrenceExceptionSelector, RecurrenceMoveSource,
        RecurrenceOccurrenceIdentity, SchedulerConfig,
    };
    use time::{Duration, macros::datetime};
    use uuid::Uuid;

    use super::{
        moved_occurrence_bounds, recurrence_context_has_imprecise_instant, shrink_attempt_bound,
    };

    #[test]
    fn shrink_attempt_bound_includes_the_clamped_minimum_attempt() {
        assert_eq!(shrink_attempt_bound(6, 4).unwrap(), 3);
        assert_eq!(shrink_attempt_bound(8, 4).unwrap(), 3);
        assert_eq!(shrink_attempt_bound(1, 60).unwrap(), 1);
    }

    #[test]
    fn moved_occurrence_bounds_count_unique_destinations_per_item() {
        let start = datetime!(2026-09-01 0:00 UTC);
        let item_id = ItemId::from_uuid(Uuid::from_u128(1));
        let other_item_id = ItemId::from_uuid(Uuid::from_u128(5));
        let first = OccurrenceId(Uuid::from_u128(2));
        let second = OccurrenceId(Uuid::from_u128(3));
        let mut request = PlanRequest {
            as_of: start,
            horizon_start: start,
            horizon_end: start + Duration::days(1),
            items: Vec::new(),
            availability: Vec::new(),
            fixed_blocks: Vec::new(),
            previous_assignments: Vec::new(),
            config: SchedulerConfig::default(),
            recurrence_context: RecurrenceContext::default(),
        };
        let exception = |id, move_start, move_end| RecurrenceException {
            item_id,
            selector: RecurrenceExceptionSelector::Occurrence { id },
            action: RecurrenceExceptionAction::Move {
                start: move_start,
                end: move_end,
                source: RecurrenceMoveSource {
                    item_revision: 1,
                    identity: RecurrenceOccurrenceIdentity::Custom,
                    nominal_start: start,
                    nominal_end: start + Duration::hours(1),
                    local_date: None,
                    ordinal: 0,
                },
            },
        };
        request.recurrence_context.exceptions = vec![
            exception(
                first,
                start + Duration::hours(1),
                start + Duration::hours(2),
            ),
            exception(
                first,
                start + Duration::hours(3),
                start + Duration::hours(4),
            ),
            exception(
                second,
                start + Duration::hours(5),
                start + Duration::hours(6),
            ),
            exception(
                OccurrenceId(Uuid::from_u128(4)),
                start + Duration::days(1),
                start + Duration::days(1) + Duration::hours(1),
            ),
            RecurrenceException {
                item_id,
                selector: RecurrenceExceptionSelector::LocalDate { date: start.date() },
                action: RecurrenceExceptionAction::Skip,
            },
            RecurrenceException {
                item_id: other_item_id,
                selector: RecurrenceExceptionSelector::Occurrence { id: first },
                action: RecurrenceExceptionAction::Move {
                    start: start + Duration::hours(7),
                    end: start + Duration::hours(8),
                    source: RecurrenceMoveSource {
                        item_revision: 1,
                        identity: RecurrenceOccurrenceIdentity::Custom,
                        nominal_start: start,
                        nominal_end: start + Duration::hours(1),
                        local_date: None,
                        ordinal: 0,
                    },
                },
            },
        ];

        let bounds = moved_occurrence_bounds(&request);
        assert_eq!(bounds.get(&item_id), Some(&2));
        assert_eq!(bounds.get(&other_item_id), Some(&1));
    }

    #[test]
    fn recurrence_identity_anchors_obey_microsecond_precision() {
        let base = datetime!(2026-09-01 0:00 UTC);
        let item_id = ItemId::from_uuid(Uuid::from_u128(10));
        let imprecise = base + Duration::nanoseconds(1);
        for identity in [
            RecurrenceOccurrenceIdentity::RollingMinutes {
                index: 0,
                anchor: imprecise,
            },
            RecurrenceOccurrenceIdentity::AfterCompletion { anchor: imprecise },
            RecurrenceOccurrenceIdentity::RollingMonth {
                cycle: 0,
                index: 0,
                anchor: imprecise,
            },
        ] {
            let mut request = PlanRequest {
                as_of: base,
                horizon_start: base,
                horizon_end: base + Duration::days(1),
                items: Vec::new(),
                availability: Vec::new(),
                fixed_blocks: Vec::new(),
                previous_assignments: Vec::new(),
                config: SchedulerConfig::default(),
                recurrence_context: RecurrenceContext::default(),
            };
            request.recurrence_context.exceptions = vec![RecurrenceException {
                item_id,
                selector: RecurrenceExceptionSelector::Occurrence {
                    id: OccurrenceId(Uuid::from_u128(11)),
                },
                action: RecurrenceExceptionAction::Move {
                    start: base + Duration::hours(9),
                    end: base + Duration::hours(10),
                    source: RecurrenceMoveSource {
                        item_revision: 1,
                        identity,
                        nominal_start: base,
                        nominal_end: base + Duration::hours(1),
                        local_date: None,
                        ordinal: 0,
                    },
                },
            }];
            assert!(recurrence_context_has_imprecise_instant(&request));
        }
    }
}
