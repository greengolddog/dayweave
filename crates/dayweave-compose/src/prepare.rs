use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Datelike as _, LocalResult, NaiveDate, Offset as _, TimeZone as _, Utc};
use chrono_tz::Tz;
use dayweave_core::{
    AvailabilityWindow, BreakCategory, BreakSpec, DailyTimeWindow, DurationEstimate, EnergyLevel,
    FixedBlock, FixedBlockSource, GoalSpec, HabitSpec, ItemId, ItemKind as PlanningItemKind,
    Minutes, OccurrenceId, PlanRequest, PreviousAssignment, PreviousBlock, Priority, Qualified,
    Recurrence, RecurrenceContext, RecurrenceExceptionAction, RecurrenceExceptionSelector,
    RecurrenceOccurrenceIdentity, RecurringTaskSpec, RoutineSpec, SchedulerConfig,
    SchedulingConstraints, SplitPolicy as PlanningSplitPolicy, WorkItem, WorkStatus,
    ZonedDayBoundary,
};
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::metadata::{
    EnergyMetadata, MAX_RECURRENCE_BYTES, MAX_SCHEDULING_METADATA_BYTES, SchedulingMetadata,
    SchedulingMetadataInput, validate_scheduling_metadata,
};
use crate::model::{
    AvailabilityInput, CanonicalItem, CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy,
    ComposeScheduleRequest, EnergyInput, FixedBlockInput, FixedBlockSourceInput,
    IgnoredPreviousAssignment, ManualPlacementInput, PreparedSchedule, PreviousAssignmentInput,
    RejectedScheduleItem,
};

pub const MAX_CANONICAL_ITEMS: usize = 10_000;
pub const MAX_AVAILABILITY_WINDOWS: usize = 10_000;
pub const MAX_FIXED_BLOCKS: usize = 10_000;
pub const MAX_PREVIOUS_ASSIGNMENTS: usize = 10_000;
pub const MAX_PREVIOUS_BLOCKS: usize = 50_000;
/// Maximum number of exact placement groups accepted in one interactive compose request.
pub const MAX_MANUAL_PLACEMENTS: usize = 64;
/// Maximum number of work assignments across all manual placement groups.
pub const MAX_MANUAL_ASSIGNMENTS: usize = 128;
/// Maximum number of exact time blocks across all manual placement assignments.
pub const MAX_MANUAL_BLOCKS: usize = 256;
/// Maximum number of retained-placement release commands accepted in one request.
pub const MAX_MANUAL_PLACEMENT_RELEASES: usize = 64;
pub const MAX_RECURRENCE_CONTEXT_ENTRIES: usize = 10_000;
pub const MAX_HORIZON_DAYS: i64 = 90;
pub const MAX_CALENDAR_DAYS: usize = 92;
pub const MAX_WEIGHT: u32 = 1_000_000;
pub const MAX_BLOCK_TITLE_CHARACTERS: usize = 500;
const MAX_NOTES_CHARACTERS: usize = 100_000;
const MAX_DURATION_SECONDS: u32 = 366 * 24 * 60 * 60;
const MAX_SIBLING_ORDER: u32 = 1_000_000;

#[derive(Debug, thiserror::Error, Clone, Eq, PartialEq)]
pub enum PrepareScheduleError {
    #[error("invalid schedule preview request: {0}")]
    InvalidRequest(String),
    #[error("canonical item count exceeds the supported limit of {MAX_CANONICAL_ITEMS}")]
    TooManyItems,
    #[error("canonical snapshot contains duplicate item identifier {0}")]
    DuplicateCanonicalItem(Uuid),
    #[error("canonical snapshot contains invalid item {0}")]
    InvalidCanonicalItem(Uuid),
    #[error("canonical schedule preparation overflowed its bounded accounting")]
    AccountingOverflow,
}

/// Validates the caller-owned portion of a schedule request without reading
/// canonical items or executing the scheduler.
///
/// # Errors
///
/// Returns [`PrepareScheduleError::InvalidRequest`] when the horizon,
/// timestamps, timezone, collection bounds, identifiers, titles, or scheduler
/// weights do not satisfy the public compose contract.
#[allow(clippy::too_many_lines)]
pub fn validate_schedule_request(
    request: &ComposeScheduleRequest,
) -> Result<(), PrepareScheduleError> {
    let horizon = request.horizon_end - request.horizon_start;
    if horizon <= chrono::Duration::zero() || horizon > chrono::Duration::days(MAX_HORIZON_DAYS) {
        return invalid(format!(
            "horizon must be positive and no longer than {MAX_HORIZON_DAYS} days"
        ));
    }
    if request.as_of > request.horizon_end {
        return invalid("as_of must not be later than horizon_end");
    }
    let chrono_instant_is_precise =
        |value: DateTime<Utc>| value.timestamp_subsec_nanos().is_multiple_of(1_000);
    let offset_instant_is_precise =
        |value: OffsetDateTime| value.nanosecond().is_multiple_of(1_000);
    let recurrence_instants_are_precise =
        request
            .recurrence_context
            .completion_anchors
            .values()
            .chain(request.recurrence_context.rolling_anchors.values())
            .all(|value| offset_instant_is_precise(*value))
            && request.recurrence_context.calendar.days.iter().all(|day| {
                offset_instant_is_precise(day.start) && offset_instant_is_precise(day.end)
            })
            && request.recurrence_context.pauses.iter().all(|pause| {
                offset_instant_is_precise(pause.start) && offset_instant_is_precise(pause.end)
            })
            && request
                .recurrence_context
                .exceptions
                .iter()
                .all(|exception| {
                    let selector = match exception.selector {
                        RecurrenceExceptionSelector::NominalStart { at } => {
                            offset_instant_is_precise(at)
                        }
                        RecurrenceExceptionSelector::Occurrence { .. }
                        | RecurrenceExceptionSelector::LocalDate { .. } => true,
                    };
                    let action = match exception.action {
                        RecurrenceExceptionAction::Move { start, end, source } => {
                            offset_instant_is_precise(start)
                                && offset_instant_is_precise(end)
                                && offset_instant_is_precise(source.nominal_start)
                                && offset_instant_is_precise(source.nominal_end)
                                && !matches!(
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
                                        if !offset_instant_is_precise(anchor)
                                )
                        }
                        RecurrenceExceptionAction::Skip => true,
                    };
                    selector && action
                });
    let manual_instants_are_precise = request.manual_placements.iter().all(|placement| {
        placement.assignments.iter().all(|assignment| {
            assignment.blocks.iter().all(|block| {
                chrono_instant_is_precise(block.start) && chrono_instant_is_precise(block.end)
            })
        })
    });
    if !chrono_instant_is_precise(request.as_of)
        || !chrono_instant_is_precise(request.horizon_start)
        || !chrono_instant_is_precise(request.horizon_end)
        || request.availability.iter().any(|window| {
            !chrono_instant_is_precise(window.start) || !chrono_instant_is_precise(window.end)
        })
        || request.fixed_blocks.iter().any(|block| {
            !chrono_instant_is_precise(block.start) || !chrono_instant_is_precise(block.end)
        })
        || request.previous_assignments.iter().any(|assignment| {
            assignment.blocks.iter().any(|block| {
                !chrono_instant_is_precise(block.start) || !chrono_instant_is_precise(block.end)
            })
        })
        || !recurrence_instants_are_precise
        || !manual_instants_are_precise
    {
        return invalid("schedule instants must use PostgreSQL microsecond precision");
    }
    if request.timezone_name.parse::<Tz>().is_err() {
        return invalid("timezone_name must be a valid IANA timezone");
    }
    if request.availability.len() > MAX_AVAILABILITY_WINDOWS {
        return invalid(format!(
            "availability supports at most {MAX_AVAILABILITY_WINDOWS} windows"
        ));
    }
    if request.fixed_blocks.len() > MAX_FIXED_BLOCKS {
        return invalid(format!(
            "fixed_blocks supports at most {MAX_FIXED_BLOCKS} entries"
        ));
    }
    if request.fixed_blocks.iter().any(|block| {
        block.title.trim().is_empty()
            || block.title.chars().count() > MAX_BLOCK_TITLE_CHARACTERS
            || block.title.chars().any(char::is_control)
    }) {
        return invalid(format!(
            "fixed block titles must contain 1-{MAX_BLOCK_TITLE_CHARACTERS} non-control characters"
        ));
    }
    let mut fixed_ids = BTreeSet::new();
    if request
        .fixed_blocks
        .iter()
        .any(|block| !fixed_ids.insert(block.id))
    {
        return invalid("fixed block ids must be unique");
    }
    if request.previous_assignments.len() > MAX_PREVIOUS_ASSIGNMENTS {
        return invalid(format!(
            "previous_assignments supports at most {MAX_PREVIOUS_ASSIGNMENTS} entries"
        ));
    }
    let previous_blocks = request
        .previous_assignments
        .iter()
        .try_fold(0_usize, |total, assignment| {
            total.checked_add(assignment.blocks.len())
        })
        .ok_or_else(|| PrepareScheduleError::InvalidRequest("too many previous blocks".into()))?;
    if previous_blocks > MAX_PREVIOUS_BLOCKS {
        return invalid(format!(
            "previous assignments support at most {MAX_PREVIOUS_BLOCKS} blocks"
        ));
    }
    validate_manual_placement_shape(request)?;
    if !(1..=60).contains(&request.config.slot_granularity_minutes) {
        return invalid("slot_granularity_minutes must be in 1..=60");
    }
    if request.config.stability_weight > MAX_WEIGHT
        || request.config.default_soft_weight > MAX_WEIGHT
    {
        return invalid(format!("scheduler weights must be at most {MAX_WEIGHT}"));
    }
    let context_entries = request
        .recurrence_context
        .completion_anchors
        .len()
        .saturating_add(request.recurrence_context.rolling_anchors.len())
        .saturating_add(request.recurrence_context.minimum_spacing.len())
        .saturating_add(request.recurrence_context.completed_occurrence_ids.len())
        .saturating_add(request.recurrence_context.pauses.len())
        .saturating_add(request.recurrence_context.exceptions.len());
    if context_entries > MAX_RECURRENCE_CONTEXT_ENTRIES {
        return invalid(format!(
            "recurrence_context supports at most {MAX_RECURRENCE_CONTEXT_ENTRIES} entries"
        ));
    }
    if request.recurrence_context.calendar.days.len() > MAX_CALENDAR_DAYS {
        return invalid("recurrence calendar contains more days than the maximum horizon");
    }
    Ok(())
}

fn validate_manual_placement_shape(
    request: &ComposeScheduleRequest,
) -> Result<(), PrepareScheduleError> {
    if request.manual_placements.len() > MAX_MANUAL_PLACEMENTS {
        return invalid(format!(
            "manual_placements supports at most {MAX_MANUAL_PLACEMENTS} entries"
        ));
    }
    if request.manual_placement_releases.len() > MAX_MANUAL_PLACEMENT_RELEASES {
        return invalid(format!(
            "manual_placement_releases supports at most {MAX_MANUAL_PLACEMENT_RELEASES} entries"
        ));
    }
    validate_manual_placement_releases(request)?;
    let mut placement_ids = BTreeSet::new();
    let mut assignment_identities = BTreeSet::new();
    let mut assignment_count = 0_usize;
    let mut block_count = 0_usize;
    let earliest = request.as_of.max(request.horizon_start);
    for placement in &request.manual_placements {
        if placement.id.is_nil()
            || placement
                .source_schedule_revision_id
                .is_some_and(|revision| revision.is_nil())
            || !placement_ids.insert(placement.id)
            || placement.assignments.is_empty()
        {
            return invalid(
                "manual placement ids and assignment groups must be unique and non-empty",
            );
        }
        assignment_count = assignment_count
            .checked_add(placement.assignments.len())
            .ok_or_else(|| {
                PrepareScheduleError::InvalidRequest("too many manual assignments".into())
            })?;
        if assignment_count > MAX_MANUAL_ASSIGNMENTS {
            return invalid(format!(
                "manual placement assignments support at most {MAX_MANUAL_ASSIGNMENTS} entries"
            ));
        }
        let mut occurrence_identity = None;
        for assignment in &placement.assignments {
            if assignment.item_id.is_nil()
                || assignment.item_revision == 0
                || assignment.blocks.is_empty()
                || !assignment_identities.insert((assignment.item_id, assignment.occurrence_id))
            {
                return invalid(
                    "manual placement assignments must have unique current work identities and non-empty blocks",
                );
            }
            if let Some(occurrence_id) = assignment.occurrence_id
                && occurrence_id.get_version_num() != 5
            {
                return invalid(
                    "manual recurrence placements require a scheduler-issued v5 occurrence id",
                );
            }
            match (occurrence_identity, assignment.occurrence_id) {
                (None, value) => occurrence_identity = Some(value),
                (Some(expected), value) if expected != value => {
                    return invalid(
                        "one manual placement cannot mix recurrence occurrence identities",
                    );
                }
                _ => {}
            }
            block_count = block_count
                .checked_add(assignment.blocks.len())
                .ok_or_else(|| {
                    PrepareScheduleError::InvalidRequest("too many manual placement blocks".into())
                })?;
            if block_count > MAX_MANUAL_BLOCKS {
                return invalid(format!(
                    "manual placement assignments support at most {MAX_MANUAL_BLOCKS} blocks"
                ));
            }
            let mut session_indices = BTreeSet::new();
            for block in &assignment.blocks {
                if block.start >= block.end
                    || block.start < earliest
                    || block.end > request.horizon_end
                    || !session_indices.insert(block.session_index)
                {
                    return invalid(
                        "manual placement blocks must be unique, future, positive, and fully inside the horizon",
                    );
                }
            }
        }
    }
    validate_manual_placement_capacity(request, assignment_count, block_count)
}

fn validate_manual_placement_capacity(
    request: &ComposeScheduleRequest,
    assignment_count: usize,
    block_count: usize,
) -> Result<(), PrepareScheduleError> {
    if request
        .previous_assignments
        .len()
        .checked_add(assignment_count)
        .is_none_or(|count| count > MAX_PREVIOUS_ASSIGNMENTS)
    {
        return invalid("manual placement assignment count exceeds the scheduling limit");
    }
    let previous_block_count = request
        .previous_assignments
        .iter()
        .map(|assignment| assignment.blocks.len())
        .sum::<usize>();
    if previous_block_count
        .checked_add(block_count)
        .is_none_or(|count| count > MAX_PREVIOUS_BLOCKS)
    {
        return invalid("manual placement block count exceeds the scheduling limit");
    }
    Ok(())
}

fn validate_manual_placement_releases(
    request: &ComposeScheduleRequest,
) -> Result<(), PrepareScheduleError> {
    let mut release_ids = BTreeSet::new();
    let mut released_placement_ids = BTreeSet::new();
    for release in &request.manual_placement_releases {
        if release.id.is_nil()
            || release.placement_id.is_nil()
            || release.source_schedule_revision_id.is_nil()
            || !release_ids.insert(release.id)
            || !released_placement_ids.insert(release.placement_id)
        {
            return invalid(
                "manual placement releases require unique non-empty command and placement ids",
            );
        }
    }
    Ok(())
}

/// Converts a complete active canonical snapshot into one normalized core
/// request without running the scheduler.
///
/// Source items and all result evidence are canonicalized independently of the
/// caller's input order. Malformed scheduling metadata is isolated as a
/// rejected item; malformed request-level input fails the complete operation.
///
/// # Errors
///
/// Returns an error for an invalid request, an oversized or duplicate snapshot,
/// invalid recurrence references, timestamp conversion failures, or bounded
/// accounting overflow.
#[allow(clippy::too_many_lines)]
pub fn prepare_canonical_schedule(
    mut source_items: Vec<CanonicalItem>,
    request: ComposeScheduleRequest,
) -> Result<PreparedSchedule, PrepareScheduleError> {
    validate_schedule_request(&request)?;
    if source_items.len() > MAX_CANONICAL_ITEMS {
        return Err(PrepareScheduleError::TooManyItems);
    }
    source_items.sort_by_key(|item| item.id);
    for pair in source_items.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(PrepareScheduleError::DuplicateCanonicalItem(pair[0].id));
        }
    }
    for item in &source_items {
        validate_canonical_item(item)?;
    }

    let source_item_count = source_items.len();
    let source_item_revisions = source_items
        .iter()
        .map(|item| (item.id, item.revision))
        .collect();
    let effective_sensitivity = effective_sensitivity_by_item(&source_items);
    let inbox_subtree_item_ids = inbox_subtree_item_ids(&source_items);
    let mut rejected_items = Vec::new();
    let mut accepted = Vec::with_capacity(source_items.len());
    let mut accepted_without_work_count = 0_usize;
    for item in &source_items {
        if inbox_subtree_item_ids.contains(&item.id) {
            accepted_without_work_count = accepted_without_work_count
                .checked_add(1)
                .ok_or(PrepareScheduleError::AccountingOverflow)?;
            continue;
        }
        let is_sensitive = effective_sensitivity.get(&item.id).copied().unwrap_or(true);
        match classify_item(item, is_sensitive) {
            Ok(MappedScheduleItem::Plannable(mapped)) => accepted.push(*mapped),
            Ok(MappedScheduleItem::ContextOnly) => {
                accepted_without_work_count = accepted_without_work_count
                    .checked_add(1)
                    .ok_or(PrepareScheduleError::AccountingOverflow)?;
            }
            Err(reason) => rejected_items.push(RejectedScheduleItem {
                item_id: item.id,
                is_sensitive,
                title: item.title.clone(),
                reason,
            }),
        }
    }
    prune_orphaned_items(&mut accepted, &mut rejected_items);
    accepted.sort_by_key(|item| item.id);
    rejected_items.sort_by_key(|item| item.item_id);

    let revisions: BTreeMap<_, _> = accepted
        .iter()
        .map(|item| (item.id, item.revision))
        .collect();
    let manual_placements = request.manual_placements;
    let manual_placement_releases = request.manual_placement_releases;
    let mut recurrence_context = request.recurrence_context;
    remove_inbox_subtree_recurrence_references(&mut recurrence_context, &inbox_subtree_item_ids);
    validate_recurrence_context_references(&recurrence_context, &revisions)?;
    let planning_timezone: Tz = request
        .timezone_name
        .parse()
        .map_err(|_| PrepareScheduleError::InvalidRequest("invalid timezone_name".into()))?;
    let (mut previous_assignments, ignored_previous_assignments) =
        map_previous_assignments(&request.previous_assignments, &revisions, planning_timezone)?;
    map_manual_placements(
        &manual_placements,
        &revisions,
        planning_timezone,
        &mut previous_assignments,
    )?;

    let horizon_start = to_time_in_timezone(request.horizon_start, planning_timezone)?;
    let horizon_end = to_time_in_timezone(request.horizon_end, planning_timezone)?;
    populate_recurrence_calendar(
        &mut recurrence_context,
        &request.timezone_name,
        request.horizon_start,
        request.horizon_end,
    )?;
    let plan_request = PlanRequest {
        as_of: to_time_in_timezone(request.as_of, planning_timezone)?,
        horizon_start,
        horizon_end,
        items: accepted,
        availability: request
            .availability
            .into_iter()
            .map(|input| map_availability(input, planning_timezone))
            .collect::<Result<_, _>>()?,
        fixed_blocks: request
            .fixed_blocks
            .into_iter()
            .map(|input| map_fixed_block(input, planning_timezone))
            .collect::<Result<_, _>>()?,
        previous_assignments,
        config: SchedulerConfig {
            slot_granularity: Minutes(request.config.slot_granularity_minutes),
            stability_weight: request.config.stability_weight,
            default_soft_weight: request.config.default_soft_weight,
        },
        recurrence_context,
    };

    let accepted_item_count = plan_request
        .items
        .len()
        .checked_add(accepted_without_work_count)
        .ok_or(PrepareScheduleError::AccountingOverflow)?;
    if accepted_item_count.checked_add(rejected_items.len()) != Some(source_item_count) {
        return Err(PrepareScheduleError::AccountingOverflow);
    }

    Ok(PreparedSchedule {
        timezone_name: request.timezone_name,
        source_item_count,
        source_item_revisions,
        effective_sensitivity,
        accepted_item_count,
        rejected_items,
        ignored_previous_assignments,
        manual_placements,
        manual_placement_releases,
        plan_request,
    })
}

/// Re-establishes the storage invariants that the server repository normally
/// guarantees. A local process boundary must not treat a corrupt cached DTO as
/// an authoritative canonical snapshot.
fn validate_canonical_item(item: &CanonicalItem) -> Result<(), PrepareScheduleError> {
    let recurrence_is_valid = item.recurrence.as_ref().is_none_or(|value| {
        value.is_object()
            && serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= MAX_RECURRENCE_BYTES)
    });
    let constraints_are_valid = item.flexible_constraints.is_object()
        && serde_json::to_vec(&item.flexible_constraints)
            .is_ok_and(|encoded| encoded.len() <= MAX_SCHEDULING_METADATA_BYTES);
    let split_is_valid = match item.split_policy {
        CanonicalSplitPolicy::Indivisible => true,
        CanonicalSplitPolicy::Splittable {
            minimum_chunk_seconds,
            maximum_chunk_seconds,
        } => item.duration_seconds.is_some_and(|duration| {
            minimum_chunk_seconds > 0
                && maximum_chunk_seconds >= minimum_chunk_seconds
                && minimum_chunk_seconds <= duration
                && maximum_chunk_seconds <= duration
        }),
    };
    let valid = item.deleted_at.is_none()
        && item.revision > 0
        && !item.title.is_empty()
        && item.title == item.title.trim()
        && item.title.chars().count() <= MAX_BLOCK_TITLE_CHARACTERS
        && item
            .notes
            .as_ref()
            .is_none_or(|notes| notes.chars().count() <= MAX_NOTES_CHARACTERS)
        && item.timezone_name.parse::<Tz>().is_ok()
        && item
            .duration_seconds
            .is_none_or(|value| (1..=MAX_DURATION_SECONDS).contains(&value))
        && item
            .earliest_start_at
            .zip(item.deadline_at)
            .is_none_or(|(earliest, deadline)| earliest < deadline)
        && recurrence_is_valid
        && constraints_are_valid
        && split_is_valid
        && item.importance <= 100
        && item.urgency <= 100
        && item.sibling_order <= MAX_SIBLING_ORDER;
    if valid {
        Ok(())
    } else {
        Err(PrepareScheduleError::InvalidCanonicalItem(item.id))
    }
}

/// Resolves sensitivity without recursion. A missing ancestor or hierarchy
/// cycle fails closed for the entire unresolved path.
fn effective_sensitivity_by_item(items: &[CanonicalItem]) -> BTreeMap<Uuid, bool> {
    let by_id: BTreeMap<_, _> = items.iter().map(|item| (item.id, item)).collect();
    let mut resolved = BTreeMap::new();
    for start in by_id.keys().copied() {
        if resolved.contains_key(&start) {
            continue;
        }
        let mut path = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut current = Some(start);
        let inherited = loop {
            let Some(id) = current else {
                break false;
            };
            if let Some(value) = resolved.get(&id) {
                break *value;
            }
            let Some(item) = by_id.get(&id) else {
                break true;
            };
            if !visiting.insert(id) {
                break true;
            }
            path.push(id);
            current = item.parent_id;
        };
        let mut inherited = inherited;
        for id in path.into_iter().rev() {
            let value = by_id
                .get(&id)
                .is_none_or(|item| item.is_sensitive || inherited);
            resolved.insert(id, value);
            inherited = value;
        }
    }
    resolved
}

fn validate_recurrence_context_references(
    context: &RecurrenceContext,
    revisions: &BTreeMap<ItemId, u64>,
) -> Result<(), PrepareScheduleError> {
    let mut referenced = BTreeSet::new();
    referenced.extend(context.completion_anchors.keys().copied());
    referenced.extend(context.rolling_anchors.keys().copied());
    referenced.extend(context.minimum_spacing.keys().copied());
    referenced.extend(context.pauses.iter().map(|pause| pause.item_id));
    referenced.extend(context.exceptions.iter().map(|exception| exception.item_id));
    if let Some(missing) = referenced
        .into_iter()
        .find(|item_id| !revisions.contains_key(item_id))
    {
        return invalid(format!(
            "recurrence_context references unavailable item {missing}"
        ));
    }
    Ok(())
}

fn remove_inbox_subtree_recurrence_references(
    context: &mut RecurrenceContext,
    inbox_subtree_item_ids: &BTreeSet<Uuid>,
) {
    let is_retained = |item_id: &ItemId| !inbox_subtree_item_ids.contains(&item_id.0);
    context
        .completion_anchors
        .retain(|item_id, _| is_retained(item_id));
    context
        .rolling_anchors
        .retain(|item_id, _| is_retained(item_id));
    context
        .minimum_spacing
        .retain(|item_id, _| is_retained(item_id));
    context.pauses.retain(|pause| is_retained(&pause.item_id));
    context
        .exceptions
        .retain(|exception| is_retained(&exception.item_id));
}

enum MappedScheduleItem {
    Plannable(Box<WorkItem>),
    ContextOnly,
}

fn classify_item(item: &CanonicalItem, is_sensitive: bool) -> Result<MappedScheduleItem, String> {
    let item_timezone: Tz = item
        .timezone_name
        .parse()
        .map_err(|_| "canonical item timezone is invalid".to_owned())?;
    let validated = validate_scheduling_metadata(SchedulingMetadataInput {
        item_id: item.id,
        kind: item.kind,
        status: item.status,
        timezone_name: &item.timezone_name,
        duration_seconds: item.duration_seconds,
        deadline_at: item.deadline_at,
        earliest_start_at: item.earliest_start_at,
        recurrence: item.recurrence.as_ref(),
        flexible_constraints: &item.flexible_constraints,
        split_policy: &item.split_policy,
        parent_id: item.parent_id,
    })
    .map_err(|error| error.to_string())?;
    let metadata = validated.metadata;
    let recurrence = validated.recurrence;
    if metadata.calendar_context.is_some() {
        return Ok(MappedScheduleItem::ContextOnly);
    }
    let duration = item.duration_seconds.map(duration_estimate);
    let mut constraints = metadata.constraints.clone();
    if let Some(earliest) = item.earliest_start_at {
        if constraints.earliest_start.is_some() {
            return Err(
                "earliest start is defined in both the canonical field and metadata".into(),
            );
        }
        constraints.earliest_start = Some(Qualified::hard(
            to_time_in_timezone(earliest, item_timezone).map_err(|error| error.to_string())?,
        ));
    }
    if let Some(deadline) = item.deadline_at {
        if constraints.latest_finish.is_some() {
            return Err("deadline is defined in both the canonical field and metadata".into());
        }
        constraints.latest_finish = Some(Qualified::hard(
            to_time_in_timezone(deadline, item_timezone).map_err(|error| error.to_string())?,
        ));
    }
    if let Some(preferred_start) = metadata.preferred_start_minute {
        add_legacy_preferred_window(
            &mut constraints,
            preferred_start,
            duration,
            item.duration_seconds,
        )?;
    }

    let kind = map_kind(item.kind, recurrence, &metadata)?;
    let split_policy = map_split_policy(&item.split_policy, &metadata);
    Ok(MappedScheduleItem::Plannable(Box::new(WorkItem {
        id: ItemId(item.id),
        is_sensitive,
        revision: item.revision,
        title: item.title.clone(),
        kind,
        status: map_status(item.status),
        parent_id: item.parent_id.map(ItemId),
        sibling_order: Some(item.sibling_order),
        has_own_effort: metadata.has_own_effort,
        goal_ids: metadata.goal_ids.into_iter().map(ItemId).collect(),
        priority: Priority {
            importance: normalize_priority(item.importance),
            urgency: normalize_priority(item.urgency),
        },
        duration: if item.kind == CanonicalItemKind::Event {
            None
        } else {
            duration
        },
        constraints,
        split_policy,
        energy: metadata.energy.map(EnergyMetadata::into_qualified),
        tags: metadata.tags,
        created_at: to_time_in_timezone(item.created_at, item_timezone)
            .map_err(|error| error.to_string())?,
        updated_at: to_time_in_timezone(item.updated_at, item_timezone)
            .map_err(|error| error.to_string())?,
    })))
}

fn map_kind(
    kind: CanonicalItemKind,
    recurrence: Option<Recurrence>,
    metadata: &SchedulingMetadata,
) -> Result<PlanningItemKind, String> {
    match kind {
        CanonicalItemKind::Event => {
            if recurrence.is_some() {
                return Err(
                    "calendar event recurrence must be expanded by its calendar source".into(),
                );
            }
            let event = match (
                metadata.calendar_event.clone(),
                metadata.dayweave_firm_block.as_ref(),
            ) {
                (Some(event), None) => event,
                (None, Some(firm)) => firm
                    .as_calendar_event()
                    .map_err(|error| error.to_string())?,
                (Some(_), Some(_)) => {
                    return Err(
                        "event metadata cannot combine calendar_event and dayweave_firm_block"
                            .into(),
                    );
                }
                (None, None) => {
                    return Err(
                        "event metadata requires calendar_event or dayweave_firm_block".into(),
                    );
                }
            };
            Ok(PlanningItemKind::CalendarEvent(event))
        }
        CanonicalItemKind::Task => Ok(recurrence.map_or(PlanningItemKind::Task, |recurrence| {
            PlanningItemKind::RecurringTask(RecurringTaskSpec { recurrence })
        })),
        CanonicalItemKind::Habit => recurrence
            .map(|recurrence| {
                PlanningItemKind::Habit(HabitSpec {
                    recurrence,
                    target: metadata.habit_target.clone(),
                    preserves_streak_when_paused: metadata.preserves_streak_when_paused,
                })
            })
            .ok_or_else(|| "habit requires recurrence".into()),
        CanonicalItemKind::Routine => Ok(PlanningItemKind::Routine(RoutineSpec {
            ordered: metadata.routine_ordered,
            recurrence,
        })),
        CanonicalItemKind::Goal => {
            reject_recurrence(recurrence.as_ref(), "goal")?;
            Ok(PlanningItemKind::Goal(GoalSpec {
                measures: metadata.goal_measures.clone(),
                weekly_allocation: metadata.goal_weekly_allocation,
            }))
        }
        CanonicalItemKind::Break => {
            reject_recurrence(recurrence.as_ref(), "break")?;
            Ok(PlanningItemKind::Break(BreakSpec {
                category: metadata.break_category.unwrap_or(BreakCategory::Other),
                mandatory: metadata.break_mandatory,
                prompt_to_resume: metadata.break_prompt_to_resume,
            }))
        }
    }
}

fn reject_recurrence(recurrence: Option<&Recurrence>, kind: &str) -> Result<(), String> {
    if recurrence.is_some() {
        Err(format!(
            "{kind} does not support recurrence; use a routine or habit"
        ))
    } else {
        Ok(())
    }
}

fn map_split_policy(
    policy: &CanonicalSplitPolicy,
    metadata: &SchedulingMetadata,
) -> PlanningSplitPolicy {
    match policy {
        CanonicalSplitPolicy::Indivisible => PlanningSplitPolicy::Indivisible,
        CanonicalSplitPolicy::Splittable {
            minimum_chunk_seconds,
            maximum_chunk_seconds,
        } => PlanningSplitPolicy::Splittable {
            minimum_session: seconds_to_minutes(*minimum_chunk_seconds),
            maximum_session: seconds_to_minutes(*maximum_chunk_seconds),
            maximum_sessions: metadata.maximum_sessions.unwrap_or(u16::MAX),
            minimum_gap: Minutes(metadata.minimum_gap_minutes),
            maximum_days: metadata.maximum_split_days,
        },
    }
}

fn duration_estimate(seconds: u32) -> DurationEstimate {
    DurationEstimate::exact(seconds_to_minutes(seconds).get())
}

const fn seconds_to_minutes(seconds: u32) -> Minutes {
    Minutes(seconds.saturating_add(59) / 60)
}

const fn normalize_priority(value: u8) -> u8 {
    value.saturating_add(9) / 10
}

const fn map_status(status: CanonicalItemStatus) -> WorkStatus {
    match status {
        CanonicalItemStatus::Inbox | CanonicalItemStatus::Planned => WorkStatus::NotStarted,
        CanonicalItemStatus::Scheduled => WorkStatus::Scheduled,
        CanonicalItemStatus::InProgress => WorkStatus::Active,
        CanonicalItemStatus::Paused => WorkStatus::Paused,
        CanonicalItemStatus::Completed => WorkStatus::Completed,
        CanonicalItemStatus::Skipped => WorkStatus::Skipped,
        CanonicalItemStatus::Cancelled => WorkStatus::Canceled,
    }
}

fn add_legacy_preferred_window(
    constraints: &mut SchedulingConstraints,
    start_minute: u16,
    duration: Option<DurationEstimate>,
    duration_seconds: Option<u32>,
) -> Result<(), String> {
    if start_minute > 1_439 {
        return Err("preferred_start_minute must be in 0..=1439".into());
    }
    let duration_minutes = duration.map_or(1, |value| value.expected.get());
    let end = u32::from(start_minute).saturating_add(duration_minutes);
    if end > 1_440 || duration_seconds.is_none() {
        return Err("preferred_start_minute requires a duration that finishes the same day".into());
    }
    constraints.preferred_daily_windows.push(Qualified::soft(
        DailyTimeWindow {
            weekdays: BTreeSet::new(),
            start_minute,
            end_minute: u16::try_from(end).map_err(|_| "invalid preferred window")?,
        },
        100,
    ));
    Ok(())
}

fn inbox_subtree_item_ids(items: &[CanonicalItem]) -> BTreeSet<Uuid> {
    let mut children_by_parent = BTreeMap::<Uuid, Vec<Uuid>>::new();
    let mut excluded = BTreeSet::new();
    let mut frontier = VecDeque::new();
    for item in items {
        if let Some(parent_id) = item.parent_id {
            children_by_parent
                .entry(parent_id)
                .or_default()
                .push(item.id);
        }
        if item.status == CanonicalItemStatus::Inbox && excluded.insert(item.id) {
            frontier.push_back(item.id);
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_unstable();
    }
    for _ in 0..items.len() {
        let Some(parent_id) = frontier.pop_front() else {
            break;
        };
        if let Some(children) = children_by_parent.get(&parent_id) {
            for child_id in children {
                if excluded.insert(*child_id) {
                    frontier.push_back(*child_id);
                }
            }
        }
    }
    debug_assert!(frontier.is_empty());
    excluded
}

fn prune_orphaned_items(items: &mut Vec<WorkItem>, rejected: &mut Vec<RejectedScheduleItem>) {
    let all_ids: BTreeSet<_> = items.iter().map(|item| item.id).collect();
    let mut children = BTreeMap::<ItemId, BTreeSet<ItemId>>::new();
    let mut frontier = BTreeSet::new();
    for item in items.iter() {
        if let Some(parent_id) = item.parent_id {
            children.entry(parent_id).or_default().insert(item.id);
            if !all_ids.contains(&parent_id) {
                frontier.insert(item.id);
            }
        }
    }
    let mut retained: BTreeMap<_, _> = items.drain(..).map(|item| (item.id, item)).collect();
    while let Some(item_id) = frontier.pop_first() {
        let Some(item) = retained.remove(&item_id) else {
            continue;
        };
        let Some(parent_id) = item.parent_id else {
            continue;
        };
        rejected.push(RejectedScheduleItem {
            item_id: item.id.0,
            is_sensitive: item.is_sensitive,
            title: item.title,
            reason: format!("parent {parent_id} is unavailable for scheduling"),
        });
        if let Some(descendants) = children.get(&item_id) {
            frontier.extend(descendants.iter().copied());
        }
    }
    items.extend(retained.into_values());
}

fn map_previous_assignments(
    assignments: &[PreviousAssignmentInput],
    revisions: &BTreeMap<ItemId, u64>,
    timezone: Tz,
) -> Result<(Vec<PreviousAssignment>, Vec<IgnoredPreviousAssignment>), PrepareScheduleError> {
    let mut seen = BTreeSet::new();
    let mut accepted = Vec::new();
    let mut ignored = Vec::new();
    for assignment in assignments {
        let item_id = ItemId(assignment.item_id);
        if !seen.insert((item_id, assignment.occurrence_id)) {
            return invalid(format!(
                "duplicate previous assignment for item {} and occurrence",
                assignment.item_id
            ));
        }
        let current_revision = revisions.get(&item_id).copied();
        if current_revision != Some(assignment.item_revision) {
            ignored.push(IgnoredPreviousAssignment {
                item_id: assignment.item_id,
                requested_revision: assignment.item_revision,
                current_revision,
                reason: if current_revision.is_some() {
                    "canonical item revision changed".into()
                } else {
                    "canonical item is unavailable for scheduling".into()
                },
            });
            continue;
        }
        accepted.push(PreviousAssignment {
            item_id,
            occurrence_id: assignment.occurrence_id.map(OccurrenceId),
            blocks: assignment
                .blocks
                .iter()
                .map(|block| {
                    Ok(PreviousBlock {
                        start: to_time_in_timezone(block.start, timezone)?,
                        end: to_time_in_timezone(block.end, timezone)?,
                        session_index: block.session_index,
                    })
                })
                .collect::<Result<_, PrepareScheduleError>>()?,
            pinned: assignment.pinned,
            manual_placement_id: None,
        });
    }
    Ok((accepted, ignored))
}

fn map_manual_placements(
    placements: &[ManualPlacementInput],
    revisions: &BTreeMap<ItemId, u64>,
    timezone: Tz,
    assignments: &mut Vec<PreviousAssignment>,
) -> Result<(), PrepareScheduleError> {
    for placement in placements {
        for requested in &placement.assignments {
            let item_id = ItemId(requested.item_id);
            let current_revision = revisions.get(&item_id).copied();
            if current_revision != Some(requested.item_revision) {
                return invalid(format!(
                    "manual placement {} references a stale or unavailable item {}",
                    placement.id, requested.item_id
                ));
            }
            let occurrence_id = requested.occurrence_id.map(OccurrenceId);
            assignments.retain(|assignment| {
                (assignment.item_id, assignment.occurrence_id) != (item_id, occurrence_id)
            });
            assignments.push(PreviousAssignment {
                item_id,
                occurrence_id,
                blocks: requested
                    .blocks
                    .iter()
                    .map(|block| {
                        Ok(PreviousBlock {
                            start: to_time_in_timezone(block.start, timezone)?,
                            end: to_time_in_timezone(block.end, timezone)?,
                            session_index: block.session_index,
                        })
                    })
                    .collect::<Result<_, PrepareScheduleError>>()?,
                pinned: true,
                manual_placement_id: Some(placement.id),
            });
        }
    }
    Ok(())
}

fn map_availability(
    input: AvailabilityInput,
    timezone: Tz,
) -> Result<AvailabilityWindow, PrepareScheduleError> {
    Ok(AvailabilityWindow {
        start: to_time_in_timezone(input.start, timezone)?,
        end: to_time_in_timezone(input.end, timezone)?,
        contexts: input.contexts,
        location: input.location,
        energy: match input.energy {
            EnergyInput::Low => EnergyLevel::Low,
            EnergyInput::Medium => EnergyLevel::Medium,
            EnergyInput::Deep => EnergyLevel::Deep,
        },
    })
}

fn map_fixed_block(
    input: FixedBlockInput,
    timezone: Tz,
) -> Result<FixedBlock, PrepareScheduleError> {
    Ok(FixedBlock {
        id: input.id,
        is_sensitive: input.is_sensitive,
        title: input.title,
        start: to_time_in_timezone(input.start, timezone)?,
        end: to_time_in_timezone(input.end, timezone)?,
        source: match input.source {
            FixedBlockSourceInput::GoogleCalendar => FixedBlockSource::GoogleCalendar,
            FixedBlockSourceInput::Sleep => FixedBlockSource::Sleep,
            FixedBlockSourceInput::ProtectedTime => FixedBlockSource::ProtectedTime,
            FixedBlockSourceInput::Travel => FixedBlockSource::Travel,
            FixedBlockSourceInput::Manual => FixedBlockSource::Manual,
        },
    })
}

fn populate_recurrence_calendar(
    context: &mut RecurrenceContext,
    timezone_name: &str,
    horizon_start: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
) -> Result<(), PrepareScheduleError> {
    if !context.calendar.days.is_empty() {
        if context
            .calendar
            .time_zone_id
            .as_deref()
            .is_some_and(|value| value != timezone_name)
        {
            return invalid("recurrence calendar timezone does not match timezone_name");
        }
        context.calendar.time_zone_id = Some(timezone_name.to_owned());
        return Ok(());
    }
    let timezone: Tz = timezone_name
        .parse()
        .map_err(|_| PrepareScheduleError::InvalidRequest("invalid timezone_name".into()))?;
    let mut date = horizon_start.with_timezone(&timezone).date_naive();
    let mut days = Vec::new();
    loop {
        let next = date
            .succ_opt()
            .ok_or_else(|| PrepareScheduleError::InvalidRequest("horizon date overflow".into()))?;
        let start = zoned_midnight(timezone, date)?;
        let end = zoned_midnight(timezone, next)?;
        if start < to_time(horizon_end)? && end > to_time(horizon_start)? {
            days.push(ZonedDayBoundary {
                local_date: time::Date::from_calendar_date(
                    date.year(),
                    time::Month::try_from(u8::try_from(date.month()).map_err(|_| {
                        PrepareScheduleError::InvalidRequest("invalid local month".into())
                    })?)
                    .map_err(|_| {
                        PrepareScheduleError::InvalidRequest("invalid local month".into())
                    })?,
                    u8::try_from(date.day()).map_err(|_| {
                        PrepareScheduleError::InvalidRequest("invalid local day".into())
                    })?,
                )
                .map_err(|_| PrepareScheduleError::InvalidRequest("invalid local date".into()))?,
                start,
                end,
            });
        }
        if end >= to_time(horizon_end)? {
            break;
        }
        date = next;
        if days.len() > MAX_CALENDAR_DAYS {
            return invalid("generated recurrence calendar exceeded horizon bounds");
        }
    }
    context.calendar.time_zone_id = Some(timezone_name.to_owned());
    context.calendar.days = days;
    Ok(())
}

fn zoned_midnight(timezone: Tz, date: NaiveDate) -> Result<OffsetDateTime, PrepareScheduleError> {
    let local = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| PrepareScheduleError::InvalidRequest("invalid local midnight".into()))?;
    let resolved = match timezone.from_local_datetime(&local) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(first, second) => first.min(second),
        LocalResult::None => {
            return invalid(format!(
                "timezone {timezone} has no representable midnight on {date}"
            ));
        }
    };
    let utc = resolved.with_timezone(&Utc);
    let offset = UtcOffset::from_whole_seconds(resolved.offset().fix().local_minus_utc())
        .map_err(|_| PrepareScheduleError::InvalidRequest("timezone offset is invalid".into()))?;
    Ok(to_time(utc)?.to_offset(offset))
}

fn to_time(value: DateTime<Utc>) -> Result<OffsetDateTime, PrepareScheduleError> {
    OffsetDateTime::from_unix_timestamp(value.timestamp())
        .and_then(|instant| instant.replace_nanosecond(value.timestamp_subsec_nanos()))
        .map_err(|_| {
            PrepareScheduleError::InvalidRequest("timestamp is outside supported range".into())
        })
}

fn to_time_in_timezone(
    value: DateTime<Utc>,
    timezone: Tz,
) -> Result<OffsetDateTime, PrepareScheduleError> {
    let localized = value.with_timezone(&timezone);
    let offset = UtcOffset::from_whole_seconds(localized.offset().fix().local_minus_utc())
        .map_err(|_| PrepareScheduleError::InvalidRequest("timezone offset is invalid".into()))?;
    Ok(to_time(value)?.to_offset(offset))
}

fn invalid<T>(message: impl Into<String>) -> Result<T, PrepareScheduleError> {
    Err(PrepareScheduleError::InvalidRequest(message.into()))
}
