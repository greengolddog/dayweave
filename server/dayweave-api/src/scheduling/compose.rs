use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    ops::Deref,
};

use chrono::{DateTime, Datelike as _, LocalResult, NaiveDate, Offset as _, TimeZone as _, Utc};
use chrono_tz::Tz;
use dayweave_core::{
    AllocationRange, AvailabilityWindow, BreakCategory, BreakSpec, CalendarEventSpec,
    DailyTimeWindow, DurationEstimate, EnergyLevel, FixedBlock, FixedBlockSource, GoalMeasure,
    GoalSpec, HabitSpec, ItemId, ItemKind as PlanningItemKind, Minutes, OccurrenceId, PlanRequest,
    PreviousAssignment, PreviousBlock, Priority, Qualified, QuantityTarget, Recurrence,
    RecurrenceContext, RecurrenceExceptionAction, RecurrenceExceptionSelector, RecurringTaskSpec,
    RoutineSpec, ScheduleError, SchedulePlan, Scheduler, SchedulerConfig, SchedulingConstraints,
    SplitPolicy as PlanningSplitPolicy, WorkItem, WorkStatus, ZonedDayBoundary,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::items::{
    Item, ItemKind, ItemQuery, ItemService, ItemServiceError, ItemStatus, SplitPolicy,
};

use super::{
    CalendarProjectionFenceError, CalendarProjectionStamp, postgres::PostgresSchedulingRepository,
};

const MAX_CANONICAL_ITEMS: usize = 10_000;
const MAX_AVAILABILITY_WINDOWS: usize = 10_000;
const MAX_FIXED_BLOCKS: usize = 10_000;
const MAX_PREVIOUS_ASSIGNMENTS: usize = 10_000;
const MAX_PREVIOUS_BLOCKS: usize = 50_000;
const MAX_RECURRENCE_CONTEXT_ENTRIES: usize = 10_000;
const MAX_HORIZON_DAYS: i64 = 90;
const MAX_CALENDAR_DAYS: usize = 92;
const MAX_WEIGHT: u32 = 1_000_000;
const MAX_PERSISTED_BLOCK_TITLE_CHARACTERS: usize = 500;

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposeScheduleRequest {
    pub as_of: DateTime<Utc>,
    pub horizon_start: DateTime<Utc>,
    pub horizon_end: DateTime<Utc>,
    pub timezone_name: String,
    #[serde(default)]
    pub availability: Vec<AvailabilityInput>,
    #[serde(default)]
    pub fixed_blocks: Vec<FixedBlockInput>,
    #[serde(default)]
    pub previous_assignments: Vec<PreviousAssignmentInput>,
    #[serde(default)]
    pub config: SchedulerConfigInput,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub recurrence_context: RecurrenceContext,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityInput {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    #[serde(default)]
    pub contexts: BTreeSet<String>,
    pub location: Option<String>,
    #[serde(default)]
    pub energy: EnergyInput,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnergyInput {
    Low,
    #[default]
    Medium,
    Deep,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FixedBlockInput {
    pub id: Uuid,
    pub is_sensitive: bool,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub source: FixedBlockSourceInput,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FixedBlockSourceInput {
    GoogleCalendar,
    Sleep,
    ProtectedTime,
    Travel,
    Manual,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviousAssignmentInput {
    pub item_id: Uuid,
    pub item_revision: u64,
    pub occurrence_id: Option<Uuid>,
    #[serde(default)]
    pub blocks: Vec<PreviousBlockInput>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviousBlockInput {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub session_index: u16,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfigInput {
    pub slot_granularity_minutes: u32,
    pub stability_weight: u32,
    pub default_soft_weight: u32,
}

impl Default for SchedulerConfigInput {
    fn default() -> Self {
        Self {
            slot_granularity_minutes: 5,
            stability_weight: 4,
            default_soft_weight: 100,
        }
    }
}

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
    /// Canonical items understood by this scheduler schema. This includes
    /// retained nonblocking calendar context even though it emits no work item
    /// or schedule block.
    pub accepted_item_count: usize,
    pub rejected_items: Vec<RejectedScheduleItem>,
    pub ignored_previous_assignments: Vec<IgnoredPreviousAssignment>,
    #[schema(value_type = Object)]
    pub plan: Rfc3339SchedulePlan,
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

#[derive(Debug, Clone, Eq, PartialEq, Serialize, ToSchema)]
pub struct RejectedScheduleItem {
    pub item_id: Uuid,
    pub is_sensitive: bool,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, ToSchema)]
pub struct IgnoredPreviousAssignment {
    pub item_id: Uuid,
    pub requested_revision: u64,
    pub current_revision: Option<u64>,
    pub reason: String,
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
}

impl ComposeScheduleError {
    #[must_use]
    pub const fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidRequest(_) | Self::TooManyItems | Self::Scheduler(_)
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
    request: ComposeScheduleRequest,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    validate_request_shape(&request)?;
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
    compose_items_with_projection_for_schema(
        items,
        request,
        super::SCHEDULER_PUBLICATION_SCHEMA,
        projection_before,
    )
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
    request: ComposeScheduleRequest,
    scheduler_publication_schema: &str,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    compose_items_with_projection_for_schema(
        source_items,
        request,
        scheduler_publication_schema,
        Vec::new(),
    )
}

#[allow(clippy::too_many_lines)] // One pipeline keeps snapshot counts, digest, and plan atomic.
fn compose_items_with_projection_for_schema(
    source_items: Vec<Item>,
    request: ComposeScheduleRequest,
    scheduler_publication_schema: &str,
    calendar_projection_stamps: Vec<CalendarProjectionStamp>,
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
    let publication_timezone = request.timezone_name.clone();
    let source_item_count = source_items.len();
    let source_item_revisions = source_items
        .iter()
        .map(|item| (item.id, item.revision))
        .collect();
    let effective_sensitivity = effective_sensitivity_by_item(&source_items);
    let mut rejected_items = Vec::new();
    let mut accepted = Vec::with_capacity(source_items.len());
    let mut context_only_count = 0_usize;
    for item in source_items {
        let is_sensitive = effective_sensitivity.get(&item.id).copied().unwrap_or(true);
        match classify_item(&item, is_sensitive) {
            Ok(MappedScheduleItem::Plannable(mapped)) => accepted.push(*mapped),
            Ok(MappedScheduleItem::ContextOnly) => {
                context_only_count = context_only_count
                    .checked_add(1)
                    .ok_or(ComposeScheduleError::Encoding)?;
            }
            Err(reason) => rejected_items.push(RejectedScheduleItem {
                item_id: item.id,
                is_sensitive,
                title: item.title,
                reason,
            }),
        }
    }
    prune_orphaned_items(&mut accepted, &mut rejected_items);
    accepted.sort_by_key(|item| item.id);

    let revisions: BTreeMap<_, _> = accepted
        .iter()
        .map(|item| (item.id, item.revision))
        .collect();
    validate_recurrence_context_references(&request.recurrence_context, &revisions)?;
    let planning_timezone: Tz = request
        .timezone_name
        .parse()
        .map_err(|_| ComposeScheduleError::InvalidRequest("invalid timezone_name".into()))?;
    let (previous_assignments, ignored_previous_assignments) =
        map_previous_assignments(&request.previous_assignments, &revisions, planning_timezone)?;

    let horizon_start = to_time_in_timezone(request.horizon_start, planning_timezone)?;
    let horizon_end = to_time_in_timezone(request.horizon_end, planning_timezone)?;
    let mut recurrence_context = request.recurrence_context;
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

    let input_digest = request_digest(
        scheduler_publication_schema,
        &request.timezone_name,
        &source_item_revisions,
        &calendar_projection_stamps,
        &plan_request,
    )?;
    let plan = Scheduler.plan(&plan_request)?;
    let accepted_item_count = plan_request
        .items
        .len()
        .checked_add(context_only_count)
        .ok_or(ComposeScheduleError::Encoding)?;
    if accepted_item_count.checked_add(rejected_items.len()) != Some(source_item_count) {
        return Err(ComposeScheduleError::Encoding);
    }
    let result = ComposeScheduleResult {
        input_digest,
        source_item_count,
        source_item_revisions,
        source_item_sensitivity: effective_sensitivity,
        calendar_projection_stamps,
        accepted_item_count,
        rejected_items,
        ignored_previous_assignments,
        plan: Rfc3339SchedulePlan(plan),
    };
    super::postgres::validate_publishable_compose_result(&publication_timezone, &result).map_err(
        |_| {
            ComposeScheduleError::InvalidRequest(
                "composed schedule exceeds the durable publication contract".to_owned(),
            )
        },
    )?;
    Ok(result)
}

/// Resolves effective sensitivity over the already-validated canonical tree.
/// Missing ancestors or a corrupted cycle fail closed without affecting the
/// scheduler's placement inputs.
fn effective_sensitivity_by_item(items: &[Item]) -> BTreeMap<Uuid, bool> {
    fn resolve(
        id: Uuid,
        items: &BTreeMap<Uuid, &Item>,
        resolved: &mut BTreeMap<Uuid, bool>,
        visiting: &mut BTreeSet<Uuid>,
    ) -> bool {
        if let Some(value) = resolved.get(&id) {
            return *value;
        }
        let Some(item) = items.get(&id) else {
            return true;
        };
        if !visiting.insert(id) {
            return true;
        }
        let inherited = item
            .parent_id
            .is_some_and(|parent_id| resolve(parent_id, items, resolved, visiting));
        visiting.remove(&id);
        let value = item.is_sensitive || inherited;
        resolved.insert(id, value);
        value
    }

    let by_id: BTreeMap<_, _> = items.iter().map(|item| (item.id, item)).collect();
    let mut resolved = BTreeMap::new();
    for id in by_id.keys().copied() {
        resolve(id, &by_id, &mut resolved, &mut BTreeSet::new());
    }
    resolved
}

#[allow(clippy::too_many_lines)] // One validation pass keeps preview and publication acceptance identical.
fn validate_request_shape(request: &ComposeScheduleRequest) -> Result<(), ComposeScheduleError> {
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
                        RecurrenceExceptionAction::Move { start, end } => {
                            offset_instant_is_precise(start) && offset_instant_is_precise(end)
                        }
                        RecurrenceExceptionAction::Skip => true,
                    };
                    selector && action
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
            || block.title.chars().count() > MAX_PERSISTED_BLOCK_TITLE_CHARACTERS
            || block.title.chars().any(char::is_control)
    }) {
        return invalid(format!(
            "fixed block titles must contain 1-{MAX_PERSISTED_BLOCK_TITLE_CHARACTERS} non-control characters"
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
        .ok_or_else(|| ComposeScheduleError::InvalidRequest("too many previous blocks".into()))?;
    if previous_blocks > MAX_PREVIOUS_BLOCKS {
        return invalid(format!(
            "previous assignments support at most {MAX_PREVIOUS_BLOCKS} blocks"
        ));
    }
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

fn validate_recurrence_context_references(
    context: &RecurrenceContext,
    revisions: &BTreeMap<ItemId, u64>,
) -> Result<(), ComposeScheduleError> {
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

enum MappedScheduleItem {
    Plannable(Box<WorkItem>),
    ContextOnly,
}

fn classify_item(item: &Item, is_sensitive: bool) -> Result<MappedScheduleItem, String> {
    let item_timezone: Tz = item
        .timezone_name
        .parse()
        .map_err(|_| "canonical item timezone is invalid".to_owned())?;
    let metadata: SchedulingMetadata = serde_json::from_value(item.flexible_constraints.clone())
        .map_err(|error| format!("unsupported flexible_constraints: {error}"))?;
    let recurrence = parse_recurrence(item.recurrence.as_ref())?;
    if let Some(context) = metadata.calendar_context.as_ref() {
        validate_calendar_context(item, recurrence.as_ref(), &metadata, context)?;
        return Ok(MappedScheduleItem::ContextOnly);
    }
    if item.kind != ItemKind::Event && metadata.calendar_event.is_some() {
        return Err("calendar_event metadata is only valid for event items".into());
    }
    if metadata.dayweave_firm_block.is_some()
        && (item.kind != ItemKind::Event
            || item
                .flexible_constraints
                .as_object()
                .is_none_or(|constraints| {
                    constraints.len() != 1 || !constraints.contains_key("dayweave_firm_block")
                }))
    {
        return Err(
            "dayweave_firm_block is only valid as the sole metadata for an event item".into(),
        );
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
        duration: if item.kind == ItemKind::Event {
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

fn validate_calendar_context(
    item: &Item,
    recurrence: Option<&Recurrence>,
    metadata: &SchedulingMetadata,
    context: &CalendarContextSpec,
) -> Result<(), String> {
    if item.kind != ItemKind::Event {
        return Err("calendar_context metadata is only valid for event items".into());
    }
    if item.parent_id.is_some() {
        return Err("calendar_context event must be a root item".into());
    }
    if recurrence.is_some() {
        return Err("calendar_context event must be one expanded occurrence".into());
    }
    if metadata.calendar_event.is_some()
        || item
            .flexible_constraints
            .as_object()
            .is_none_or(|constraints| {
                constraints.len() != 1 || !constraints.contains_key("calendar_context")
            })
    {
        return Err("calendar_context cannot be combined with other scheduling metadata".into());
    }
    let owner = if context.all_day {
        "all-day calendar_context"
    } else {
        "calendar_context"
    };
    validate_calendar_bounds(context.start, context.end, owner)
}

fn validate_calendar_bounds(
    start: OffsetDateTime,
    end: OffsetDateTime,
    owner: &str,
) -> Result<(), String> {
    if start >= end {
        return Err(format!("{owner} end must follow start"));
    }
    if !start.nanosecond().is_multiple_of(1_000) || !end.nanosecond().is_multiple_of(1_000) {
        return Err(format!(
            "{owner} instants must use PostgreSQL microsecond precision"
        ));
    }
    Ok(())
}

fn map_kind(
    kind: ItemKind,
    recurrence: Option<Recurrence>,
    metadata: &SchedulingMetadata,
) -> Result<PlanningItemKind, String> {
    match kind {
        ItemKind::Event => {
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
                (None, Some(firm)) => firm.as_calendar_event()?,
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
            validate_calendar_bounds(event.start, event.end, "calendar_event")?;
            Ok(PlanningItemKind::CalendarEvent(event))
        }
        ItemKind::Task => Ok(recurrence.map_or(PlanningItemKind::Task, |recurrence| {
            PlanningItemKind::RecurringTask(RecurringTaskSpec { recurrence })
        })),
        ItemKind::Habit => recurrence
            .map(|recurrence| {
                PlanningItemKind::Habit(HabitSpec {
                    recurrence,
                    target: metadata.habit_target.clone(),
                    preserves_streak_when_paused: metadata.preserves_streak_when_paused,
                })
            })
            .ok_or_else(|| "habit requires recurrence".into()),
        ItemKind::Routine => Ok(PlanningItemKind::Routine(RoutineSpec {
            ordered: metadata.routine_ordered,
            recurrence,
        })),
        ItemKind::Goal => {
            reject_recurrence(recurrence.as_ref(), "goal")?;
            Ok(PlanningItemKind::Goal(GoalSpec {
                measures: metadata.goal_measures.clone(),
                weekly_allocation: metadata.goal_weekly_allocation,
            }))
        }
        ItemKind::Break => {
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

fn parse_recurrence(value: Option<&Value>) -> Result<Option<Recurrence>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut value = value.clone();
    let object = value
        .as_object_mut()
        .ok_or_else(|| "recurrence must be an object".to_owned())?;
    let recurrence_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "recurrence.type is required".to_owned())?;
    match recurrence_type {
        "daily" => {
            object
                .entry("times_per_day")
                .or_insert_with(|| Value::from(1));
        }
        "weekly" => {
            let default = object
                .get("weekdays")
                .and_then(Value::as_array)
                .map_or(1, |days| days.len().max(1));
            object
                .entry("times_per_week")
                .or_insert_with(|| Value::from(default));
            object
                .entry("weekdays")
                .or_insert_with(|| Value::Array(Vec::new()));
        }
        "monthly" => {
            object
                .entry("times_per_month")
                .or_insert_with(|| Value::from(1));
        }
        _ => {}
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|error| format!("unsupported recurrence: {error}"))
}

fn map_split_policy(policy: &SplitPolicy, metadata: &SchedulingMetadata) -> PlanningSplitPolicy {
    match policy {
        SplitPolicy::Indivisible => PlanningSplitPolicy::Indivisible,
        SplitPolicy::Splittable {
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

const fn map_status(status: ItemStatus) -> WorkStatus {
    match status {
        ItemStatus::Inbox | ItemStatus::Planned => WorkStatus::NotStarted,
        ItemStatus::Scheduled => WorkStatus::Scheduled,
        ItemStatus::InProgress => WorkStatus::Active,
        ItemStatus::Paused => WorkStatus::Paused,
        ItemStatus::Completed => WorkStatus::Completed,
        ItemStatus::Skipped => WorkStatus::Skipped,
        ItemStatus::Cancelled => WorkStatus::Canceled,
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

fn prune_orphaned_items(items: &mut Vec<WorkItem>, rejected: &mut Vec<RejectedScheduleItem>) {
    loop {
        let ids: BTreeSet<_> = items.iter().map(|item| item.id).collect();
        let mut removed = false;
        items.retain(|item| {
            let Some(parent_id) = item.parent_id else {
                return true;
            };
            if ids.contains(&parent_id) {
                return true;
            }
            rejected.push(RejectedScheduleItem {
                item_id: item.id.0,
                is_sensitive: item.is_sensitive,
                title: item.title.clone(),
                reason: format!("parent {parent_id} is unavailable for scheduling"),
            });
            removed = true;
            false
        });
        if !removed {
            break;
        }
    }
    rejected.sort_by_key(|item| item.item_id);
}

fn map_previous_assignments(
    assignments: &[PreviousAssignmentInput],
    revisions: &BTreeMap<ItemId, u64>,
    timezone: Tz,
) -> Result<(Vec<PreviousAssignment>, Vec<IgnoredPreviousAssignment>), ComposeScheduleError> {
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
                .collect::<Result<_, ComposeScheduleError>>()?,
            pinned: assignment.pinned,
        });
    }
    Ok((accepted, ignored))
}

fn map_availability(
    input: AvailabilityInput,
    timezone: Tz,
) -> Result<AvailabilityWindow, ComposeScheduleError> {
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
) -> Result<FixedBlock, ComposeScheduleError> {
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
) -> Result<(), ComposeScheduleError> {
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
        .map_err(|_| ComposeScheduleError::InvalidRequest("invalid timezone_name".into()))?;
    let mut date = horizon_start.with_timezone(&timezone).date_naive();
    let mut days = Vec::new();
    loop {
        let next = date
            .succ_opt()
            .ok_or_else(|| ComposeScheduleError::InvalidRequest("horizon date overflow".into()))?;
        let start = zoned_midnight(timezone, date)?;
        let end = zoned_midnight(timezone, next)?;
        if start < to_time(horizon_end)? && end > to_time(horizon_start)? {
            days.push(ZonedDayBoundary {
                local_date: time::Date::from_calendar_date(
                    date.year(),
                    time::Month::try_from(u8::try_from(date.month()).map_err(|_| {
                        ComposeScheduleError::InvalidRequest("invalid local month".into())
                    })?)
                    .map_err(|_| {
                        ComposeScheduleError::InvalidRequest("invalid local month".into())
                    })?,
                    u8::try_from(date.day()).map_err(|_| {
                        ComposeScheduleError::InvalidRequest("invalid local day".into())
                    })?,
                )
                .map_err(|_| ComposeScheduleError::InvalidRequest("invalid local date".into()))?,
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

fn zoned_midnight(timezone: Tz, date: NaiveDate) -> Result<OffsetDateTime, ComposeScheduleError> {
    let local = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| ComposeScheduleError::InvalidRequest("invalid local midnight".into()))?;
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
        .map_err(|_| ComposeScheduleError::InvalidRequest("timezone offset is invalid".into()))?;
    Ok(to_time(utc)?.to_offset(offset))
}

fn to_time(value: DateTime<Utc>) -> Result<OffsetDateTime, ComposeScheduleError> {
    OffsetDateTime::from_unix_timestamp(value.timestamp())
        .and_then(|instant| instant.replace_nanosecond(value.timestamp_subsec_nanos()))
        .map_err(|_| {
            ComposeScheduleError::InvalidRequest("timestamp is outside supported range".into())
        })
}

fn to_time_in_timezone(
    value: DateTime<Utc>,
    timezone: Tz,
) -> Result<OffsetDateTime, ComposeScheduleError> {
    let localized = value.with_timezone(&timezone);
    let offset = UtcOffset::from_whole_seconds(localized.offset().fix().local_minus_utc())
        .map_err(|_| ComposeScheduleError::InvalidRequest("timezone offset is invalid".into()))?;
    Ok(to_time(value)?.to_offset(offset))
}

fn rfc3339(value: OffsetDateTime) -> Result<String, time::error::Format> {
    value.format(&Rfc3339)
}

fn request_digest(
    scheduler_publication_schema: &str,
    timezone_name: &str,
    source_item_revisions: &BTreeMap<Uuid, u64>,
    calendar_projection_stamps: &[CalendarProjectionStamp],
    request: &PlanRequest,
) -> Result<String, ComposeScheduleError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        scheduler_publication_schema: &'a str,
        timezone_name: &'a str,
        source_item_revisions: &'a BTreeMap<Uuid, u64>,
        calendar_projection_stamps: &'a [CalendarProjectionStamp],
        request: &'a PlanRequest,
    }

    let bytes = serde_json::to_vec(&DigestInput {
        scheduler_publication_schema,
        timezone_name,
        source_item_revisions,
        calendar_projection_stamps,
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

fn invalid<T>(message: impl Into<String>) -> Result<T, ComposeScheduleError> {
    Err(ComposeScheduleError::InvalidRequest(message.into()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // These are independent persisted user toggles.
struct SchedulingMetadata {
    constraints: SchedulingConstraints,
    has_own_effort: bool,
    goal_ids: BTreeSet<Uuid>,
    energy: Option<EnergyMetadata>,
    tags: BTreeSet<String>,
    calendar_event: Option<CalendarEventSpec>,
    calendar_context: Option<CalendarContextSpec>,
    /// Backward-compatible ownership/publication shape used by already-created
    /// `DayWeave` Calendar blocks. New provider projections never emit it.
    dayweave_firm_block: Option<DayWeaveFirmBlockSpec>,
    habit_target: Option<QuantityTarget>,
    preserves_streak_when_paused: bool,
    routine_ordered: bool,
    goal_measures: Vec<GoalMeasure>,
    goal_weekly_allocation: Option<AllocationRange>,
    break_category: Option<BreakCategory>,
    break_mandatory: bool,
    break_prompt_to_resume: bool,
    maximum_sessions: Option<u16>,
    minimum_gap_minutes: u32,
    maximum_split_days: Option<u16>,
    preferred_start_minute: Option<u16>,
}

impl Default for SchedulingMetadata {
    fn default() -> Self {
        Self {
            constraints: SchedulingConstraints::default(),
            has_own_effort: false,
            goal_ids: BTreeSet::new(),
            energy: None,
            tags: BTreeSet::new(),
            calendar_event: None,
            calendar_context: None,
            dayweave_firm_block: None,
            habit_target: None,
            preserves_streak_when_paused: true,
            routine_ordered: false,
            goal_measures: Vec::new(),
            goal_weekly_allocation: None,
            break_category: None,
            break_mandatory: false,
            break_prompt_to_resume: true,
            maximum_sessions: None,
            minimum_gap_minutes: 0,
            maximum_split_days: None,
            preferred_start_minute: None,
        }
    }
}

/// A retained provider occurrence that is useful calendar context but must not
/// reserve planner capacity. Provider identifiers deliberately do not belong
/// in this scheduling representation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalendarContextSpec {
    #[serde(with = "time::serde::rfc3339")]
    start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    end: OffsetDateTime,
    all_day: bool,
}

/// Legacy canonical shape for a DayWeave-owned event. It remains strict so
/// accepting an existing owned block cannot smuggle provider identifiers into
/// scheduling evidence. Google echoes are independently authenticated and
/// deduplicated by the provider mapping layer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Independent provider publication semantics.
struct DayWeaveFirmBlockSpec {
    owned: bool,
    #[serde(with = "time::serde::rfc3339")]
    starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    ends_at: OffsetDateTime,
    #[serde(default)]
    all_day: bool,
    #[serde(default)]
    tentative: bool,
    #[serde(default = "default_true")]
    busy: bool,
}

impl DayWeaveFirmBlockSpec {
    fn as_calendar_event(&self) -> Result<CalendarEventSpec, String> {
        if !self.owned {
            return Err("dayweave_firm_block must be explicitly owned".into());
        }
        // These publication flags affect only the provider representation.
        // The locally owned work still reserves capacity in either state.
        let _ = (self.tentative, self.busy);
        validate_calendar_bounds(self.starts_at, self.ends_at, "dayweave_firm_block")?;
        Ok(CalendarEventSpec {
            start: self.starts_at,
            end: self.ends_at,
            immutable: true,
            all_day: self.all_day,
            source_calendar_id: None,
        })
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum EnergyMetadata {
    Simple(EnergyLevel),
    Qualified(Qualified<EnergyLevel>),
}

impl EnergyMetadata {
    fn into_qualified(self) -> Qualified<EnergyLevel> {
        match self {
            Self::Simple(value) => Qualified::soft(value, 100),
            Self::Qualified(value) => value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            config: SchedulerConfigInput::default(),
            recurrence_context: RecurrenceContext::default(),
        }
    }

    fn map_plannable(item: &Item) -> WorkItem {
        match classify_item(item, item.is_sensitive).expect("valid plannable item") {
            MappedScheduleItem::Plannable(item) => *item,
            MappedScheduleItem::ContextOnly => panic!("expected plannable item"),
        }
    }

    #[test]
    fn composes_canonical_item_and_is_digest_stable() {
        let item = canonical_item(Uuid::from_u128(1));
        let first = compose_items(vec![item.clone()], preview_request()).unwrap();
        let second = compose_items(vec![item], preview_request()).unwrap();
        assert_eq!(first.input_digest, second.input_digest);
        assert_eq!(first.accepted_item_count, 1);
        assert_eq!(
            first.source_item_revisions.get(&Uuid::from_u128(1)),
            Some(&3)
        );
        assert_eq!(first.plan.blocks.len(), 1);
        assert!(first.input_digest.starts_with("sha256:"));
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

        let mut context = RecurrenceContext::default();
        populate_recurrence_calendar(
            &mut context,
            "Europe/Madrid",
            Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 27, 0, 0, 0).unwrap(),
        )
        .unwrap();
        assert!(
            context
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
        let sensitivity = invalid.is_sensitive;
        assert!(classify_item(&invalid, sensitivity).is_err());
    }
}
