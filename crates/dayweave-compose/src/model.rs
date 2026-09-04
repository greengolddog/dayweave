use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use dayweave_core::{PlanRequest, RecurrenceContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// Complete active canonical-item snapshot consumed by schedule preparation.
///
/// The shape deliberately mirrors the server item contract rather than the
/// smaller scheduling-core work item. Fields not currently used for placement
/// remain part of the strict boundary so a caller cannot accidentally pass a
/// partial or future-mutated snapshot as authoritative input.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CanonicalItem {
    pub id: Uuid,
    pub is_sensitive: bool,
    pub kind: CanonicalItemKind,
    pub status: CanonicalItemStatus,
    pub title: String,
    pub notes: Option<String>,
    pub timezone_name: String,
    #[serde(default)]
    pub duration_kind: Option<CanonicalDurationKind>,
    pub duration_seconds: Option<u32>,
    pub duration_min_seconds: Option<u32>,
    pub duration_max_seconds: Option<u32>,
    pub duration_source: Option<CanonicalDurationSource>,
    #[serde(default)]
    pub deadline_kind: Option<CanonicalDeadlineKind>,
    pub deadline_date: Option<chrono::NaiveDate>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub deadline_strength: Option<CanonicalDeadlineStrength>,
    pub deadline_soft_weight: Option<u32>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[schema(value_type = Object)]
    pub flexible_constraints: Value,
    #[serde(default)]
    pub has_own_effort: Option<bool>,
    pub split_policy: CanonicalSplitPolicy,
    pub importance: u8,
    pub urgency: u8,
    pub parent_id: Option<Uuid>,
    pub sibling_order: u32,
    pub is_executable: bool,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub blocked_reason_kind: Option<CanonicalBlockedReasonKind>,
    pub blocked_by_item_id: Option<Uuid>,
    pub blocked_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalItemKind {
    Event,
    Task,
    Habit,
    Routine,
    Goal,
    Project,
    Break,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalItemStatus {
    Inbox,
    Planned,
    Scheduled,
    InProgress,
    Paused,
    Completed,
    Skipped,
    Cancelled,
    Blocked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDurationKind {
    Unknown,
    Exact,
    Range,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDurationSource {
    User,
    Assistant,
    Learned,
    Imported,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDeadlineKind {
    None,
    Date,
    DateTime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDeadlineStrength {
    Hard,
    Soft,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalBlockedReasonKind {
    Dependency,
    Manual,
    External,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanonicalSplitPolicy {
    #[default]
    Indivisible,
    Splittable {
        minimum_chunk_seconds: u32,
        maximum_chunk_seconds: u32,
    },
}

impl<'de> Deserialize<'de> for CanonicalSplitPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum StrictSplitPolicy {
            Indivisible {},
            Splittable {
                minimum_chunk_seconds: u32,
                maximum_chunk_seconds: u32,
            },
        }

        Ok(match StrictSplitPolicy::deserialize(deserializer)? {
            StrictSplitPolicy::Indivisible {} => Self::Indivisible,
            StrictSplitPolicy::Splittable {
                minimum_chunk_seconds,
                maximum_chunk_seconds,
            } => Self::Splittable {
                minimum_chunk_seconds,
                maximum_chunk_seconds,
            },
        })
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, ToSchema)]
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
    /// Explicit exact placements evaluated as pinned demand. Unlike
    /// `previous_assignments`, these are user proposals rather than caller
    /// claims about already-published state.
    #[serde(default)]
    pub manual_placements: Vec<ManualPlacementInput>,
    /// Explicitly removes a retained manual pin. Releases are bound to the
    /// current published revision and are distinct from placement proposals.
    #[serde(default)]
    pub manual_placement_releases: Vec<ManualPlacementReleaseInput>,
    #[serde(default)]
    pub config: SchedulerConfigInput,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub recurrence_context: RecurrenceContext,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, ToSchema)]
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

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnergyInput {
    Low,
    #[default]
    Medium,
    Deep,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FixedBlockInput {
    pub id: Uuid,
    pub is_sensitive: bool,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub source: FixedBlockSourceInput,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FixedBlockSourceInput {
    GoogleCalendar,
    Sleep,
    ProtectedTime,
    Travel,
    Manual,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize, ToSchema)]
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

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviousBlockInput {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub session_index: u16,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementInput {
    pub id: Uuid,
    /// Optional exact published revision the UI moved from. A configured
    /// server validates this against its authoritative planning snapshot.
    pub source_schedule_revision_id: Option<Uuid>,
    pub assignments: Vec<ManualPlacementAssignmentInput>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementAssignmentInput {
    pub item_id: Uuid,
    pub item_revision: u64,
    pub occurrence_id: Option<Uuid>,
    pub blocks: Vec<PreviousBlockInput>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPlacementReleaseInput {
    pub id: Uuid,
    pub placement_id: Uuid,
    pub source_schedule_revision_id: Uuid,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
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

/// Fully normalized scheduling input and the evidence required to validate a
/// caller's snapshot. No schedule has been executed at this stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSchedule {
    pub timezone_name: String,
    pub source_item_count: usize,
    pub source_item_revisions: BTreeMap<Uuid, u64>,
    pub effective_sensitivity: BTreeMap<Uuid, bool>,
    pub accepted_item_count: usize,
    pub rejected_items: Vec<RejectedScheduleItem>,
    pub ignored_previous_assignments: Vec<IgnoredPreviousAssignment>,
    pub manual_placements: Vec<ManualPlacementInput>,
    pub manual_placement_releases: Vec<ManualPlacementReleaseInput>,
    pub plan_request: PlanRequest,
}
