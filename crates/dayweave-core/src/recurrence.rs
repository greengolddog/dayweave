use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::{Date, Duration, Month, OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::{
    AbsoluteWindow, ConstraintStrength, DayOfWeek, Dependency, DependencyRelation, ItemId,
    ItemKind, Minutes, Occurrence, OccurrenceId, OccurrenceState, PlanRequest, PreviousAssignment,
    Recurrence, RecurrenceExceptionAction, RecurrenceExceptionSelector,
    RecurrenceOccurrenceIdentity, RecurrencePeriod, RecurrenceSemantics, WorkItem,
    ZonedDayBoundary,
    custom_recurrence::{CustomRecurrenceRuleError, parse_custom_rrule},
};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecurrenceError {
    #[error("recurrence for item {item_id} has an invalid value: {message}")]
    InvalidRule { item_id: ItemId, message: String },
    #[error("timezone day {date} has invalid or mismatched boundaries")]
    InvalidZonedDay { date: Date },
    #[error("timezone calendar contains duplicate local day {0}")]
    DuplicateZonedDay(Date),
    #[error("timezone calendar does not continuously cover the planning horizon")]
    IncompleteZonedCalendar,
    #[error("pause for item {0} must have a positive duration")]
    InvalidPause(ItemId),
    #[error("moved recurrence exception for item {0} must have a positive duration")]
    InvalidException(ItemId),
    #[error("moved recurrence exception for item {0} has invalid source identity")]
    InvalidMoveSource(ItemId),
    #[error("moved recurrence exception for item {0} crosses the planning horizon")]
    MoveCrossesHorizon(ItemId),
    #[error("occurrence id {0} is claimed by more than one recurrence")]
    DuplicateOccurrence(OccurrenceId),
    #[error("manual placement {0} does not bind to an exact materialized occurrence")]
    InvalidManualPlacement(Uuid),
    #[error("partial progress for occurrence {0} is invalid or does not bind to schedulable work")]
    InvalidPartialProgress(OccurrenceId),
    #[error(
        "dependency from item {successor_id} to item {predecessor_id} crosses a materialized recurring subtree boundary"
    )]
    CrossRecurringSubtreeDependency {
        successor_id: ItemId,
        predecessor_id: ItemId,
    },
    #[error("calendar date arithmetic exceeded supported range")]
    DateOutOfRange,
}

/// Expands recurrence definitions into stable, bounded occurrences without
/// invoking the scheduler.
///
/// # Errors
///
/// Returns [`RecurrenceError`] for malformed frequency rules, timezone day
/// boundaries, pauses, exceptions, or out-of-range calendar arithmetic.
pub fn expand_occurrences(request: &PlanRequest) -> Result<Vec<Occurrence>, RecurrenceError> {
    validate_recurrence_context(request)?;
    let days = resolved_days(request)?;
    let by_id: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
    let recurring_ids: BTreeSet<_> = request
        .items
        .iter()
        .filter_map(|item| recurrence_of(item).map(|_| item.id))
        .collect();

    let roots: Vec<_> = request
        .items
        .iter()
        .filter(|item| {
            recurrence_of(item).is_some() && !has_recurring_ancestor(item, &by_id, &recurring_ids)
        })
        .collect();
    let root_ids: BTreeSet<_> = roots.iter().map(|item| item.id).collect();
    if let Some(exception) = request
        .recurrence_context
        .exceptions
        .iter()
        .find(|exception| {
            matches!(exception.action, RecurrenceExceptionAction::Move { .. })
                && !root_ids.contains(&exception.item_id)
        })
    {
        return Err(RecurrenceError::InvalidException(exception.item_id));
    }

    let mut result = Vec::new();
    for item in roots {
        let Some(recurrence) = recurrence_of(item) else {
            continue;
        };
        let mut occurrences = expand_series(request, item, recurrence, &days)?;
        append_moved_occurrences_for_horizon(request, item, recurrence, &mut occurrences)?;
        apply_pauses_and_exceptions(request, item.id, &mut occurrences);
        result.extend(occurrences);
    }
    let mut occurrence_owners = BTreeMap::new();
    let mut occurrence_states = BTreeMap::new();
    for occurrence in &result {
        if occurrence_owners
            .insert(occurrence.id, occurrence.series_item_id)
            .is_some()
        {
            return Err(RecurrenceError::DuplicateOccurrence(occurrence.id));
        }
        occurrence_states.insert(occurrence.id, occurrence.state);
    }
    for (occurrence_id, progress) in &request.recurrence_context.partial_progress {
        let valid_shape = (1..crate::HABIT_BASIS_POINTS_SCALE)
            .contains(&progress.progress_basis_points)
            && !progress.expected_duration_minutes.is_zero()
            && progress.expected_duration_minutes.get() <= crate::MAX_DEPENDENCY_LAG_MINUTES
            && progress.remaining_duration_minutes.is_none_or(|remaining| {
                !remaining.is_zero()
                    && remaining <= progress.expected_duration_minutes
                    && remaining.get() <= crate::MAX_DEPENDENCY_LAG_MINUTES
            });
        let valid_occurrence = occurrence_states
            .get(occurrence_id)
            .zip(occurrence_owners.get(occurrence_id))
            .is_some_and(|(state, owner)| {
                matches!(state, OccurrenceState::Generated | OccurrenceState::Paused)
                    && by_id
                        .get(owner)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Habit(_)))
            });
        if !valid_shape || !valid_occurrence {
            return Err(RecurrenceError::InvalidPartialProgress(*occurrence_id));
        }
    }
    result.sort_by_key(|occurrence| {
        (
            occurrence.nominal_start,
            occurrence.series_item_id,
            occurrence.ordinal,
            occurrence.id,
        )
    });
    Ok(result)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MaterializedIdentity {
    pub series_item_id: ItemId,
    pub occurrence_id: OccurrenceId,
}

pub(crate) struct MaterializedPlan {
    pub request: PlanRequest,
    pub occurrences: Vec<Occurrence>,
    pub identities: BTreeMap<ItemId, MaterializedIdentity>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn materialize_recurrences(
    request: &PlanRequest,
) -> Result<MaterializedPlan, RecurrenceError> {
    let occurrences = expand_occurrences(request)?;
    let by_id: BTreeMap<_, _> = request.items.iter().map(|item| (item.id, item)).collect();
    let children = children_by_parent(&request.items);
    let recurring_ids: BTreeSet<_> = request
        .items
        .iter()
        .filter_map(|item| recurrence_of(item).map(|_| item.id))
        .collect();
    let roots: Vec<_> = request
        .items
        .iter()
        .filter(|item| {
            recurrence_of(item).is_some() && !has_recurring_ancestor(item, &by_id, &recurring_ids)
        })
        .map(|item| item.id)
        .collect();

    let mut removed = BTreeSet::new();
    let mut subtrees = BTreeMap::new();
    for root in &roots {
        let subtree = collect_subtree(*root, &children);
        removed.extend(subtree.iter().copied());
        subtrees.insert(*root, subtree);
    }
    validate_recurring_subtree_dependencies(request, &subtrees)?;

    let mut items: Vec<WorkItem> = request
        .items
        .iter()
        .filter(|item| !removed.contains(&item.id))
        .cloned()
        .collect();
    let mut identities = BTreeMap::new();
    let mut previous_first_leaf = BTreeMap::<ItemId, ItemId>::new();

    for root in roots {
        let subtree = &subtrees[&root];
        let root_occurrences: Vec<_> = occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.series_item_id == root && occurrence.state == OccurrenceState::Generated
            })
            .collect();
        for occurrence in root_occurrences {
            let clone_ids: BTreeMap<_, _> = subtree
                .iter()
                .map(|original_id| {
                    let clone_id = if *original_id == root {
                        ItemId(occurrence.id.0)
                    } else {
                        ItemId(Uuid::new_v5(&original_id.0, occurrence.id.0.as_bytes()))
                    };
                    (*original_id, clone_id)
                })
                .collect();

            let leaves = subtree_leaves(subtree, &children, &by_id);
            let first_leaf = leaves.first().map(|id| clone_ids[id]);
            let spacing = minimum_spacing(request, root);

            for original_id in subtree {
                let original = by_id[original_id];
                let mut clone = original.clone();
                clone.id = clone_ids[original_id];
                clone.parent_id = original
                    .parent_id
                    .map(|parent| clone_ids.get(&parent).copied().unwrap_or(parent));
                for dependency in &mut clone.constraints.dependencies {
                    if let Some(rewritten) = clone_ids.get(&dependency.item_id) {
                        dependency.item_id = *rewritten;
                    }
                }
                clone.constraints.occurrence_window = Some(AbsoluteWindow {
                    start: occurrence.window_start,
                    end: occurrence.window_end,
                });
                if let Some(progress) = request
                    .recurrence_context
                    .partial_progress
                    .get(&occurrence.id)
                    .filter(|_| *original_id == root)
                {
                    let remaining = partial_remaining_minutes(occurrence.id, *progress)?;
                    let source = clone
                        .duration
                        .map_or(crate::EstimateSource::User, |duration| duration.source);
                    clone.duration = Some(crate::DurationEstimate {
                        minimum: progress.expected_duration_minutes,
                        expected: progress.expected_duration_minutes,
                        maximum: progress.expected_duration_minutes,
                        remaining: Some(remaining),
                        source,
                    });
                } else if let Some(duration) = &mut clone.duration {
                    duration.remaining = None;
                }
                if !spacing.is_zero()
                    && Some(clone.id) == first_leaf
                    && let Some(predecessor) = previous_first_leaf.get(&root)
                {
                    clone.constraints.dependencies.push(Dependency {
                        item_id: *predecessor,
                        relation: DependencyRelation::StartToStart,
                        minimum_lag: spacing,
                        strength: ConstraintStrength::Hard,
                    });
                }
                identities.insert(
                    clone.id,
                    MaterializedIdentity {
                        series_item_id: *original_id,
                        occurrence_id: occurrence.id,
                    },
                );
                items.push(clone);
            }
            if let Some(first_leaf) = first_leaf {
                previous_first_leaf.insert(root, first_leaf);
            }
        }
    }
    items.sort_by_key(|item| item.id);

    let mut materialized_request = request.clone();
    materialized_request.items = items;
    materialized_request.previous_assignments =
        materialize_previous_assignments(request, &occurrences, &identities, &removed);
    let requested_manual_counts = manual_placement_counts(&request.previous_assignments);
    let materialized_manual_counts =
        manual_placement_counts(&materialized_request.previous_assignments);
    if let Some(placement_id) = requested_manual_counts
        .iter()
        .find_map(|(placement_id, count)| {
            (materialized_manual_counts.get(placement_id) != Some(count)).then_some(*placement_id)
        })
    {
        return Err(RecurrenceError::InvalidManualPlacement(placement_id));
    }
    Ok(MaterializedPlan {
        request: materialized_request,
        occurrences,
        identities,
    })
}

fn partial_remaining_minutes(
    occurrence_id: OccurrenceId,
    progress: crate::RecurrencePartialProgress,
) -> Result<Minutes, RecurrenceError> {
    if let Some(remaining) = progress.remaining_duration_minutes {
        return (!remaining.is_zero() && remaining <= progress.expected_duration_minutes)
            .then_some(remaining)
            .ok_or(RecurrenceError::InvalidPartialProgress(occurrence_id));
    }
    let incomplete = u64::from(crate::HABIT_BASIS_POINTS_SCALE - progress.progress_basis_points);
    let numerator = u64::from(progress.expected_duration_minutes.get())
        .checked_mul(incomplete)
        .and_then(|value| value.checked_add(u64::from(crate::HABIT_BASIS_POINTS_SCALE - 1)))
        .ok_or(RecurrenceError::InvalidPartialProgress(occurrence_id))?;
    let remaining = numerator / u64::from(crate::HABIT_BASIS_POINTS_SCALE);
    let remaining = u32::try_from(remaining)
        .ok()
        .filter(|value| *value > 0 && *value <= progress.expected_duration_minutes.get())
        .map(Minutes)
        .ok_or(RecurrenceError::InvalidPartialProgress(occurrence_id))?;
    Ok(remaining)
}

fn validate_recurring_subtree_dependencies(
    request: &PlanRequest,
    subtrees: &BTreeMap<ItemId, Vec<ItemId>>,
) -> Result<(), RecurrenceError> {
    let recurring_owner = subtrees
        .iter()
        .flat_map(|(root_id, subtree)| subtree.iter().map(move |item_id| (*item_id, *root_id)))
        .collect::<BTreeMap<_, _>>();

    for successor in &request.items {
        for dependency in &successor.constraints.dependencies {
            let Some(predecessor_owner) = recurring_owner.get(&dependency.item_id) else {
                continue;
            };
            if recurring_owner.get(&successor.id) != Some(predecessor_owner) {
                return Err(RecurrenceError::CrossRecurringSubtreeDependency {
                    successor_id: successor.id,
                    predecessor_id: dependency.item_id,
                });
            }
        }
    }
    Ok(())
}

fn manual_placement_counts(assignments: &[PreviousAssignment]) -> BTreeMap<Uuid, (usize, usize)> {
    let mut result = BTreeMap::new();
    for assignment in assignments {
        let Some(placement_id) = assignment.manual_placement_id else {
            continue;
        };
        let entry = result.entry(placement_id).or_insert((0_usize, 0_usize));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(assignment.blocks.len());
    }
    result
}

fn recurrence_of(item: &WorkItem) -> Option<&Recurrence> {
    match &item.kind {
        ItemKind::RecurringTask(spec) => Some(&spec.recurrence),
        ItemKind::Habit(spec) => Some(&spec.recurrence),
        ItemKind::Routine(spec) => spec.recurrence.as_ref(),
        ItemKind::Task
        | ItemKind::Project
        | ItemKind::Goal(_)
        | ItemKind::Break(_)
        | ItemKind::CalendarEvent(_) => None,
    }
}

fn has_recurring_ancestor(
    item: &WorkItem,
    by_id: &BTreeMap<ItemId, &WorkItem>,
    recurring_ids: &BTreeSet<ItemId>,
) -> bool {
    let mut parent = item.parent_id;
    while let Some(id) = parent {
        if recurring_ids.contains(&id) {
            return true;
        }
        parent = by_id.get(&id).and_then(|value| value.parent_id);
    }
    false
}

fn validate_recurrence_context(request: &PlanRequest) -> Result<(), RecurrenceError> {
    validate_zoned_calendar(request)?;
    for pause in &request.recurrence_context.pauses {
        if pause.start >= pause.end {
            return Err(RecurrenceError::InvalidPause(pause.item_id));
        }
    }
    if let Some(item_id) =
        request
            .recurrence_context
            .minimum_spacing
            .iter()
            .find_map(|(item_id, spacing)| {
                (spacing.get() > crate::MAX_DEPENDENCY_LAG_MINUTES).then_some(*item_id)
            })
    {
        return Err(RecurrenceError::InvalidRule {
            item_id,
            message: format!(
                "minimum spacing must be at most {} minutes",
                crate::MAX_DEPENDENCY_LAG_MINUTES
            ),
        });
    }
    for exception in &request.recurrence_context.exceptions {
        if let RecurrenceExceptionAction::Move { start, end, source } = exception.action {
            if start >= end {
                return Err(RecurrenceError::InvalidException(exception.item_id));
            }
            if !matches!(
                exception.selector,
                RecurrenceExceptionSelector::Occurrence { .. }
            ) {
                return Err(RecurrenceError::InvalidMoveSource(exception.item_id));
            }
            if source.item_revision == 0
                || source.nominal_start >= source.nominal_end
                || source
                    .local_date
                    .is_some_and(|date| date != source.nominal_start.date())
            {
                return Err(RecurrenceError::InvalidMoveSource(exception.item_id));
            }
        }
    }
    validate_item_recurrences(request)
}

fn validate_zoned_calendar(request: &PlanRequest) -> Result<(), RecurrenceError> {
    let mut dates = BTreeSet::new();
    for day in &request.recurrence_context.calendar.days {
        let Some(last_instant) = day.end.checked_sub(Duration::nanoseconds(1)) else {
            return Err(RecurrenceError::InvalidZonedDay {
                date: day.local_date,
            });
        };
        if day.start >= day.end
            || day.start.date() != day.local_date
            || last_instant.date() != day.local_date
        {
            return Err(RecurrenceError::InvalidZonedDay {
                date: day.local_date,
            });
        }
        if !dates.insert(day.local_date) {
            return Err(RecurrenceError::DuplicateZonedDay(day.local_date));
        }
    }
    if !request.recurrence_context.calendar.days.is_empty() {
        let mut days = request.recurrence_context.calendar.days.clone();
        days.sort_by_key(|day| day.local_date);
        if days
            .first()
            .is_none_or(|day| day.start > request.horizon_start)
            || days.last().is_none_or(|day| day.end < request.horizon_end)
            || days.windows(2).any(|pair| {
                pair[0].local_date.next_day() != Some(pair[1].local_date)
                    || pair[0].end != pair[1].start
            })
        {
            return Err(RecurrenceError::IncompleteZonedCalendar);
        }
    }
    Ok(())
}

fn validate_item_recurrences(request: &PlanRequest) -> Result<(), RecurrenceError> {
    for item in &request.items {
        if let ItemKind::Habit(spec) = &item.kind
            && spec.minimum_spacing.get() > crate::MAX_DEPENDENCY_LAG_MINUTES
        {
            return Err(RecurrenceError::InvalidRule {
                item_id: item.id,
                message: format!(
                    "habit minimum spacing must be at most {} minutes",
                    crate::MAX_DEPENDENCY_LAG_MINUTES
                ),
            });
        }
        if let Some(recurrence) = recurrence_of(item) {
            validate_rule(item.id, recurrence)?;
            if let Recurrence::Custom { rrule } = recurrence {
                parse_custom_rrule(rrule)
                    .and_then(|rule| {
                        rule.all_occurrence_dates(
                            item.created_at.date(),
                            request.recurrence_context.calendar.week_starts_on,
                        )
                    })
                    .map(|_| ())
                    .map_err(|error| custom_rule_error(item.id, &error))?;
            }
        }
    }
    Ok(())
}

fn validate_rule(item_id: ItemId, recurrence: &Recurrence) -> Result<(), RecurrenceError> {
    let invalid = |message: &str| RecurrenceError::InvalidRule {
        item_id,
        message: message.to_owned(),
    };
    match recurrence {
        Recurrence::Daily { times_per_day } if *times_per_day == 0 => {
            Err(invalid("times_per_day must be greater than zero"))
        }
        Recurrence::Weekly { times_per_week, .. } if *times_per_week == 0 => {
            Err(invalid("times_per_week must be greater than zero"))
        }
        Recurrence::Monthly { times_per_month } if *times_per_month == 0 => {
            Err(invalid("times_per_month must be greater than zero"))
        }
        Recurrence::EveryInterval { interval } | Recurrence::AfterCompletion { interval }
            if interval.is_zero() =>
        {
            Err(invalid("interval must be greater than zero"))
        }
        Recurrence::Frequency { target, .. } if *target == 0 => {
            Err(invalid("frequency target must be greater than zero"))
        }
        Recurrence::Frequency {
            target,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Rolling,
            ..
        } if u32::from(*target) > 24 * 60 => {
            Err(invalid("rolling daily target exceeds minute precision"))
        }
        Recurrence::Frequency {
            target,
            period: RecurrencePeriod::Week,
            semantics: RecurrenceSemantics::Rolling,
            ..
        } if u32::from(*target) > 7 * 24 * 60 => {
            Err(invalid("rolling weekly target exceeds minute precision"))
        }
        Recurrence::Custom { rrule } => parse_custom_rrule(rrule)
            .map(|_| ())
            .map_err(|error| custom_rule_error(item_id, &error)),
        _ => Ok(()),
    }
}

fn resolved_days(request: &PlanRequest) -> Result<Vec<ZonedDayBoundary>, RecurrenceError> {
    if !request.recurrence_context.calendar.days.is_empty() {
        let mut days = request.recurrence_context.calendar.days.clone();
        days.sort_by_key(|day| day.local_date);
        return Ok(days);
    }

    let offset = request.horizon_start.offset();
    let mut date = request.horizon_start.to_offset(offset).date();
    let mut result = Vec::new();
    loop {
        let next = date.next_day().ok_or(RecurrenceError::DateOutOfRange)?;
        let start = midnight(date, offset)?;
        let end = midnight(next, offset)?;
        if start >= request.horizon_end {
            break;
        }
        if end > request.horizon_start {
            result.push(ZonedDayBoundary {
                local_date: date,
                start,
                end,
            });
        }
        date = next;
    }
    Ok(result)
}

fn midnight(date: Date, offset: UtcOffset) -> Result<OffsetDateTime, RecurrenceError> {
    date.with_hms(0, 0, 0)
        .map(|value| value.assume_offset(offset))
        .map_err(|_| RecurrenceError::DateOutOfRange)
}

#[allow(clippy::too_many_lines)]
fn expand_series(
    request: &PlanRequest,
    item: &WorkItem,
    recurrence: &Recurrence,
    days: &[ZonedDayBoundary],
) -> Result<Vec<Occurrence>, RecurrenceError> {
    let week_start = request.recurrence_context.calendar.week_starts_on;
    let spacing = minimum_spacing(request, item.id);
    match recurrence {
        Recurrence::Daily { times_per_day } => Ok(expand_calendar_buckets(
            item.id,
            days.iter()
                .map(|day| (day.local_date.to_string(), vec![*day])),
            *times_per_day,
            spacing,
            request,
            "daily",
        )),
        Recurrence::Weekly {
            times_per_week,
            weekdays,
        } => {
            let groups = complete_week_groups(request, days, week_start, weekdays)?;
            Ok(expand_calendar_buckets(
                item.id,
                groups
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value)),
                *times_per_week,
                spacing,
                request,
                "weekly",
            ))
        }
        Recurrence::Monthly { times_per_month } => expand_monthly(
            request,
            item.id,
            days,
            *times_per_month,
            &BTreeSet::new(),
            spacing,
            "monthly",
        ),
        Recurrence::EveryInterval { interval } => expand_rolling_minutes(
            request,
            item,
            request
                .recurrence_context
                .rolling_anchors
                .get(&item.id)
                .copied()
                .unwrap_or(item.created_at),
            interval.get(),
            "interval",
        ),
        Recurrence::AfterCompletion { interval } => {
            expand_after_completion(request, item, *interval)
        }
        Recurrence::Frequency {
            target,
            period,
            semantics,
            weekdays,
            minimum_spacing: _,
            anchor,
        } => match semantics {
            RecurrenceSemantics::Calendar => match period {
                RecurrencePeriod::Day => Ok(expand_calendar_buckets(
                    item.id,
                    days.iter()
                        .filter(|day| {
                            weekdays.is_empty()
                                || weekdays
                                    .contains(&DayOfWeek::from_time(day.local_date.weekday()))
                        })
                        .map(|day| (day.local_date.to_string(), vec![*day])),
                    *target,
                    spacing,
                    request,
                    "frequency-calendar-day",
                )),
                RecurrencePeriod::Week => {
                    let groups = complete_week_groups(request, days, week_start, weekdays)?;
                    Ok(expand_calendar_buckets(
                        item.id,
                        groups
                            .into_iter()
                            .map(|(key, value)| (key.to_string(), value)),
                        *target,
                        spacing,
                        request,
                        "frequency-calendar-week",
                    ))
                }
                RecurrencePeriod::Month => expand_monthly(
                    request,
                    item.id,
                    days,
                    *target,
                    weekdays,
                    spacing,
                    "frequency-calendar-month",
                ),
            },
            RecurrenceSemantics::Rolling => {
                let anchor = anchor
                    .or_else(|| {
                        request
                            .recurrence_context
                            .rolling_anchors
                            .get(&item.id)
                            .copied()
                    })
                    .unwrap_or(item.created_at);
                match period {
                    RecurrencePeriod::Day => expand_rolling_frequency(
                        request,
                        item,
                        anchor,
                        *target,
                        24 * 60,
                        "frequency-rolling-day",
                    ),
                    RecurrencePeriod::Week => expand_rolling_frequency(
                        request,
                        item,
                        anchor,
                        *target,
                        7 * 24 * 60,
                        "frequency-rolling-week",
                    ),
                    RecurrencePeriod::Month => {
                        expand_rolling_months(request, item, anchor, *target, days)
                    }
                }
            }
        },
        Recurrence::Custom { rrule } => expand_custom(request, item, rrule, days),
    }
}

fn custom_rule_error(item_id: ItemId, error: &CustomRecurrenceRuleError) -> RecurrenceError {
    RecurrenceError::InvalidRule {
        item_id,
        message: error.to_string(),
    }
}

fn expand_custom(
    request: &PlanRequest,
    item: &WorkItem,
    rrule: &str,
    days: &[ZonedDayBoundary],
) -> Result<Vec<Occurrence>, RecurrenceError> {
    let parsed_rule =
        parse_custom_rrule(rrule).map_err(|error| custom_rule_error(item.id, &error))?;
    let Some(through) = days.last().map(|day| day.local_date) else {
        return Ok(Vec::new());
    };
    let anchor = item.created_at.date();
    let dates = parsed_rule
        .occurrence_dates_through(
            anchor,
            through,
            request.recurrence_context.calendar.week_starts_on,
        )
        .map_err(|error| custom_rule_error(item.id, &error))?;
    let rule_id = parsed_rule.rule_id();
    let mut result = Vec::new();
    for (sequence, date) in dates {
        let Some(day) = days.iter().find(|day| day.local_date == date) else {
            continue;
        };
        let nominal_start = if date == anchor {
            day.start.max(item.created_at)
        } else {
            day.start
        };
        if nominal_start >= day.end {
            continue;
        }
        let identity = RecurrenceOccurrenceIdentity::CustomRule {
            rule_id,
            sequence,
            date,
        };
        let key = custom_identity_key(rule_id, sequence, date);
        let occurrence = make_occurrence(
            item.id,
            key.as_bytes(),
            identity,
            (nominal_start, day.end),
            (
                nominal_start.max(request.horizon_start),
                day.end.min(request.horizon_end),
            ),
            Some(date),
        );
        if occurrence.window_start < occurrence.window_end {
            result.push(occurrence);
        }
    }
    Ok(result)
}

fn custom_identity_key(rule_id: Uuid, sequence: u32, date: Date) -> String {
    format!("custom:{rule_id}:{sequence}:{date}")
}

fn expand_monthly(
    request: &PlanRequest,
    item_id: ItemId,
    days: &[ZonedDayBoundary],
    target: u16,
    weekdays: &BTreeSet<DayOfWeek>,
    spacing: Minutes,
    label: &str,
) -> Result<Vec<Occurrence>, RecurrenceError> {
    let mut months = BTreeSet::new();
    for day in days {
        months.insert((day.local_date.year(), u8::from(day.local_date.month())));
    }
    let mut groups = Vec::new();
    for (year, month) in months {
        let month_value = Month::try_from(month).map_err(|_| RecurrenceError::DateOutOfRange)?;
        let mut values = Vec::new();
        for day in 1..=month_value.length(year) {
            let date = Date::from_calendar_date(year, month_value, day)
                .map_err(|_| RecurrenceError::DateOutOfRange)?;
            if weekdays.is_empty() || weekdays.contains(&DayOfWeek::from_time(date.weekday())) {
                values.push(resolved_boundary(date, days, request));
            }
        }
        groups.push((format!("{year:04}-{month:02}"), values));
    }
    Ok(expand_calendar_buckets(
        item_id, groups, target, spacing, request, label,
    ))
}

fn complete_week_groups(
    request: &PlanRequest,
    days: &[ZonedDayBoundary],
    starts_on: DayOfWeek,
    weekdays: &BTreeSet<DayOfWeek>,
) -> Result<BTreeMap<i32, Vec<ZonedDayBoundary>>, RecurrenceError> {
    let keys = days
        .iter()
        .map(|day| week_key(day.local_date, starts_on))
        .collect::<BTreeSet<_>>();
    let mut groups = BTreeMap::new();
    for key in keys {
        let start = Date::from_julian_day(key).map_err(|_| RecurrenceError::DateOutOfRange)?;
        let mut values = Vec::new();
        for offset in 0..7 {
            let date = start
                .checked_add(Duration::days(offset))
                .ok_or(RecurrenceError::DateOutOfRange)?;
            if weekdays.is_empty() || weekdays.contains(&DayOfWeek::from_time(date.weekday())) {
                values.push(resolved_boundary(date, days, request));
            }
        }
        groups.insert(key, values);
    }
    Ok(groups)
}

fn resolved_boundary(
    date: Date,
    days: &[ZonedDayBoundary],
    request: &PlanRequest,
) -> ZonedDayBoundary {
    if let Some(day) = days.iter().find(|day| day.local_date == date) {
        return *day;
    }
    // Full calendar buckets are needed to choose a stable weekday, but an out-of-horizon date
    // has no authoritative timezone boundary in this request. Represent it as an empty sentinel
    // at the nearest horizon edge so it participates in allocation without materializing work.
    let instant = days.first().map_or(request.horizon_start, |first| {
        if date < first.local_date {
            request.horizon_start
        } else {
            request.horizon_end
        }
    });
    ZonedDayBoundary {
        local_date: date,
        start: instant,
        end: instant,
    }
}

fn expand_calendar_buckets<I>(
    item_id: ItemId,
    buckets: I,
    target: u16,
    minimum_spacing: Minutes,
    request: &PlanRequest,
    label: &str,
) -> Vec<Occurrence>
where
    I: IntoIterator<Item = (String, Vec<ZonedDayBoundary>)>,
{
    let mut result = Vec::new();
    for (bucket_key, mut eligible_days) in buckets {
        eligible_days.sort_by_key(|day| day.local_date);
        if eligible_days.is_empty() {
            continue;
        }
        let mut allocations: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
        for index in 0..target {
            let day_index = usize::from(index) * eligible_days.len() / usize::from(target);
            allocations.entry(day_index).or_default().push(index);
        }
        for (day_index, indexes) in allocations {
            let day = eligible_days[day_index];
            let count = indexes.len();
            let day_duration = day.end - day.start;
            for (position, bucket_ordinal) in indexes.into_iter().enumerate() {
                let numerator = i32::try_from(position).unwrap_or(i32::MAX);
                let denominator = i32::try_from(count).unwrap_or(i32::MAX);
                let start = day_duration
                    .checked_mul(numerator)
                    .and_then(|value| value.checked_div(denominator))
                    .and_then(|value| day.start.checked_add(value))
                    .unwrap_or(day.end);
                let end = if position + 1 == count {
                    // Preserve the authoritative post-transition offset at the end of a DST day.
                    day.end
                } else {
                    let numerator = i32::try_from(position + 1).unwrap_or(i32::MAX);
                    day_duration
                        .checked_mul(numerator)
                        .and_then(|value| value.checked_div(denominator))
                        .and_then(|value| day.start.checked_add(value))
                        .unwrap_or(day.end)
                };
                let spacing_end = start
                    .checked_add(Duration::minutes(i64::from(minimum_spacing.get())))
                    // An overflowing positive spacing value is necessarily beyond this bounded
                    // calendar day, so clipping it to the day end preserves the intended result.
                    .map_or(day.end, |value| value.min(day.end));
                let mut effective_end = end.max(spacing_end);
                if effective_end == day.end {
                    effective_end = day.end;
                }
                let key = format!("{label}:{bucket_key}:{bucket_ordinal}");
                let identity = match label {
                    "daily" | "frequency-calendar-day" => {
                        RecurrenceOccurrenceIdentity::CalendarDay {
                            date: day.local_date,
                            bucket_ordinal,
                        }
                    }
                    "weekly" | "frequency-calendar-week" => {
                        RecurrenceOccurrenceIdentity::CalendarWeek {
                            week_key: bucket_key
                                .parse()
                                .expect("weekly bucket keys are generated integers"),
                            bucket_ordinal,
                        }
                    }
                    "monthly" | "frequency-calendar-month" => {
                        RecurrenceOccurrenceIdentity::CalendarMonth {
                            year: day.local_date.year(),
                            month: u8::from(day.local_date.month()),
                            bucket_ordinal,
                        }
                    }
                    _ => unreachable!("calendar bucket labels are internal constants"),
                };
                let occurrence = make_occurrence(
                    item_id,
                    key.as_bytes(),
                    identity,
                    (start, effective_end),
                    (
                        start.max(request.horizon_start),
                        effective_end.min(request.horizon_end),
                    ),
                    Some(day.local_date),
                );
                if occurrence.window_start < occurrence.window_end {
                    result.push(occurrence);
                }
            }
        }
    }
    result
}

fn expand_rolling_minutes(
    request: &PlanRequest,
    item: &WorkItem,
    anchor: OffsetDateTime,
    interval_minutes: u32,
    label: &str,
) -> Result<Vec<Occurrence>, RecurrenceError> {
    if interval_minutes == 0 {
        return Ok(Vec::new());
    }
    let interval = Duration::minutes(i64::from(interval_minutes));
    let elapsed = request.horizon_start - anchor;
    let mut index = elapsed
        .whole_minutes()
        .div_euclid(i64::from(interval_minutes));
    index = index.max(0);
    loop {
        let next_index = index
            .checked_add(1)
            .ok_or(RecurrenceError::DateOutOfRange)?;
        if rolling_instant(anchor, interval, next_index)? > request.horizon_start {
            break;
        }
        index = next_index;
    }
    let mut result = Vec::new();
    loop {
        let start = rolling_instant(anchor, interval, index)?;
        if start >= request.horizon_end {
            break;
        }
        if u32::try_from(index).is_err() {
            return Err(RecurrenceError::DateOutOfRange);
        }
        let nominal_end = start
            .checked_add(interval)
            .ok_or(RecurrenceError::DateOutOfRange)?;
        let end = nominal_end.min(request.horizon_end);
        let key = format!(
            "{label}:{}:{interval_minutes}:{index}",
            anchor.unix_timestamp_nanos()
        );
        result.push(make_occurrence(
            item.id,
            key.as_bytes(),
            RecurrenceOccurrenceIdentity::RollingMinutes { index, anchor },
            (start, nominal_end),
            (start.max(request.horizon_start), end),
            None,
        ));
        index = index
            .checked_add(1)
            .ok_or(RecurrenceError::DateOutOfRange)?;
    }
    Ok(result)
}

fn rolling_instant(
    anchor: OffsetDateTime,
    interval: Duration,
    index: i64,
) -> Result<OffsetDateTime, RecurrenceError> {
    let index = i32::try_from(index).map_err(|_| RecurrenceError::DateOutOfRange)?;
    let elapsed = interval
        .checked_mul(index)
        .ok_or(RecurrenceError::DateOutOfRange)?;
    anchor
        .checked_add(elapsed)
        .ok_or(RecurrenceError::DateOutOfRange)
}

fn expand_rolling_frequency(
    request: &PlanRequest,
    item: &WorkItem,
    anchor: OffsetDateTime,
    target: u16,
    period_minutes: u32,
    label: &str,
) -> Result<Vec<Occurrence>, RecurrenceError> {
    if target == 0 || u32::from(target) > period_minutes {
        return Ok(Vec::new());
    }
    let elapsed_minutes = (request.horizon_start - anchor).whole_minutes().max(0);
    let estimated_index = i128::from(elapsed_minutes)
        .checked_mul(i128::from(target))
        .and_then(|value| value.checked_div(i128::from(period_minutes)))
        .ok_or(RecurrenceError::DateOutOfRange)?;
    let mut index = i64::try_from(estimated_index).map_err(|_| RecurrenceError::DateOutOfRange)?;
    while index > 0
        && rolling_frequency_instant(anchor, target, period_minutes, index)? > request.horizon_start
    {
        index -= 1;
    }
    loop {
        let next = index
            .checked_add(1)
            .ok_or(RecurrenceError::DateOutOfRange)?;
        if rolling_frequency_instant(anchor, target, period_minutes, next)? > request.horizon_start
        {
            break;
        }
        index = next;
    }

    let mut result = Vec::new();
    loop {
        let start = rolling_frequency_instant(anchor, target, period_minutes, index)?;
        if start >= request.horizon_end {
            break;
        }
        if u32::try_from(index).is_err() {
            return Err(RecurrenceError::DateOutOfRange);
        }
        let next = index
            .checked_add(1)
            .ok_or(RecurrenceError::DateOutOfRange)?;
        let nominal_end = rolling_frequency_instant(anchor, target, period_minutes, next)?;
        let key = format!(
            "{label}:{}:{target}:{period_minutes}:{index}",
            anchor.unix_timestamp_nanos()
        );
        result.push(make_occurrence(
            item.id,
            key.as_bytes(),
            RecurrenceOccurrenceIdentity::RollingMinutes { index, anchor },
            (start, nominal_end),
            (
                start.max(request.horizon_start),
                nominal_end.min(request.horizon_end),
            ),
            None,
        ));
        index = next;
    }
    Ok(result)
}

fn rolling_frequency_instant(
    anchor: OffsetDateTime,
    target: u16,
    period_minutes: u32,
    index: i64,
) -> Result<OffsetDateTime, RecurrenceError> {
    if index < 0 || target == 0 {
        return Err(RecurrenceError::DateOutOfRange);
    }
    let target = i128::from(target);
    let index = i128::from(index);
    let cycle = index / target;
    let slot = index % target;
    let offset = cycle
        .checked_mul(i128::from(period_minutes))
        .and_then(|value| {
            slot.checked_mul(i128::from(period_minutes))
                .and_then(|slot_value| slot_value.checked_div(target))
                .and_then(|slot_value| value.checked_add(slot_value))
        })
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(RecurrenceError::DateOutOfRange)?;
    anchor
        .checked_add(Duration::minutes(offset))
        .ok_or(RecurrenceError::DateOutOfRange)
}

fn expand_after_completion(
    request: &PlanRequest,
    item: &WorkItem,
    interval: Minutes,
) -> Result<Vec<Occurrence>, RecurrenceError> {
    let anchor = request
        .recurrence_context
        .completion_anchors
        .get(&item.id)
        .copied()
        .unwrap_or(item.created_at);
    let due = anchor
        .checked_add(Duration::minutes(i64::from(interval.get())))
        .ok_or(RecurrenceError::DateOutOfRange)?;
    let nominal_end = due
        .checked_add(Duration::minutes(i64::from(interval.get())))
        .ok_or(RecurrenceError::DateOutOfRange)?;
    if due >= request.horizon_end || nominal_end <= request.horizon_start {
        return Ok(Vec::new());
    }
    let start = due.max(request.horizon_start);
    let key = format!(
        "after-completion:{}:{}",
        anchor.unix_timestamp_nanos(),
        interval.get()
    );
    Ok(vec![make_occurrence(
        item.id,
        key.as_bytes(),
        RecurrenceOccurrenceIdentity::AfterCompletion { anchor },
        (due, nominal_end),
        (start, nominal_end.min(request.horizon_end)),
        None,
    )])
}

fn expand_rolling_months(
    request: &PlanRequest,
    item: &WorkItem,
    anchor: OffsetDateTime,
    target: u16,
    days: &[ZonedDayBoundary],
) -> Result<Vec<Occurrence>, RecurrenceError> {
    // The recurrence anchor carries its own offset, so its calendar date is stable even when the
    // current planning horizon does not contain the anchor's timezone boundary.
    let anchor_date = anchor.date();
    let first_date = days
        .first()
        .map_or(request.horizon_start.date(), |day| day.local_date);
    let anchor_month = month_index(anchor_date);
    let first_month = month_index(first_date);
    let mut cycle = i64::from(first_month - anchor_month - 1).max(0);
    let mut result = Vec::new();
    loop {
        let start_date = add_months(anchor_date, cycle)?;
        let next_cycle = cycle
            .checked_add(1)
            .ok_or(RecurrenceError::DateOutOfRange)?;
        let end_date = add_months(anchor_date, next_cycle)?;
        let cycle_start = boundary_instant(start_date, days, request.horizon_start.offset())?;
        let cycle_end = boundary_instant(end_date, days, request.horizon_start.offset())?;
        if cycle_start >= request.horizon_end {
            break;
        }
        if cycle_end > request.horizon_start {
            let duration = cycle_end - cycle_start;
            for index in 0..target {
                let start = if index == 0 {
                    cycle_start
                } else {
                    duration
                        .checked_mul(i32::from(index))
                        .and_then(|value| value.checked_div(i32::from(target)))
                        .and_then(|value| cycle_start.checked_add(value))
                        .ok_or(RecurrenceError::DateOutOfRange)?
                };
                let end = if index + 1 == target {
                    cycle_end
                } else {
                    duration
                        .checked_mul(i32::from(index + 1))
                        .and_then(|value| value.checked_div(i32::from(target)))
                        .and_then(|value| cycle_start.checked_add(value))
                        .ok_or(RecurrenceError::DateOutOfRange)?
                };
                if start < request.horizon_end && request.horizon_start < end {
                    let key = format!(
                        "frequency-rolling-month:{}:{target}:{cycle}:{index}",
                        anchor.unix_timestamp_nanos()
                    );
                    result.push(make_occurrence(
                        item.id,
                        key.as_bytes(),
                        RecurrenceOccurrenceIdentity::RollingMonth {
                            cycle,
                            index,
                            anchor,
                        },
                        (start, end),
                        (
                            start.max(request.horizon_start),
                            end.min(request.horizon_end),
                        ),
                        None,
                    ));
                }
            }
        }
        cycle = next_cycle;
    }
    Ok(result)
}

fn make_occurrence(
    item_id: ItemId,
    key: &[u8],
    identity: RecurrenceOccurrenceIdentity,
    nominal: (OffsetDateTime, OffsetDateTime),
    window: (OffsetDateTime, OffsetDateTime),
    local_date: Option<Date>,
) -> Occurrence {
    let id = OccurrenceId(Uuid::new_v5(&item_id.0, key));
    Occurrence {
        id,
        series_item_id: item_id,
        identity,
        nominal_start: nominal.0,
        nominal_end: nominal.1,
        window_start: window.0,
        window_end: window.1,
        local_date,
        ordinal: stable_ordinal(identity)
            .expect("generated recurrence identities have valid ordinals"),
        state: OccurrenceState::Generated,
    }
}

fn stable_ordinal(identity: RecurrenceOccurrenceIdentity) -> Option<u32> {
    match identity {
        RecurrenceOccurrenceIdentity::CalendarDay { bucket_ordinal, .. }
        | RecurrenceOccurrenceIdentity::CalendarWeek { bucket_ordinal, .. }
        | RecurrenceOccurrenceIdentity::CalendarMonth { bucket_ordinal, .. } => {
            Some(u32::from(bucket_ordinal))
        }
        RecurrenceOccurrenceIdentity::RollingMinutes { index, .. } => u32::try_from(index).ok(),
        RecurrenceOccurrenceIdentity::AfterCompletion { .. }
        | RecurrenceOccurrenceIdentity::Custom => Some(0),
        RecurrenceOccurrenceIdentity::RollingMonth { index, .. } => Some(u32::from(index)),
        RecurrenceOccurrenceIdentity::CustomRule { sequence, .. } => Some(sequence),
    }
}

fn apply_pauses_and_exceptions(
    request: &PlanRequest,
    item_id: ItemId,
    occurrences: &mut [Occurrence],
) {
    for occurrence in occurrences {
        if request
            .recurrence_context
            .completed_occurrence_ids
            .contains(&occurrence.id)
        {
            occurrence.state = OccurrenceState::Completed;
            continue;
        }
        for exception in request
            .recurrence_context
            .exceptions
            .iter()
            .filter(|exception| exception.item_id == item_id)
        {
            let matches = match exception.selector {
                RecurrenceExceptionSelector::Occurrence { id } => id == occurrence.id,
                RecurrenceExceptionSelector::LocalDate { date } => {
                    occurrence.local_date == Some(date)
                }
                RecurrenceExceptionSelector::NominalStart { at } => occurrence.nominal_start == at,
            };
            if !matches {
                continue;
            }
            match exception.action {
                RecurrenceExceptionAction::Skip => occurrence.state = OccurrenceState::Skipped,
                RecurrenceExceptionAction::Move {
                    start,
                    end,
                    source: _,
                } => {
                    // A moved occurrence belongs to exactly one planning horizon. Suppress its
                    // nominal placement until a request fully contains the destination window;
                    // otherwise adjacent rolling horizons could schedule partial duplicates.
                    if request.horizon_start <= start && end <= request.horizon_end {
                        occurrence.window_start = start;
                        occurrence.window_end = end;
                        occurrence.state = OccurrenceState::Generated;
                    } else {
                        occurrence.state = OccurrenceState::Paused;
                    }
                }
            }
        }
        // Pause eligibility is evaluated against the final moved window. A move
        // must never make habit work bypass an authoritative pause interval.
        if occurrence.state == OccurrenceState::Generated
            && request.recurrence_context.pauses.iter().any(|pause| {
                pause.item_id == item_id
                    && pause.start < occurrence.window_end
                    && occurrence.window_start < pause.end
            })
        {
            occurrence.state = OccurrenceState::Paused;
        }
    }
}

/// Restores an occurrence-ID move when its nominal occurrence has rolled out of the horizon.
///
/// Occurrence IDs are the durable identity used by completion and execution evidence. A move
/// exception already supplies that identity and its exact destination window, so the target-day
/// request can safely materialize the moved work without regenerating the old nominal bucket.
/// The synthetic nominal fields are presentation metadata only; materialization and subsequent
/// reconciliation continue to use the original occurrence ID.
#[allow(clippy::too_many_lines)]
fn append_moved_occurrences_for_horizon(
    request: &PlanRequest,
    item: &WorkItem,
    recurrence: &Recurrence,
    occurrences: &mut Vec<Occurrence>,
) -> Result<(), RecurrenceError> {
    let item_id = item.id;
    let has_custom_move = request
        .recurrence_context
        .exceptions
        .iter()
        .any(|exception| {
            exception.item_id == item_id
                && matches!(exception.action, RecurrenceExceptionAction::Move { .. })
        });
    let custom_proof = match recurrence {
        Recurrence::Custom { rrule } if has_custom_move => {
            let rule =
                parse_custom_rrule(rrule).map_err(|error| custom_rule_error(item.id, &error))?;
            let dates = rule
                .all_occurrence_dates(
                    item.created_at.date(),
                    request.recurrence_context.calendar.week_starts_on,
                )
                .map_err(|error| custom_rule_error(item.id, &error))?;
            Some(CustomMoveProof {
                rule_id: rule.rule_id(),
                dates,
            })
        }
        _ => None,
    };
    let mut moved_ids = BTreeSet::new();
    let mut occurrence_indices = BTreeMap::new();
    for (index, occurrence) in occurrences.iter().enumerate() {
        if occurrence_indices.insert(occurrence.id, index).is_some() {
            return Err(RecurrenceError::DuplicateOccurrence(occurrence.id));
        }
    }
    for exception in request
        .recurrence_context
        .exceptions
        .iter()
        .filter(|exception| exception.item_id == item_id)
    {
        let (
            RecurrenceExceptionSelector::Occurrence { id },
            RecurrenceExceptionAction::Move { start, end, source },
        ) = (exception.selector, exception.action)
        else {
            continue;
        };
        if !moved_ids.insert(id)
            || !move_source_is_valid(request, item, recurrence, id, source, custom_proof.as_ref())
        {
            return Err(RecurrenceError::InvalidMoveSource(item_id));
        }
        let existing_index = occurrence_indices.get(&id).copied();
        if let Some(index) = existing_index {
            let occurrence = &mut occurrences[index];
            let native_metadata_matches = match recurrence {
                Recurrence::Frequency {
                    period: RecurrencePeriod::Month,
                    semantics: RecurrenceSemantics::Rolling,
                    ..
                } => true,
                _ => {
                    occurrence.nominal_start == source.nominal_start
                        && occurrence.nominal_end == source.nominal_end
                }
            };
            if occurrence.identity != source.identity
                || occurrence.local_date != source.local_date
                || !native_metadata_matches
            {
                return Err(RecurrenceError::InvalidMoveSource(item_id));
            }
            occurrence.identity = source.identity;
            occurrence.nominal_start = source.nominal_start;
            occurrence.nominal_end = source.nominal_end;
            occurrence.local_date = source.local_date;
            occurrence.ordinal = source.ordinal;
        }
        let destination_is_contained = request.horizon_start <= start && end <= request.horizon_end;
        if !destination_is_contained && start < request.horizon_end && request.horizon_start < end {
            return Err(RecurrenceError::MoveCrossesHorizon(item_id));
        }
        if existing_index.is_none() && destination_is_contained {
            occurrence_indices.insert(id, occurrences.len());
            occurrences.push(Occurrence {
                id,
                series_item_id: item_id,
                identity: source.identity,
                nominal_start: source.nominal_start,
                nominal_end: source.nominal_end,
                window_start: start,
                window_end: end,
                local_date: source.local_date,
                ordinal: source.ordinal,
                state: OccurrenceState::Generated,
            });
        }
    }
    Ok(())
}

struct CustomMoveProof {
    rule_id: Uuid,
    dates: Vec<Date>,
}

fn move_source_is_valid(
    request: &PlanRequest,
    item: &WorkItem,
    recurrence: &Recurrence,
    id: OccurrenceId,
    source: crate::RecurrenceMoveSource,
    custom_proof: Option<&CustomMoveProof>,
) -> bool {
    if id.0.get_version_num() != 5
        || source.item_revision != item.revision
        || stable_ordinal(source.identity) != Some(source.ordinal)
    {
        return false;
    }
    let Some((key, expected_local_date)) =
        validated_identity_name(request, item, recurrence, source, custom_proof)
    else {
        return false;
    };
    id == OccurrenceId(Uuid::new_v5(&item.id.0, key.as_bytes()))
        && source.local_date == expected_local_date
}

#[allow(clippy::too_many_lines)]
fn validated_identity_name(
    request: &PlanRequest,
    item: &WorkItem,
    recurrence: &Recurrence,
    source: crate::RecurrenceMoveSource,
    custom_proof: Option<&CustomMoveProof>,
) -> Option<(String, Option<Date>)> {
    let calendar = &request.recurrence_context.calendar;
    let local_day_is_plausible = |date: Date| {
        source.local_date == Some(date)
            && source.nominal_start.date() == date
            && source
                .nominal_end
                .checked_sub(Duration::nanoseconds(1))
                .is_some_and(|value| value.date() == date)
    };
    match (recurrence, source.identity) {
        (
            Recurrence::Daily { times_per_day },
            RecurrenceOccurrenceIdentity::CalendarDay {
                date,
                bucket_ordinal,
            },
        ) if bucket_ordinal < *times_per_day
            && local_day_is_plausible(date)
            && calendar_source_matches(
                request,
                item.id,
                source,
                date,
                *times_per_day,
                1,
                bucket_ordinal,
            ) =>
        {
            Some((format!("daily:{date}:{bucket_ordinal}"), Some(date)))
        }
        (
            Recurrence::Weekly {
                times_per_week,
                weekdays,
            },
            RecurrenceOccurrenceIdentity::CalendarWeek {
                week_key: identity_week_key,
                bucket_ordinal,
            },
        ) => {
            let date = allocated_week_date(
                identity_week_key,
                calendar.week_starts_on,
                weekdays,
                *times_per_week,
                bucket_ordinal,
            )?;
            (local_day_is_plausible(date)
                && calendar_source_matches(
                    request,
                    item.id,
                    source,
                    date,
                    *times_per_week,
                    if weekdays.is_empty() {
                        7
                    } else {
                        weekdays.len()
                    },
                    bucket_ordinal,
                ))
            .then(|| {
                (
                    format!("weekly:{identity_week_key}:{bucket_ordinal}"),
                    Some(date),
                )
            })
        }
        (
            Recurrence::Monthly { times_per_month },
            RecurrenceOccurrenceIdentity::CalendarMonth {
                year,
                month,
                bucket_ordinal,
            },
        ) => {
            let date = allocated_month_date(
                year,
                month,
                &BTreeSet::new(),
                *times_per_month,
                bucket_ordinal,
            )?;
            let eligible_day_count = usize::from(Month::try_from(month).ok()?.length(year));
            (local_day_is_plausible(date)
                && calendar_source_matches(
                    request,
                    item.id,
                    source,
                    date,
                    *times_per_month,
                    eligible_day_count,
                    bucket_ordinal,
                ))
            .then(|| {
                (
                    format!("monthly:{year:04}-{month:02}:{bucket_ordinal}"),
                    Some(date),
                )
            })
        }
        (
            Recurrence::EveryInterval { interval },
            RecurrenceOccurrenceIdentity::RollingMinutes { index, anchor },
        ) => validated_rolling_minute_name(
            source,
            effective_rolling_anchor(request, item, None),
            anchor,
            interval.get(),
            index,
            "interval",
        ),
        (
            Recurrence::AfterCompletion { interval },
            RecurrenceOccurrenceIdentity::AfterCompletion { anchor },
        ) => {
            let effective_anchor = request
                .recurrence_context
                .completion_anchors
                .get(&item.id)
                .copied()
                .unwrap_or(item.created_at);
            let due = effective_anchor.checked_add(Duration::minutes(i64::from(interval.get())))?;
            let expected_end = due.checked_add(Duration::minutes(i64::from(interval.get())))?;
            (anchor == effective_anchor
                && anchor.offset() == effective_anchor.offset()
                && source.nominal_start == due
                && source.nominal_start.offset() == due.offset()
                && source.nominal_end == expected_end
                && source.nominal_end.offset() == expected_end.offset()
                && source.local_date.is_none())
            .then(|| {
                (
                    format!(
                        "after-completion:{}:{}",
                        anchor.unix_timestamp_nanos(),
                        interval.get()
                    ),
                    None,
                )
            })
        }
        (
            Recurrence::Frequency {
                target,
                period: RecurrencePeriod::Day,
                semantics: RecurrenceSemantics::Calendar,
                weekdays,
                ..
            },
            RecurrenceOccurrenceIdentity::CalendarDay {
                date,
                bucket_ordinal,
            },
        ) if bucket_ordinal < *target
            && (weekdays.is_empty()
                || weekdays.contains(&DayOfWeek::from_time(date.weekday())))
            && local_day_is_plausible(date)
            && calendar_source_matches(
                request,
                item.id,
                source,
                date,
                *target,
                1,
                bucket_ordinal,
            ) =>
        {
            Some((
                format!("frequency-calendar-day:{date}:{bucket_ordinal}"),
                Some(date),
            ))
        }
        (
            Recurrence::Frequency {
                target,
                period: RecurrencePeriod::Week,
                semantics: RecurrenceSemantics::Calendar,
                weekdays,
                ..
            },
            RecurrenceOccurrenceIdentity::CalendarWeek {
                week_key: identity_week_key,
                bucket_ordinal,
            },
        ) => {
            let date = allocated_week_date(
                identity_week_key,
                calendar.week_starts_on,
                weekdays,
                *target,
                bucket_ordinal,
            )?;
            (local_day_is_plausible(date)
                && calendar_source_matches(
                    request,
                    item.id,
                    source,
                    date,
                    *target,
                    if weekdays.is_empty() {
                        7
                    } else {
                        weekdays.len()
                    },
                    bucket_ordinal,
                ))
            .then(|| {
                (
                    format!("frequency-calendar-week:{identity_week_key}:{bucket_ordinal}"),
                    Some(date),
                )
            })
        }
        (
            Recurrence::Frequency {
                target,
                period: RecurrencePeriod::Month,
                semantics: RecurrenceSemantics::Calendar,
                weekdays,
                ..
            },
            RecurrenceOccurrenceIdentity::CalendarMonth {
                year,
                month,
                bucket_ordinal,
            },
        ) => {
            let date = allocated_month_date(year, month, weekdays, *target, bucket_ordinal)?;
            let month_value = Month::try_from(month).ok()?;
            let eligible_day_count = (1..=month_value.length(year))
                .filter_map(|day| Date::from_calendar_date(year, month_value, day).ok())
                .filter(|date| {
                    weekdays.is_empty() || weekdays.contains(&DayOfWeek::from_time(date.weekday()))
                })
                .count();
            (local_day_is_plausible(date)
                && calendar_source_matches(
                    request,
                    item.id,
                    source,
                    date,
                    *target,
                    eligible_day_count,
                    bucket_ordinal,
                ))
            .then(|| {
                (
                    format!("frequency-calendar-month:{year:04}-{month:02}:{bucket_ordinal}"),
                    Some(date),
                )
            })
        }
        (
            Recurrence::Frequency {
                target,
                period,
                semantics: RecurrenceSemantics::Rolling,
                anchor,
                ..
            },
            RecurrenceOccurrenceIdentity::RollingMinutes {
                index,
                anchor: identity_anchor,
            },
        ) if matches!(period, RecurrencePeriod::Day | RecurrencePeriod::Week) => {
            let (period_minutes, label) = match period {
                RecurrencePeriod::Day => (24 * 60, "frequency-rolling-day"),
                RecurrencePeriod::Week => (7 * 24 * 60, "frequency-rolling-week"),
                RecurrencePeriod::Month => unreachable!(),
            };
            validated_rolling_frequency_name(
                source,
                effective_rolling_anchor(request, item, *anchor),
                identity_anchor,
                *target,
                period_minutes,
                index,
                label,
            )
        }
        (
            Recurrence::Frequency {
                target,
                period: RecurrencePeriod::Month,
                semantics: RecurrenceSemantics::Rolling,
                anchor,
                ..
            },
            RecurrenceOccurrenceIdentity::RollingMonth {
                cycle,
                index,
                anchor: identity_anchor,
            },
        ) if cycle >= 0 && cycle <= i64::from(i32::MAX) && index < *target => {
            let effective_anchor = effective_rolling_anchor(request, item, *anchor);
            let nominal_dates_match = identity_anchor == effective_anchor
                && identity_anchor.offset() == effective_anchor.offset()
                && rolling_month_source_matches(
                    request,
                    source,
                    effective_anchor,
                    cycle,
                    index,
                    *target,
                )
                && source.local_date.is_none();
            nominal_dates_match.then(|| {
                (
                    format!(
                        "frequency-rolling-month:{}:{target}:{cycle}:{index}",
                        effective_anchor.unix_timestamp_nanos()
                    ),
                    None,
                )
            })
        }
        (
            Recurrence::Custom { rrule: _ },
            RecurrenceOccurrenceIdentity::CustomRule {
                rule_id,
                sequence,
                date,
            },
        ) => {
            let proof = custom_proof?;
            if proof.rule_id != rule_id
                || usize::try_from(sequence)
                    .ok()
                    .and_then(|index| proof.dates.get(index).copied())
                    != Some(date)
                || !local_day_is_plausible(date)
            {
                return None;
            }
            let day = calendar
                .days
                .iter()
                .find(|boundary| boundary.local_date == date)
                .filter(|_| calendar.time_zone_id.is_some())?;
            let expected_start = if date == item.created_at.date() {
                day.start.max(item.created_at)
            } else {
                day.start
            };
            let nominal_matches = source.nominal_start == expected_start
                && source.nominal_start.offset() == expected_start.offset()
                && source.nominal_end == day.end
                && source.nominal_end.offset() == day.end.offset();
            nominal_matches.then(|| (custom_identity_key(rule_id, sequence, date), Some(date)))
        }
        // The legacy placeholder is never valid proof for a generated custom occurrence.
        _ => None,
    }
}

fn validated_rolling_minute_name(
    source: crate::RecurrenceMoveSource,
    anchor: OffsetDateTime,
    identity_anchor: OffsetDateTime,
    interval_minutes: u32,
    index: i64,
    label: &str,
) -> Option<(String, Option<Date>)> {
    let index = i32::try_from(index).ok()?;
    let interval = Duration::minutes(i64::from(interval_minutes));
    let elapsed = interval.checked_mul(index)?;
    let expected_start = anchor.checked_add(elapsed)?;
    let expected_end = expected_start.checked_add(interval)?;
    (identity_anchor == anchor
        && identity_anchor.offset() == anchor.offset()
        && source.nominal_start == expected_start
        && source.nominal_start.offset() == expected_start.offset()
        && source.nominal_end == expected_end
        && source.nominal_end.offset() == expected_end.offset()
        && source.local_date.is_none())
    .then(|| {
        (
            format!(
                "{label}:{}:{interval_minutes}:{index}",
                anchor.unix_timestamp_nanos()
            ),
            None,
        )
    })
}

fn validated_rolling_frequency_name(
    source: crate::RecurrenceMoveSource,
    anchor: OffsetDateTime,
    identity_anchor: OffsetDateTime,
    target: u16,
    period_minutes: u32,
    index: i64,
    label: &str,
) -> Option<(String, Option<Date>)> {
    let expected_start = rolling_frequency_instant(anchor, target, period_minutes, index).ok()?;
    let expected_end =
        rolling_frequency_instant(anchor, target, period_minutes, index.checked_add(1)?).ok()?;
    (identity_anchor == anchor
        && identity_anchor.offset() == anchor.offset()
        && source.nominal_start == expected_start
        && source.nominal_start.offset() == expected_start.offset()
        && source.nominal_end == expected_end
        && source.nominal_end.offset() == expected_end.offset()
        && source.local_date.is_none())
    .then(|| {
        (
            format!(
                "{label}:{}:{target}:{period_minutes}:{index}",
                anchor.unix_timestamp_nanos()
            ),
            None,
        )
    })
}

fn effective_rolling_anchor(
    request: &PlanRequest,
    item: &WorkItem,
    rule_anchor: Option<OffsetDateTime>,
) -> OffsetDateTime {
    rule_anchor
        .or_else(|| {
            request
                .recurrence_context
                .rolling_anchors
                .get(&item.id)
                .copied()
        })
        .unwrap_or(item.created_at)
}

fn allocated_week_date(
    identity_week_key: i32,
    starts_on: DayOfWeek,
    weekdays: &BTreeSet<DayOfWeek>,
    target: u16,
    bucket_ordinal: u16,
) -> Option<Date> {
    let start = Date::from_julian_day(identity_week_key).ok()?;
    if week_key(start, starts_on) != identity_week_key
        || DayOfWeek::from_time(start.weekday()) != starts_on
    {
        return None;
    }
    let dates = (0..7)
        .filter_map(|offset| start.checked_add(Duration::days(offset)))
        .filter(|date| {
            weekdays.is_empty() || weekdays.contains(&DayOfWeek::from_time(date.weekday()))
        })
        .collect::<Vec<_>>();
    allocated_date(&dates, target, bucket_ordinal)
}

fn allocated_month_date(
    year: i32,
    month: u8,
    weekdays: &BTreeSet<DayOfWeek>,
    target: u16,
    bucket_ordinal: u16,
) -> Option<Date> {
    let month = Month::try_from(month).ok()?;
    let dates = (1..=month.length(year))
        .filter_map(|day| Date::from_calendar_date(year, month, day).ok())
        .filter(|date| {
            weekdays.is_empty() || weekdays.contains(&DayOfWeek::from_time(date.weekday()))
        })
        .collect::<Vec<_>>();
    allocated_date(&dates, target, bucket_ordinal)
}

fn allocated_date(dates: &[Date], target: u16, bucket_ordinal: u16) -> Option<Date> {
    if target == 0 || bucket_ordinal >= target || dates.is_empty() {
        return None;
    }
    let index = usize::from(bucket_ordinal) * dates.len() / usize::from(target);
    dates.get(index).copied()
}

fn calendar_source_matches(
    request: &PlanRequest,
    item_id: ItemId,
    source: crate::RecurrenceMoveSource,
    date: Date,
    target: u16,
    eligible_day_count: usize,
    bucket_ordinal: u16,
) -> bool {
    let Some(day) = authoritative_day_boundary(request, date) else {
        return false;
    };
    let Some((expected_start, expected_end)) = calendar_occurrence_bounds(
        day,
        target,
        eligible_day_count,
        bucket_ordinal,
        minimum_spacing(request, item_id),
    ) else {
        return false;
    };
    source.nominal_start == expected_start
        && source.nominal_start.offset() == expected_start.offset()
        && source.nominal_end == expected_end
        && source.nominal_end.offset() == expected_end.offset()
}

fn authoritative_day_boundary(request: &PlanRequest, date: Date) -> Option<ZonedDayBoundary> {
    let calendar = &request.recurrence_context.calendar;
    if !calendar.days.is_empty() {
        calendar.time_zone_id.as_ref()?;
        return calendar
            .days
            .iter()
            .find(|day| day.local_date == date)
            .copied();
    }
    let offset = request.horizon_start.offset();
    let start = midnight(date, offset).ok()?;
    let end = midnight(date.next_day()?, offset).ok()?;
    Some(ZonedDayBoundary {
        local_date: date,
        start,
        end,
    })
}

fn authoritative_boundary_instant(request: &PlanRequest, date: Date) -> Option<OffsetDateTime> {
    let calendar = &request.recurrence_context.calendar;
    if !calendar.days.is_empty() {
        calendar.time_zone_id.as_ref()?;
        return calendar.days.iter().find_map(|day| {
            if day.local_date == date {
                Some(day.start)
            } else if day.local_date.next_day() == Some(date) {
                Some(day.end)
            } else {
                None
            }
        });
    }
    midnight(date, request.horizon_start.offset()).ok()
}

fn rolling_month_source_matches(
    request: &PlanRequest,
    source: crate::RecurrenceMoveSource,
    anchor: OffsetDateTime,
    cycle: i64,
    index: u16,
    target: u16,
) -> bool {
    let Some(next_cycle) = cycle.checked_add(1) else {
        return false;
    };
    let Some(start_date) = add_months(anchor.date(), cycle).ok() else {
        return false;
    };
    let Some(end_date) = add_months(anchor.date(), next_cycle).ok() else {
        return false;
    };
    let Some(cycle_start) = authoritative_boundary_instant(request, start_date) else {
        return false;
    };
    let Some(cycle_end) = authoritative_boundary_instant(request, end_date) else {
        return false;
    };
    let duration = cycle_end - cycle_start;
    let Some(start_offset) = duration
        .checked_mul(i32::from(index))
        .and_then(|value| value.checked_div(i32::from(target)))
    else {
        return false;
    };
    let Some(expected_start) = cycle_start.checked_add(start_offset) else {
        return false;
    };
    let expected_end = if index + 1 == target {
        cycle_end
    } else {
        let Some(end_offset) = duration
            .checked_mul(i32::from(index + 1))
            .and_then(|value| value.checked_div(i32::from(target)))
        else {
            return false;
        };
        let Some(end) = cycle_start.checked_add(end_offset) else {
            return false;
        };
        end
    };
    source.nominal_start == expected_start
        && source.nominal_start.offset() == expected_start.offset()
        && source.nominal_end == expected_end
        && source.nominal_end.offset() == expected_end.offset()
}

fn calendar_occurrence_bounds(
    day: ZonedDayBoundary,
    target: u16,
    eligible_day_count: usize,
    bucket_ordinal: u16,
    minimum_spacing: Minutes,
) -> Option<(OffsetDateTime, OffsetDateTime)> {
    if target == 0 || bucket_ordinal >= target || eligible_day_count == 0 {
        return None;
    }
    let target = usize::from(target);
    let ordinal = usize::from(bucket_ordinal);
    let day_index = ordinal
        .checked_mul(eligible_day_count)?
        .checked_div(target)?;
    let first = day_index
        .checked_mul(target)?
        .checked_add(eligible_day_count - 1)?
        .checked_div(eligible_day_count)?;
    let after_last = day_index
        .checked_add(1)?
        .checked_mul(target)?
        .checked_add(eligible_day_count - 1)?
        .checked_div(eligible_day_count)?;
    let position = ordinal.checked_sub(first)?;
    let count = after_last.checked_sub(first)?;
    let position = i32::try_from(position).ok()?;
    let count = i32::try_from(count).ok()?;
    let day_duration = day.end - day.start;
    let start_offset = day_duration.checked_mul(position)?.checked_div(count)?;
    let start = day.start.checked_add(start_offset)?;
    let end = if position.checked_add(1)? == count {
        day.end
    } else {
        let end_offset = day_duration
            .checked_mul(position.checked_add(1)?)?
            .checked_div(count)?;
        day.start.checked_add(end_offset)?
    };
    let spacing_end = start
        .checked_add(Duration::minutes(i64::from(minimum_spacing.get())))
        .map_or(day.end, |value| value.min(day.end));
    Some((start, end.max(spacing_end)))
}

fn minimum_spacing(request: &PlanRequest, item_id: ItemId) -> Minutes {
    request
        .recurrence_context
        .minimum_spacing
        .get(&item_id)
        .copied()
        .or_else(|| {
            request
                .items
                .iter()
                .find(|item| item.id == item_id)
                .and_then(|item| match &item.kind {
                    ItemKind::Habit(spec) if !spec.minimum_spacing.is_zero() => {
                        Some(spec.minimum_spacing)
                    }
                    _ => recurrence_of(item).and_then(|recurrence| match recurrence {
                        Recurrence::Frequency {
                            minimum_spacing, ..
                        } => Some(*minimum_spacing),
                        _ => None,
                    }),
                })
        })
        .unwrap_or(Minutes::ZERO)
}

fn children_by_parent(items: &[WorkItem]) -> BTreeMap<ItemId, Vec<ItemId>> {
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

fn collect_subtree(root: ItemId, children: &BTreeMap<ItemId, Vec<ItemId>>) -> Vec<ItemId> {
    fn visit(id: ItemId, children: &BTreeMap<ItemId, Vec<ItemId>>, result: &mut Vec<ItemId>) {
        result.push(id);
        for child in children.get(&id).map_or(&[][..], Vec::as_slice) {
            visit(*child, children, result);
        }
    }
    let mut result = Vec::new();
    visit(root, children, &mut result);
    result
}

fn subtree_leaves(
    subtree: &[ItemId],
    children: &BTreeMap<ItemId, Vec<ItemId>>,
    by_id: &BTreeMap<ItemId, &WorkItem>,
) -> Vec<ItemId> {
    let subtree_set: BTreeSet<_> = subtree.iter().copied().collect();
    let mut leaves: Vec<_> = subtree
        .iter()
        .filter(|id| {
            let has_children = children
                .get(id)
                .is_some_and(|values| values.iter().any(|child| subtree_set.contains(child)));
            by_id[id].occupies_time(has_children)
        })
        .copied()
        .collect();
    leaves.sort_by_key(|id| (by_id[id].sibling_order.unwrap_or(u32::MAX), *id));
    leaves
}

fn materialize_previous_assignments(
    request: &PlanRequest,
    occurrences: &[Occurrence],
    identities: &BTreeMap<ItemId, MaterializedIdentity>,
    removed_templates: &BTreeSet<ItemId>,
) -> Vec<PreviousAssignment> {
    let mut result = Vec::new();
    for assignment in &request.previous_assignments {
        let candidates: Vec<_> = identities
            .iter()
            .filter(|(_, identity)| identity.series_item_id == assignment.item_id)
            .collect();
        if candidates.is_empty() {
            if !removed_templates.contains(&assignment.item_id) {
                result.push(assignment.clone());
            }
            continue;
        }
        let matched = if let Some(id) = assignment.occurrence_id {
            // An explicit occurrence identity is authoritative. Falling back
            // to a time-window or first-series match can pin a different
            // occurrence after a stale or forged manual placement request.
            candidates
                .iter()
                .find(|(_, identity)| identity.occurrence_id == id)
                .copied()
        } else {
            assignment
                .blocks
                .first()
                .and_then(|block| {
                    candidates.iter().find_map(|candidate @ (_, identity)| {
                        let occurrence = occurrences
                            .iter()
                            .find(|value| value.id == identity.occurrence_id)?;
                        let contains = occurrence.window_start <= block.start
                            && block.start < occurrence.window_end;
                        contains.then_some(*candidate)
                    })
                })
                .or_else(|| candidates.first().copied())
        };
        if let Some((clone_id, identity)) = matched {
            let mut value = assignment.clone();
            value.item_id = *clone_id;
            value.occurrence_id = Some(identity.occurrence_id);
            result.push(value);
        }
    }
    result
}

fn week_key(date: Date, starts_on: DayOfWeek) -> i32 {
    let day = weekday_index(DayOfWeek::from_time(date.weekday()));
    let start = weekday_index(starts_on);
    date.to_julian_day() - i32::from((7 + day - start) % 7)
}

const fn weekday_index(day: DayOfWeek) -> u8 {
    match day {
        DayOfWeek::Monday => 0,
        DayOfWeek::Tuesday => 1,
        DayOfWeek::Wednesday => 2,
        DayOfWeek::Thursday => 3,
        DayOfWeek::Friday => 4,
        DayOfWeek::Saturday => 5,
        DayOfWeek::Sunday => 6,
    }
}

fn month_index(date: Date) -> i32 {
    date.year() * 12 + i32::from(u8::from(date.month())) - 1
}

fn add_months(date: Date, offset: i64) -> Result<Date, RecurrenceError> {
    let base = i64::from(month_index(date));
    let value = base
        .checked_add(offset)
        .ok_or(RecurrenceError::DateOutOfRange)?;
    let year = i32::try_from(value.div_euclid(12)).map_err(|_| RecurrenceError::DateOutOfRange)?;
    let month_number =
        u8::try_from(value.rem_euclid(12) + 1).map_err(|_| RecurrenceError::DateOutOfRange)?;
    let month = Month::try_from(month_number).map_err(|_| RecurrenceError::DateOutOfRange)?;
    let day = date.day().min(month.length(year));
    Date::from_calendar_date(year, month, day).map_err(|_| RecurrenceError::DateOutOfRange)
}

fn boundary_instant(
    date: Date,
    days: &[ZonedDayBoundary],
    fallback_offset: UtcOffset,
) -> Result<OffsetDateTime, RecurrenceError> {
    if let Some(day) = days.iter().find(|day| day.local_date == date) {
        return Ok(day.start);
    }
    if let Some(previous) = days
        .iter()
        .find(|day| day.local_date.next_day() == Some(date))
    {
        return Ok(previous.end);
    }
    midnight(date, fallback_offset)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::partial_remaining_minutes;
    use crate::{HABIT_BASIS_POINTS_SCALE, Minutes, OccurrenceId, RecurrencePartialProgress};

    #[test]
    fn every_nonterminal_basis_point_derives_a_positive_ceiling_bounded_remainder() {
        let occurrence_id = OccurrenceId(Uuid::from_u128(1));
        let expected = Minutes(527_040);
        for progress_basis_points in 1..HABIT_BASIS_POINTS_SCALE {
            let remaining = partial_remaining_minutes(
                occurrence_id,
                RecurrencePartialProgress {
                    progress_basis_points,
                    expected_duration_minutes: expected,
                    remaining_duration_minutes: None,
                },
            )
            .unwrap();
            let incomplete = u64::from(HABIT_BASIS_POINTS_SCALE - progress_basis_points);
            let derived = (u64::from(expected.get()) * incomplete
                + u64::from(HABIT_BASIS_POINTS_SCALE - 1))
                / u64::from(HABIT_BASIS_POINTS_SCALE);
            assert_eq!(u64::from(remaining.get()), derived);
            assert!(!remaining.is_zero());
            assert!(remaining <= expected);
        }
    }
}
