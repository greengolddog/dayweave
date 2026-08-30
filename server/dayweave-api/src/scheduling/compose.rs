use std::{collections::BTreeMap, fmt::Write as _, ops::Deref};

use dayweave_compose::{
    CanonicalItem, CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy,
    ComposeScheduleRequest, FixedBlockSourceInput, IgnoredPreviousAssignment, MAX_CANONICAL_ITEMS,
    PrepareScheduleError, PreparedSchedule, RejectedScheduleItem, prepare_canonical_schedule,
    validate_schedule_request,
};
use dayweave_core::{ItemId, OccurrenceId, PlanRequest, ScheduleError, SchedulePlan, Scheduler};
use serde::Serialize;
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
    CalendarProjectionFenceError, CalendarProjectionStamp, postgres::PostgresSchedulingRepository,
};

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
    /// Canonical items accepted by this scheduler schema. This includes Inbox
    /// subtrees and retained nonblocking calendar context even though neither
    /// emits a work item or schedule block.
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
    validate_schedule_request(&request).map_err(map_prepare_error)?;
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
    let source_items = source_items.into_iter().map(into_canonical_item).collect();
    let prepared = prepare_canonical_schedule(source_items, request).map_err(map_prepare_error)?;
    compose_prepared_for_schema(
        prepared,
        scheduler_publication_schema,
        calendar_projection_stamps,
    )
}

fn compose_prepared_for_schema(
    prepared: PreparedSchedule,
    scheduler_publication_schema: &str,
    calendar_projection_stamps: Vec<CalendarProjectionStamp>,
) -> Result<ComposeScheduleResult, ComposeScheduleError> {
    let PreparedSchedule {
        timezone_name,
        source_item_count,
        source_item_revisions,
        effective_sensitivity,
        accepted_item_count,
        rejected_items,
        ignored_previous_assignments,
        plan_request,
    } = prepared;
    let input_digest = request_digest(
        scheduler_publication_schema,
        &timezone_name,
        &source_item_revisions,
        &calendar_projection_stamps,
        &plan_request,
    )?;
    let plan = Scheduler.plan(&plan_request)?;
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
    super::postgres::validate_publishable_compose_result(&timezone_name, &result).map_err(
        |_| {
            ComposeScheduleError::InvalidRequest(
                "composed schedule exceeds the durable publication contract".to_owned(),
            )
        },
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use chrono::{TimeZone as _, Utc};
    use dayweave_compose::{
        AvailabilityInput, EnergyInput, FixedBlockInput, PreviousAssignmentInput,
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

    #[test]
    fn composes_canonical_item_and_is_digest_stable() {
        const LEGACY_DIGEST: &str =
            "sha256:45da53ed109c08d0ac9a442b722f0f10ebb3ca813bf346fa10f79a4ccff53def";
        const LEGACY_RESPONSE: &str = r#"{"input_digest":"sha256:45da53ed109c08d0ac9a442b722f0f10ebb3ca813bf346fa10f79a4ccff53def","source_item_count":1,"source_item_revisions":{"00000000-0000-0000-0000-000000000001":3},"accepted_item_count":1,"rejected_items":[],"ignored_previous_assignments":[],"plan":{"as_of":"2026-09-01T09:00:00+02:00","horizon_start":"2026-09-01T02:00:00+02:00","horizon_end":"2026-09-02T02:00:00+02:00","blocks":[{"id":"829359ec-6709-54db-a3f2-4428470e1ae6","is_sensitive":false,"item_id":"00000000-0000-0000-0000-000000000001","occurrence_id":null,"external_block_id":null,"title":"Write schedule bridge","start":"2026-09-01T09:00:00+02:00","end":"2026-09-01T10:00:00+02:00","session_index":0,"kind":"planned","explanations":[{"code":"hard_deadline","message":"Placed within its hard deadline."},{"code":"priority","message":"Priority score is 48."},{"code":"preferred_window","message":"Matches a preferred work window."},{"code":"energy_match","message":"Matches the available energy level."},{"code":"earliest_available","message":"Uses the earliest best-scoring valid capacity."}]}],"unscheduled":[],"decisions":[{"item_id":"00000000-0000-0000-0000-000000000001","occurrence_id":null,"kind":"scheduled","message":"Reserved 60 minutes."}],"violations":[],"score":{"scheduled_minutes":60,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}"#;
        let item = canonical_item(Uuid::from_u128(1));
        let first = compose_items(vec![item.clone()], preview_request()).unwrap();
        let second = compose_items(vec![item], preview_request()).unwrap();
        assert_eq!(first.input_digest, second.input_digest);
        assert_eq!(first.input_digest, LEGACY_DIGEST);
        assert_eq!(
            serde_json::to_string(&first).expect("migrated response encoding"),
            LEGACY_RESPONSE
        );
        assert_eq!(first.accepted_item_count, 1);
        assert_eq!(
            first.source_item_revisions.get(&Uuid::from_u128(1)),
            Some(&3)
        );
        assert_eq!(first.plan.blocks.len(), 1);
        assert!(first.input_digest.starts_with("sha256:"));
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
}
