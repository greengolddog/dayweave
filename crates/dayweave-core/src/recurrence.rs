use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::{Date, Duration, Month, OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::{
    AbsoluteWindow, ConstraintStrength, DayOfWeek, Dependency, DependencyRelation, ItemId,
    ItemKind, Minutes, Occurrence, OccurrenceId, OccurrenceState, PlanRequest, PreviousAssignment,
    Recurrence, RecurrenceExceptionAction, RecurrenceExceptionSelector, RecurrencePeriod,
    RecurrenceSemantics, WorkItem, ZonedDayBoundary,
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

    let mut result = Vec::new();
    for item in roots {
        let Some(recurrence) = recurrence_of(item) else {
            continue;
        };
        let mut occurrences = expand_series(request, item, recurrence, &days)?;
        apply_pauses_and_exceptions(request, item.id, &mut occurrences);
        result.extend(occurrences);
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
                if let Some(duration) = &mut clone.duration {
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
    Ok(MaterializedPlan {
        request: materialized_request,
        occurrences,
        identities,
    })
}

fn recurrence_of(item: &WorkItem) -> Option<&Recurrence> {
    match &item.kind {
        ItemKind::RecurringTask(spec) => Some(&spec.recurrence),
        ItemKind::Habit(spec) => Some(&spec.recurrence),
        ItemKind::Routine(spec) => spec.recurrence.as_ref(),
        _ => None,
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
    let mut dates = BTreeSet::new();
    for day in &request.recurrence_context.calendar.days {
        if day.start >= day.end
            || day.start.date() != day.local_date
            || (day.end - Duration::nanoseconds(1)).date() != day.local_date
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
    for pause in &request.recurrence_context.pauses {
        if pause.start >= pause.end {
            return Err(RecurrenceError::InvalidPause(pause.item_id));
        }
    }
    for exception in &request.recurrence_context.exceptions {
        if let RecurrenceExceptionAction::Move { start, end } = exception.action
            && start >= end
        {
            return Err(RecurrenceError::InvalidException(exception.item_id));
        }
    }
    for item in &request.items {
        if let Some(recurrence) = recurrence_of(item) {
            validate_rule(item.id, recurrence)?;
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
        Recurrence::Custom { rrule } if rrule.trim().is_empty() => {
            Err(invalid("custom recurrence rule cannot be empty"))
        }
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
            let mut groups: BTreeMap<i32, Vec<ZonedDayBoundary>> = BTreeMap::new();
            for day in days {
                if weekdays.is_empty()
                    || weekdays.contains(&DayOfWeek::from_time(day.local_date.weekday()))
                {
                    groups
                        .entry(week_key(day.local_date, week_start))
                        .or_default()
                        .push(*day);
                }
            }
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
        Recurrence::Monthly { times_per_month } => Ok(expand_monthly(
            request,
            item.id,
            days,
            *times_per_month,
            &BTreeSet::new(),
            spacing,
            "monthly",
        )),
        Recurrence::EveryInterval { interval } => Ok(expand_rolling_minutes(
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
        )),
        Recurrence::AfterCompletion { interval } => {
            Ok(expand_after_completion(request, item, *interval))
        }
        Recurrence::Frequency {
            target,
            period,
            semantics,
            weekdays,
            minimum_spacing,
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
                    *minimum_spacing,
                    request,
                    "frequency-calendar-day",
                )),
                RecurrencePeriod::Week => {
                    let mut groups: BTreeMap<i32, Vec<ZonedDayBoundary>> = BTreeMap::new();
                    for day in days {
                        if weekdays.is_empty()
                            || weekdays.contains(&DayOfWeek::from_time(day.local_date.weekday()))
                        {
                            groups
                                .entry(week_key(day.local_date, week_start))
                                .or_default()
                                .push(*day);
                        }
                    }
                    Ok(expand_calendar_buckets(
                        item.id,
                        groups
                            .into_iter()
                            .map(|(key, value)| (key.to_string(), value)),
                        *target,
                        *minimum_spacing,
                        request,
                        "frequency-calendar-week",
                    ))
                }
                RecurrencePeriod::Month => Ok(expand_monthly(
                    request,
                    item.id,
                    days,
                    *target,
                    weekdays,
                    *minimum_spacing,
                    "frequency-calendar-month",
                )),
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
                    RecurrencePeriod::Day => Ok(expand_rolling_minutes(
                        request,
                        item,
                        anchor,
                        (24 * 60) / u32::from(*target),
                        "frequency-rolling-day",
                    )),
                    RecurrencePeriod::Week => Ok(expand_rolling_minutes(
                        request,
                        item,
                        anchor,
                        (7 * 24 * 60) / u32::from(*target),
                        "frequency-rolling-week",
                    )),
                    RecurrencePeriod::Month => {
                        expand_rolling_months(request, item, anchor, *target, days)
                    }
                }
            }
        },
        // RFC 5545 parsing belongs to the Google/calendar adapter. Retaining one
        // stable bounded occurrence avoids silently dropping imported work.
        Recurrence::Custom { rrule } => Ok(vec![make_occurrence(
            item.id,
            format!("custom:{rrule}").as_bytes(),
            (request.horizon_start, request.horizon_end),
            (request.horizon_start, request.horizon_end),
            None,
            0,
        )]),
    }
}

fn expand_monthly(
    request: &PlanRequest,
    item_id: ItemId,
    days: &[ZonedDayBoundary],
    target: u16,
    weekdays: &BTreeSet<DayOfWeek>,
    spacing: Minutes,
    label: &str,
) -> Vec<Occurrence> {
    let mut groups: BTreeMap<(i32, u8), Vec<ZonedDayBoundary>> = BTreeMap::new();
    for day in days {
        if weekdays.is_empty() || weekdays.contains(&DayOfWeek::from_time(day.local_date.weekday()))
        {
            groups
                .entry((day.local_date.year(), u8::from(day.local_date.month())))
                .or_default()
                .push(*day);
        }
    }
    expand_calendar_buckets(
        item_id,
        groups
            .into_iter()
            .map(|((year, month), value)| (format!("{year:04}-{month:02}"), value)),
        target,
        spacing,
        request,
        label,
    )
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
    let mut ordinal = 0_u32;
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
                let start = day.start
                    + day_duration * i32::try_from(position).unwrap_or(i32::MAX)
                        / i32::try_from(count).unwrap_or(i32::MAX);
                let end = day.start
                    + day_duration * i32::try_from(position + 1).unwrap_or(i32::MAX)
                        / i32::try_from(count).unwrap_or(i32::MAX);
                let spacing_end = start + Duration::minutes(i64::from(minimum_spacing.get()));
                let effective_end = end.max(spacing_end.min(day.end));
                let key = format!("{label}:{bucket_key}:{bucket_ordinal}");
                let occurrence = make_occurrence(
                    item_id,
                    key.as_bytes(),
                    (start, effective_end),
                    (
                        start.max(request.horizon_start),
                        effective_end.min(request.horizon_end),
                    ),
                    Some(day.local_date),
                    ordinal,
                );
                if occurrence.window_start < occurrence.window_end {
                    result.push(occurrence);
                    ordinal = ordinal.saturating_add(1);
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
) -> Vec<Occurrence> {
    if interval_minutes == 0 {
        return Vec::new();
    }
    let interval = Duration::minutes(i64::from(interval_minutes));
    let elapsed = request.horizon_start - anchor;
    let mut index = elapsed
        .whole_minutes()
        .div_euclid(i64::from(interval_minutes));
    index = index.max(0);
    while anchor + interval * i32_saturating(index + 1) <= request.horizon_start {
        index += 1;
    }
    let mut result = Vec::new();
    let mut ordinal = 0_u32;
    loop {
        let start = anchor + interval * i32_saturating(index);
        if start >= request.horizon_end {
            break;
        }
        let end = (start + interval).min(request.horizon_end);
        let key = format!("{label}:{index}");
        result.push(make_occurrence(
            item.id,
            key.as_bytes(),
            (start, start + interval),
            (start.max(request.horizon_start), end),
            None,
            ordinal,
        ));
        ordinal = ordinal.saturating_add(1);
        index += 1;
    }
    result
}

fn expand_after_completion(
    request: &PlanRequest,
    item: &WorkItem,
    interval: Minutes,
) -> Vec<Occurrence> {
    let anchor = request
        .recurrence_context
        .completion_anchors
        .get(&item.id)
        .copied()
        .unwrap_or(item.created_at);
    let due = anchor + Duration::minutes(i64::from(interval.get()));
    if due >= request.horizon_end {
        return Vec::new();
    }
    let start = due.max(request.horizon_start);
    let key = format!("after-completion:{}", anchor.unix_timestamp_nanos());
    vec![make_occurrence(
        item.id,
        key.as_bytes(),
        (due, request.horizon_end),
        (start, request.horizon_end),
        None,
        0,
    )]
}

fn expand_rolling_months(
    request: &PlanRequest,
    item: &WorkItem,
    anchor: OffsetDateTime,
    target: u16,
    days: &[ZonedDayBoundary],
) -> Result<Vec<Occurrence>, RecurrenceError> {
    let anchor_date = local_date_for(anchor, days);
    let first_date = days
        .first()
        .map_or(request.horizon_start.date(), |day| day.local_date);
    let anchor_month = month_index(anchor_date);
    let first_month = month_index(first_date);
    let mut cycle = i64::from(first_month - anchor_month - 1).max(0);
    let mut result = Vec::new();
    let mut ordinal = 0_u32;
    loop {
        let start_date = add_months(anchor_date, cycle)?;
        let end_date = add_months(anchor_date, cycle + 1)?;
        let cycle_start = boundary_instant(start_date, days, request.horizon_start.offset())?;
        let cycle_end = boundary_instant(end_date, days, request.horizon_start.offset())?;
        if cycle_start >= request.horizon_end {
            break;
        }
        if cycle_end > request.horizon_start {
            let duration = cycle_end - cycle_start;
            for index in 0..target {
                let start = cycle_start + duration * i32::from(index) / i32::from(target);
                let end = cycle_start + duration * i32::from(index + 1) / i32::from(target);
                if start < request.horizon_end && request.horizon_start < end {
                    let key = format!("frequency-rolling-month:{cycle}:{index}");
                    result.push(make_occurrence(
                        item.id,
                        key.as_bytes(),
                        (start, end),
                        (
                            start.max(request.horizon_start),
                            end.min(request.horizon_end),
                        ),
                        None,
                        ordinal,
                    ));
                    ordinal = ordinal.saturating_add(1);
                }
            }
        }
        cycle += 1;
    }
    Ok(result)
}

fn make_occurrence(
    item_id: ItemId,
    key: &[u8],
    nominal: (OffsetDateTime, OffsetDateTime),
    window: (OffsetDateTime, OffsetDateTime),
    local_date: Option<Date>,
    ordinal: u32,
) -> Occurrence {
    Occurrence {
        id: OccurrenceId(Uuid::new_v5(&item_id.0, key)),
        series_item_id: item_id,
        nominal_start: nominal.0,
        nominal_end: nominal.1,
        window_start: window.0,
        window_end: window.1,
        local_date,
        ordinal,
        state: OccurrenceState::Generated,
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
        }
        if request.recurrence_context.pauses.iter().any(|pause| {
            pause.item_id == item_id
                && pause.start < occurrence.window_end
                && occurrence.window_start < pause.end
        }) && occurrence.state != OccurrenceState::Completed
        {
            occurrence.state = OccurrenceState::Paused;
        }
        if occurrence.state == OccurrenceState::Completed {
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
                RecurrenceExceptionAction::Move { start, end } => {
                    occurrence.window_start = start;
                    occurrence.window_end = end;
                    occurrence.state = OccurrenceState::Generated;
                }
            }
        }
    }
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
                .find_map(|item| (item.id == item_id).then(|| recurrence_of(item)).flatten())
                .and_then(|recurrence| match recurrence {
                    Recurrence::Frequency {
                        minimum_spacing, ..
                    } => Some(*minimum_spacing),
                    _ => None,
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
        let matched = assignment
            .occurrence_id
            .and_then(|id| {
                candidates
                    .iter()
                    .find(|(_, identity)| identity.occurrence_id == id)
                    .copied()
            })
            .or_else(|| {
                assignment.blocks.first().and_then(|block| {
                    candidates.iter().find_map(|candidate @ (_, identity)| {
                        let occurrence = occurrences
                            .iter()
                            .find(|value| value.id == identity.occurrence_id)?;
                        let contains = occurrence.window_start <= block.start
                            && block.start < occurrence.window_end;
                        contains.then_some(*candidate)
                    })
                })
            })
            .or_else(|| candidates.first().copied());
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

fn local_date_for(value: OffsetDateTime, days: &[ZonedDayBoundary]) -> Date {
    days.iter()
        .find(|day| day.start <= value && value < day.end)
        .map_or(value.date(), |day| day.local_date)
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
    days.iter()
        .find(|day| day.local_date == date)
        .map_or_else(|| midnight(date, fallback_offset), |day| Ok(day.start))
}

fn i32_saturating(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}
