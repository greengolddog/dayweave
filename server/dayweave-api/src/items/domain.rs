use std::str::FromStr;

use chrono::{DateTime, Datelike as _, LocalResult, NaiveDate, TimeZone as _, Timelike as _, Utc};
use chrono_tz::Tz;
use dayweave_compose::{
    CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy, MAX_RECURRENCE_BYTES,
    MAX_SCHEDULING_METADATA_BYTES, SchedulingMetadataInput, is_canonical_rfc3339,
    validate_scheduling_metadata,
};
use dayweave_core::Dependency;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

const MAX_TITLE_CHARS: usize = 500;
const MAX_NOTES_CHARS: usize = 100_000;
const MAX_DURATION_SECONDS: u32 = 366 * 24 * 60 * 60;
const MAX_DEADLINE_SOFT_WEIGHT: u32 = 1_000_000;
const MAX_DEADLINE_YEAR: i32 = 9_999;
const MAX_BLOCKED_REASON_CHARS: usize = 1_000;
const MAX_SIBLING_ORDER: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Event,
    Task,
    Habit,
    Routine,
    Goal,
    Project,
    Break,
}

impl ItemKind {
    /// Whether this kind exposes an independently executable component.
    ///
    /// Projects, goals, and routines are semantic containers by default. An
    /// explicit own-effort component makes a leaf container executable; a
    /// separate work-unit identity is still required before a non-leaf
    /// container's own component can be executed independently of its
    /// descendants.
    #[must_use]
    pub(crate) const fn has_executable_component(self, has_own_effort: bool) -> bool {
        !matches!(self, Self::Project | Self::Goal | Self::Routine) || has_own_effort
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
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

impl ItemStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped | Self::Cancelled)
    }

    #[must_use]
    pub const fn prevents_execution(self) -> bool {
        self.is_terminal() || matches!(self, Self::Blocked)
    }

    #[must_use]
    pub const fn is_executing_state(self) -> bool {
        matches!(
            self,
            Self::Scheduled
                | Self::InProgress
                | Self::Paused
                | Self::Completed
                | Self::Skipped
                | Self::Cancelled
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DurationKind {
    Unknown,
    Exact,
    Range,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DurationSource {
    User,
    Assistant,
    Learned,
    Imported,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeadlineKind {
    None,
    Date,
    DateTime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeadlineStrength {
    Hard,
    Soft,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReasonKind {
    Dependency,
    Manual,
    External,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SplitPolicy {
    #[default]
    Indivisible,
    Splittable {
        minimum_chunk_seconds: u32,
        maximum_chunk_seconds: u32,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewItem {
    pub id: Uuid,
    pub is_sensitive: bool,
    pub kind: ItemKind,
    #[serde(default = "default_status")]
    pub status: ItemStatus,
    pub title: String,
    pub notes: Option<String>,
    pub timezone_name: String,
    /// Explicit duration shape. Omission preserves compatibility with legacy
    /// clients by inferring `exact` from `duration_seconds` or `unknown` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_kind: Option<DurationKind>,
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_min_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_max_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_source: Option<DurationSource>,
    /// Explicit deadline shape. Omission infers legacy `deadline_at` as a hard
    /// date-time deadline, or `none` when the legacy field is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_kind: Option<DeadlineKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_date: Option<NaiveDate>,
    /// Optional canonical latest finish in the portable RFC 3339 and microsecond contract.
    #[serde(default, deserialize_with = "deserialize_optional_canonical_datetime")]
    pub deadline_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_strength: Option<DeadlineStrength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_soft_weight: Option<u32>,
    /// Optional canonical earliest start in the portable RFC 3339 and microsecond contract.
    #[serde(default, deserialize_with = "deserialize_optional_canonical_datetime")]
    pub earliest_start_at: Option<DateTime<Utc>>,
    /// Strict authorable daily, weekly, monthly, interval, or frequency recurrence object.
    /// Custom RRULE values remain readable on legacy rows but cannot be created or replaced until
    /// bounded RFC 5545 expansion is implemented. Counts default only for legacy daily, weekly,
    /// and monthly forms.
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[serde(default = "empty_object")]
    /// Closed, semantically validated scheduling metadata. Unknown, wrong-kind,
    /// contradictory, duplicate-set, and invalid event/split policy are rejected.
    #[schema(value_type = Object)]
    pub flexible_constraints: Value,
    /// Authoritative independent effort flag. Omission infers the legacy
    /// `flexible_constraints.has_own_effort` member, defaulting to false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_own_effort: Option<bool>,
    #[serde(default)]
    pub split_policy: SplitPolicy,
    #[serde(default)]
    pub importance: u8,
    #[serde(default)]
    pub urgency: u8,
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub sibling_order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason_kind: Option<BlockedReasonKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_item_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceItem {
    pub is_sensitive: bool,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub title: String,
    pub notes: Option<String>,
    pub timezone_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_kind: Option<DurationKind>,
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_min_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_max_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_source: Option<DurationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_kind: Option<DeadlineKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_date: Option<NaiveDate>,
    /// Optional canonical latest finish in the portable RFC 3339 and microsecond contract.
    #[serde(default, deserialize_with = "deserialize_optional_canonical_datetime")]
    pub deadline_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_strength: Option<DeadlineStrength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_soft_weight: Option<u32>,
    /// Optional canonical earliest start in the portable RFC 3339 and microsecond contract.
    #[serde(default, deserialize_with = "deserialize_optional_canonical_datetime")]
    pub earliest_start_at: Option<DateTime<Utc>>,
    /// Strict authorable daily, weekly, monthly, interval, or frequency recurrence object.
    /// An existing custom RRULE remains readable but cannot cross this replacement boundary until
    /// bounded RFC 5545 expansion is implemented. Counts default only for legacy daily, weekly,
    /// and monthly forms.
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[serde(default = "empty_object")]
    /// Closed, semantically validated scheduling metadata. An exact legacy
    /// `google_sync`-only object may be preserved unchanged but not altered.
    #[schema(value_type = Object)]
    pub flexible_constraints: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_own_effort: Option<bool>,
    #[serde(default)]
    pub split_policy: SplitPolicy,
    pub importance: u8,
    pub urgency: u8,
    pub parent_id: Option<Uuid>,
    pub sibling_order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason_kind: Option<BlockedReasonKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_item_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

impl ReplaceItem {
    /// Conservative pre-lock projection used before the current item row may
    /// be read. Invalid/missing dual-authority input fails closed here and is
    /// reported precisely by ordinary domain validation after locks are held.
    pub(crate) fn may_remove_executable_component(&self) -> bool {
        let requested_own_effort = self.has_own_effort.or_else(|| {
            self.flexible_constraints
                .get("has_own_effort")
                .and_then(Value::as_bool)
        });
        !self
            .kind
            .has_executable_component(requested_own_effort.unwrap_or(false))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(try_from = "ItemWire")]
pub struct Item {
    pub id: Uuid,
    pub is_sensitive: bool,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub title: String,
    pub notes: Option<String>,
    pub timezone_name: String,
    pub duration_kind: DurationKind,
    pub duration_seconds: Option<u32>,
    pub duration_min_seconds: Option<u32>,
    pub duration_max_seconds: Option<u32>,
    pub duration_source: Option<DurationSource>,
    pub deadline_kind: DeadlineKind,
    pub deadline_date: Option<NaiveDate>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub deadline_strength: Option<DeadlineStrength>,
    pub deadline_soft_weight: Option<u32>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[schema(value_type = Object)]
    pub flexible_constraints: Value,
    pub has_own_effort: bool,
    pub split_policy: SplitPolicy,
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
    pub blocked_reason_kind: Option<BlockedReasonKind>,
    pub blocked_by_item_id: Option<Uuid>,
    pub blocked_reason: Option<String>,
}

/// Backward-compatible stored/read shape. Historical delta, idempotency, and
/// undo snapshots predate the typed structural fields, so reads infer those
/// fields exactly as legacy authoring requests do before re-serializing the
/// canonical, explicit response shape.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemWire {
    id: Uuid,
    is_sensitive: bool,
    kind: ItemKind,
    status: ItemStatus,
    title: String,
    notes: Option<String>,
    timezone_name: String,
    #[serde(default)]
    duration_kind: Option<DurationKind>,
    duration_seconds: Option<u32>,
    #[serde(default)]
    duration_min_seconds: Option<u32>,
    #[serde(default)]
    duration_max_seconds: Option<u32>,
    #[serde(default)]
    duration_source: Option<DurationSource>,
    #[serde(default)]
    deadline_kind: Option<DeadlineKind>,
    #[serde(default)]
    deadline_date: Option<NaiveDate>,
    deadline_at: Option<DateTime<Utc>>,
    #[serde(default)]
    deadline_strength: Option<DeadlineStrength>,
    #[serde(default)]
    deadline_soft_weight: Option<u32>,
    earliest_start_at: Option<DateTime<Utc>>,
    recurrence: Option<Value>,
    flexible_constraints: Value,
    #[serde(default)]
    has_own_effort: Option<bool>,
    split_policy: SplitPolicy,
    importance: u8,
    urgency: u8,
    parent_id: Option<Uuid>,
    sibling_order: u32,
    is_executable: bool,
    revision: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    blocked_reason_kind: Option<BlockedReasonKind>,
    #[serde(default)]
    blocked_by_item_id: Option<Uuid>,
    #[serde(default)]
    blocked_reason: Option<String>,
}

impl TryFrom<ItemWire> for Item {
    type Error = String;

    fn try_from(value: ItemWire) -> Result<Self, Self::Error> {
        let legacy_import = value.duration_source.is_none()
            && value.duration_kind.is_none()
            && value.duration_seconds.is_some()
            && has_legacy_import_evidence(&value.flexible_constraints);
        let duration_kind = value
            .duration_kind
            .or(legacy_import.then_some(DurationKind::Exact));
        let duration_source = value
            .duration_source
            .or(legacy_import.then_some(DurationSource::Imported));
        let duration = normalize_duration(
            duration_kind,
            value.duration_seconds,
            value.duration_min_seconds,
            value.duration_max_seconds,
            duration_source,
        )
        .map_err(|error| error.to_string())?;
        let deadline = normalize_deadline(
            value.kind,
            value.deadline_kind,
            value.deadline_date,
            value.deadline_at,
            value.deadline_strength,
            value.deadline_soft_weight,
        )
        .map_err(|error| error.to_string())?;
        let mut flexible_constraints = value.flexible_constraints;
        let has_own_effort =
            normalize_has_own_effort(value.has_own_effort, &mut flexible_constraints)
                .map_err(|error| error.to_string())?;
        let blocker = normalize_blocker(
            value.id,
            value.status,
            value.blocked_reason_kind,
            value.blocked_by_item_id,
            value.blocked_reason,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            id: value.id,
            is_sensitive: value.is_sensitive,
            kind: value.kind,
            status: value.status,
            title: value.title,
            notes: value.notes,
            timezone_name: value.timezone_name,
            duration_kind: duration.kind,
            duration_seconds: duration.expected,
            duration_min_seconds: duration.minimum,
            duration_max_seconds: duration.maximum,
            duration_source: duration.source,
            deadline_kind: deadline.kind,
            deadline_date: deadline.date,
            deadline_at: deadline.at,
            deadline_strength: deadline.strength,
            deadline_soft_weight: deadline.soft_weight,
            earliest_start_at: value.earliest_start_at,
            recurrence: value.recurrence,
            flexible_constraints,
            has_own_effort,
            split_policy: value.split_policy,
            importance: value.importance,
            urgency: value.urgency,
            parent_id: value.parent_id,
            sibling_order: value.sibling_order,
            is_executable: value.is_executable
                && value.kind.has_executable_component(has_own_effort),
            revision: value.revision,
            created_at: value.created_at,
            updated_at: value.updated_at,
            completed_at: value.completed_at,
            deleted_at: value.deleted_at,
            blocked_reason_kind: blocker.kind,
            blocked_by_item_id: blocker.item_id,
            blocked_reason: blocker.reason,
        })
    }
}

impl Item {
    /// Validates and constructs a new canonical item.
    ///
    /// # Errors
    ///
    /// Returns a field-specific domain error for an invalid item contract.
    pub fn new(input: NewItem, now: DateTime<Utc>) -> Result<Self, ItemDomainError> {
        let now = canonical_storage_instant(now);
        let id = input.id;
        let input = ItemFields::from(input).validate(id, None)?;
        let duration_kind = input
            .duration_kind
            .ok_or(ItemDomainError::InvalidDurationShape)?;
        let deadline_kind = input
            .deadline_kind
            .ok_or(ItemDomainError::InvalidDeadlineShape)?;
        let has_own_effort = input
            .has_own_effort
            .ok_or(ItemDomainError::InvalidFlexibleConstraints)?;
        Ok(Self {
            id,
            is_sensitive: input.is_sensitive,
            kind: input.kind,
            status: input.status,
            title: input.title,
            notes: input.notes,
            timezone_name: input.timezone_name,
            duration_kind,
            duration_seconds: input.duration_seconds,
            duration_min_seconds: input.duration_min_seconds,
            duration_max_seconds: input.duration_max_seconds,
            duration_source: input.duration_source,
            deadline_kind,
            deadline_date: input.deadline_date,
            deadline_at: input.deadline_at,
            deadline_strength: input.deadline_strength,
            deadline_soft_weight: input.deadline_soft_weight,
            earliest_start_at: input.earliest_start_at,
            recurrence: input.recurrence,
            flexible_constraints: input.flexible_constraints,
            has_own_effort,
            split_policy: input.split_policy,
            importance: input.importance,
            urgency: input.urgency,
            parent_id: input.parent_id,
            sibling_order: input.sibling_order,
            is_executable: input.kind.has_executable_component(has_own_effort),
            revision: 1,
            created_at: now,
            updated_at: now,
            completed_at: (input.status == ItemStatus::Completed).then_some(now),
            deleted_at: None,
            blocked_reason_kind: input.blocked_reason_kind,
            blocked_by_item_id: input.blocked_by_item_id,
            blocked_reason: input.blocked_reason,
        })
    }

    pub(crate) fn replaced(
        &self,
        input: ReplaceItem,
        now: DateTime<Utc>,
    ) -> Result<Self, ItemDomainError> {
        let now = canonical_storage_instant(now);
        let input = ItemFields::from(input).validate(self.id, Some(&self.flexible_constraints))?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(ItemDomainError::RevisionOverflow)?;
        let has_own_effort = input
            .has_own_effort
            .expect("own effort is normalized during validation");
        Ok(Self {
            id: self.id,
            is_sensitive: input.is_sensitive,
            kind: input.kind,
            status: input.status,
            title: input.title,
            notes: input.notes,
            timezone_name: input.timezone_name,
            duration_kind: input
                .duration_kind
                .expect("duration kind is normalized during validation"),
            duration_seconds: input.duration_seconds,
            duration_min_seconds: input.duration_min_seconds,
            duration_max_seconds: input.duration_max_seconds,
            duration_source: input.duration_source,
            deadline_kind: input
                .deadline_kind
                .expect("deadline kind is normalized during validation"),
            deadline_date: input.deadline_date,
            deadline_at: input.deadline_at,
            deadline_strength: input.deadline_strength,
            deadline_soft_weight: input.deadline_soft_weight,
            earliest_start_at: input.earliest_start_at,
            recurrence: input.recurrence,
            flexible_constraints: input.flexible_constraints,
            has_own_effort,
            split_policy: input.split_policy,
            importance: input.importance,
            urgency: input.urgency,
            parent_id: input.parent_id,
            sibling_order: input.sibling_order,
            is_executable: self.is_executable
                && input.kind.has_executable_component(has_own_effort),
            revision,
            created_at: self.created_at,
            updated_at: now,
            completed_at: if input.status == ItemStatus::Completed {
                self.completed_at.or(Some(now))
            } else {
                None
            },
            deleted_at: self.deleted_at,
            blocked_reason_kind: input.blocked_reason_kind,
            blocked_by_item_id: input.blocked_by_item_id,
            blocked_reason: input.blocked_reason,
        })
    }

    pub(crate) fn trashed(&self, now: DateTime<Utc>) -> Result<Self, ItemDomainError> {
        let now = canonical_storage_instant(now);
        let mut item = self.clone();
        item.revision = item
            .revision
            .checked_add(1)
            .ok_or(ItemDomainError::RevisionOverflow)?;
        item.updated_at = now;
        item.deleted_at = Some(now);
        item.is_executable = false;
        Ok(item)
    }

    pub(crate) fn restored(&self, now: DateTime<Utc>) -> Result<Self, ItemDomainError> {
        let now = canonical_storage_instant(now);
        let mut item = self.clone();
        item.revision = item
            .revision
            .checked_add(1)
            .ok_or(ItemDomainError::RevisionOverflow)?;
        item.updated_at = now;
        item.deleted_at = None;
        Ok(item)
    }

    pub(crate) fn refreshed_execution(
        &self,
        has_active_children: bool,
        now: DateTime<Utc>,
    ) -> Result<Self, ItemDomainError> {
        let now = canonical_storage_instant(now);
        let mut item = self.clone();
        item.revision = item
            .revision
            .checked_add(1)
            .ok_or(ItemDomainError::RevisionOverflow)?;
        item.updated_at = now;
        item.is_executable = item.execution_is_allowed(has_active_children);
        Ok(item)
    }

    #[must_use]
    pub(crate) const fn execution_is_allowed(&self, has_active_children: bool) -> bool {
        self.deleted_at.is_none()
            && !has_active_children
            && self.kind.has_executable_component(self.has_own_effort)
    }

    /// Returns the typed incoming dependency set carried by the portable item
    /// projection. The item boundary validates this shape before an `Item` can
    /// be created, so a decode failure here means stored state is corrupt.
    pub(crate) fn dependencies(&self) -> Result<Vec<Dependency>, ItemDomainError> {
        let Some(value) = self
            .flexible_constraints
            .get("constraints")
            .and_then(|constraints| constraints.get("dependencies"))
        else {
            return Ok(Vec::new());
        };
        serde_json::from_value(value.clone())
            .map_err(|_| ItemDomainError::InvalidFlexibleConstraints)
    }

    /// Replaces only the dependency member of the portable scheduling
    /// projection. Persistence calls this after reading the normalized graph,
    /// making `item_dependencies` authoritative while keeping existing clients
    /// and schedule-helper payloads source compatible.
    pub(crate) fn project_dependencies(
        &mut self,
        dependencies: &[Dependency],
    ) -> Result<(), ItemDomainError> {
        let root = self
            .flexible_constraints
            .as_object_mut()
            .ok_or(ItemDomainError::InvalidFlexibleConstraints)?;
        if dependencies.is_empty() {
            if let Some(constraints) = root.get_mut("constraints") {
                let constraints = constraints
                    .as_object_mut()
                    .ok_or(ItemDomainError::InvalidFlexibleConstraints)?;
                constraints.remove("dependencies");
                if constraints.is_empty() {
                    root.remove("constraints");
                }
            }
            return Ok(());
        }

        let encoded = serde_json::to_value(dependencies)
            .map_err(|_| ItemDomainError::InvalidFlexibleConstraints)?;
        let constraints = root
            .entry("constraints")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or(ItemDomainError::InvalidFlexibleConstraints)?;
        constraints.insert("dependencies".to_owned(), encoded);
        Ok(())
    }

    /// Returns the item metadata stored beside the normalized graph. Incoming
    /// dependencies are projected at read time and therefore never persist as
    /// a second writable copy in `items.scheduling_constraints`.
    pub(crate) fn constraints_without_dependencies(&self) -> Result<Value, ItemDomainError> {
        let mut projected = self.clone();
        projected.project_dependencies(&[])?;
        Ok(projected.flexible_constraints)
    }
}

fn canonical_storage_instant(value: DateTime<Utc>) -> DateTime<Utc> {
    value
        .with_nanosecond(value.nanosecond() / 1_000 * 1_000)
        .unwrap_or(value)
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ItemDomainError {
    #[error("title is required")]
    TitleRequired,
    #[error("title exceeds 500 characters")]
    TitleTooLong,
    #[error("notes exceed 100000 characters")]
    NotesTooLong,
    #[error("timezone_name must be a valid IANA timezone")]
    InvalidTimezone,
    #[error("duration_seconds must be between 1 and {MAX_DURATION_SECONDS}")]
    InvalidDuration,
    #[error(
        "duration fields must form unknown, exact, or a real minimum <= expected <= maximum range"
    )]
    InvalidDurationShape,
    #[error("deadline fields must form none, date, or date_time with a valid hard/soft strength")]
    InvalidDeadlineShape,
    #[error("earliest_start_at must be earlier than deadline_at")]
    InvalidTimeWindow,
    #[error("recurrence must be a bounded JSON object")]
    InvalidRecurrence,
    #[error("flexible_constraints must be a bounded JSON object")]
    InvalidFlexibleConstraints,
    #[error("scheduling metadata is invalid: {0}")]
    InvalidSchedulingMetadata(String),
    #[error(
        "split policy requires a duration and positive ordered chunk bounds within that duration"
    )]
    InvalidSplitPolicy,
    #[error("importance and urgency must be in 0..=100")]
    InvalidPriority,
    #[error("typed and flexible_constraints has_own_effort values disagree")]
    ConflictingOwnEffort,
    #[error("blocked status requires a valid dependency, manual, or external cause")]
    InvalidBlockedReason,
    #[error("sibling_order must be at most {MAX_SIBLING_ORDER}")]
    InvalidSiblingOrder,
    #[error("item revision exceeded the supported range")]
    RevisionOverflow,
}

struct ItemFields {
    is_sensitive: bool,
    kind: ItemKind,
    status: ItemStatus,
    title: String,
    notes: Option<String>,
    timezone_name: String,
    duration_kind: Option<DurationKind>,
    duration_seconds: Option<u32>,
    duration_min_seconds: Option<u32>,
    duration_max_seconds: Option<u32>,
    duration_source: Option<DurationSource>,
    deadline_kind: Option<DeadlineKind>,
    deadline_date: Option<NaiveDate>,
    deadline_at: Option<DateTime<Utc>>,
    deadline_strength: Option<DeadlineStrength>,
    deadline_soft_weight: Option<u32>,
    earliest_start_at: Option<DateTime<Utc>>,
    recurrence: Option<Value>,
    flexible_constraints: Value,
    has_own_effort: Option<bool>,
    split_policy: SplitPolicy,
    importance: u8,
    urgency: u8,
    parent_id: Option<Uuid>,
    sibling_order: u32,
    blocked_reason_kind: Option<BlockedReasonKind>,
    blocked_by_item_id: Option<Uuid>,
    blocked_reason: Option<String>,
}

impl ItemFields {
    #[allow(clippy::assigning_clones, clippy::too_many_lines)]
    fn validate(
        mut self,
        item_id: Uuid,
        preserved_legacy_constraints: Option<&Value>,
    ) -> Result<Self, ItemDomainError> {
        self.title = self.title.trim().to_owned();
        if self.title.is_empty() {
            return Err(ItemDomainError::TitleRequired);
        }
        if self.title.chars().count() > MAX_TITLE_CHARS {
            return Err(ItemDomainError::TitleTooLong);
        }
        if self
            .notes
            .as_ref()
            .is_some_and(|notes| notes.chars().count() > MAX_NOTES_CHARS)
        {
            return Err(ItemDomainError::NotesTooLong);
        }
        let timezone =
            Tz::from_str(&self.timezone_name).map_err(|_| ItemDomainError::InvalidTimezone)?;
        let duration = normalize_duration(
            self.duration_kind,
            self.duration_seconds,
            self.duration_min_seconds,
            self.duration_max_seconds,
            self.duration_source,
        )?;
        self.duration_kind = Some(duration.kind);
        self.duration_seconds = duration.expected;
        self.duration_min_seconds = duration.minimum;
        self.duration_max_seconds = duration.maximum;
        self.duration_source = duration.source;
        let deadline = normalize_deadline(
            self.kind,
            self.deadline_kind,
            self.deadline_date,
            self.deadline_at,
            self.deadline_strength,
            self.deadline_soft_weight,
        )?;
        self.deadline_kind = Some(deadline.kind);
        self.deadline_date = deadline.date;
        self.deadline_at = deadline.at;
        self.deadline_strength = deadline.strength;
        self.deadline_soft_weight = deadline.soft_weight;
        let effective_deadline_at = match deadline.kind {
            DeadlineKind::Date => Some(resolve_date_deadline(
                deadline.date.ok_or(ItemDomainError::InvalidDeadlineShape)?,
                timezone,
            )?),
            DeadlineKind::None | DeadlineKind::DateTime => deadline.at,
        };
        self.has_own_effort = Some(normalize_has_own_effort(
            self.has_own_effort,
            &mut self.flexible_constraints,
        )?);
        let blocker = normalize_blocker(
            item_id,
            self.status,
            self.blocked_reason_kind,
            self.blocked_by_item_id,
            self.blocked_reason,
        )?;
        self.blocked_reason_kind = blocker.kind;
        self.blocked_by_item_id = blocker.item_id;
        self.blocked_reason = blocker.reason;
        if self
            .earliest_start_at
            .zip(effective_deadline_at)
            .is_some_and(|(earliest, deadline)| earliest >= deadline)
        {
            return Err(ItemDomainError::InvalidTimeWindow);
        }
        if self.recurrence.as_ref().is_some_and(|value| {
            !value.is_object()
                || serde_json::to_vec(value)
                    .map_or(true, |encoded| encoded.len() > MAX_RECURRENCE_BYTES)
        }) {
            return Err(ItemDomainError::InvalidRecurrence);
        }
        if !self.flexible_constraints.is_object()
            || serde_json::to_vec(&self.flexible_constraints).map_or(true, |encoded| {
                encoded.len() > MAX_SCHEDULING_METADATA_BYTES
            })
        {
            return Err(ItemDomainError::InvalidFlexibleConstraints);
        }
        match self.split_policy {
            SplitPolicy::Indivisible => {}
            SplitPolicy::Splittable {
                minimum_chunk_seconds,
                maximum_chunk_seconds,
            } => {
                let Some(duration) = self.duration_seconds else {
                    return Err(ItemDomainError::InvalidSplitPolicy);
                };
                if minimum_chunk_seconds == 0
                    || maximum_chunk_seconds < minimum_chunk_seconds
                    || minimum_chunk_seconds > duration
                    || maximum_chunk_seconds > duration
                {
                    return Err(ItemDomainError::InvalidSplitPolicy);
                }
            }
        }
        if self.importance > 100 || self.urgency > 100 {
            return Err(ItemDomainError::InvalidPriority);
        }
        if self.sibling_order > MAX_SIBLING_ORDER {
            return Err(ItemDomainError::InvalidSiblingOrder);
        }
        let split_policy = match &self.split_policy {
            SplitPolicy::Indivisible => CanonicalSplitPolicy::Indivisible,
            SplitPolicy::Splittable {
                minimum_chunk_seconds,
                maximum_chunk_seconds,
            } => CanonicalSplitPolicy::Splittable {
                minimum_chunk_seconds: *minimum_chunk_seconds,
                maximum_chunk_seconds: *maximum_chunk_seconds,
            },
        };
        let empty_constraints = serde_json::json!({});
        let scheduling_constraints = if preserved_legacy_constraints.is_some_and(|current| {
            current == &self.flexible_constraints
                && is_legacy_google_task_metadata(current)
                && self.kind == ItemKind::Task
        }) {
            &empty_constraints
        } else {
            &self.flexible_constraints
        };
        validate_scheduling_metadata(SchedulingMetadataInput {
            item_id,
            kind: canonical_kind(self.kind),
            status: canonical_status(self.status),
            timezone_name: &self.timezone_name,
            duration_seconds: self.duration_seconds,
            deadline_at: effective_deadline_at,
            earliest_start_at: self.earliest_start_at,
            recurrence: self.recurrence.as_ref(),
            flexible_constraints: scheduling_constraints,
            split_policy: &split_policy,
            parent_id: self.parent_id,
        })
        .map_err(|error| ItemDomainError::InvalidSchedulingMetadata(error.to_string()))?;
        Ok(self)
    }
}

#[derive(Clone, Copy)]
struct NormalizedDuration {
    kind: DurationKind,
    expected: Option<u32>,
    minimum: Option<u32>,
    maximum: Option<u32>,
    source: Option<DurationSource>,
}

fn normalize_duration(
    kind: Option<DurationKind>,
    expected: Option<u32>,
    minimum: Option<u32>,
    maximum: Option<u32>,
    source: Option<DurationSource>,
) -> Result<NormalizedDuration, ItemDomainError> {
    let bounded = |value: u32| (1..=MAX_DURATION_SECONDS).contains(&value);
    let kind = match kind {
        Some(kind) => kind,
        None if minimum.is_some() || maximum.is_some() || source.is_some() => {
            return Err(ItemDomainError::InvalidDurationShape);
        }
        None if expected.is_some() => DurationKind::Exact,
        None => DurationKind::Unknown,
    };
    match kind {
        DurationKind::Unknown
            if expected.is_none() && minimum.is_none() && maximum.is_none() && source.is_none() =>
        {
            Ok(NormalizedDuration {
                kind,
                expected: None,
                minimum: None,
                maximum: None,
                source: None,
            })
        }
        DurationKind::Exact => {
            let expected = expected.ok_or(ItemDomainError::InvalidDurationShape)?;
            if !bounded(expected)
                || minimum.is_some_and(|value| value != expected)
                || maximum.is_some_and(|value| value != expected)
            {
                return Err(ItemDomainError::InvalidDurationShape);
            }
            Ok(NormalizedDuration {
                kind,
                expected: Some(expected),
                minimum: Some(expected),
                maximum: Some(expected),
                source: Some(source.unwrap_or(DurationSource::User)),
            })
        }
        DurationKind::Range => {
            let (Some(expected), Some(minimum), Some(maximum)) = (expected, minimum, maximum)
            else {
                return Err(ItemDomainError::InvalidDurationShape);
            };
            if !bounded(expected)
                || !bounded(minimum)
                || !bounded(maximum)
                || minimum > expected
                || expected > maximum
                || minimum == maximum
            {
                return Err(ItemDomainError::InvalidDurationShape);
            }
            Ok(NormalizedDuration {
                kind,
                expected: Some(expected),
                minimum: Some(minimum),
                maximum: Some(maximum),
                source: Some(source.unwrap_or(DurationSource::User)),
            })
        }
        DurationKind::Unknown => Err(ItemDomainError::InvalidDurationShape),
    }
}

#[derive(Clone, Copy)]
struct NormalizedDeadline {
    kind: DeadlineKind,
    date: Option<NaiveDate>,
    at: Option<DateTime<Utc>>,
    strength: Option<DeadlineStrength>,
    soft_weight: Option<u32>,
}

fn normalize_deadline(
    item_kind: ItemKind,
    kind: Option<DeadlineKind>,
    date: Option<NaiveDate>,
    at: Option<DateTime<Utc>>,
    strength: Option<DeadlineStrength>,
    soft_weight: Option<u32>,
) -> Result<NormalizedDeadline, ItemDomainError> {
    let kind = match kind {
        Some(kind) => kind,
        None if date.is_some() || strength.is_some() || soft_weight.is_some() => {
            return Err(ItemDomainError::InvalidDeadlineShape);
        }
        None if item_kind == ItemKind::Event => DeadlineKind::None,
        None if at.is_some() => DeadlineKind::DateTime,
        None => DeadlineKind::None,
    };
    if item_kind == ItemKind::Event && kind != DeadlineKind::None {
        return Err(ItemDomainError::InvalidDeadlineShape);
    }
    let normalize_strength = || {
        let strength = strength.unwrap_or(DeadlineStrength::Hard);
        match (strength, soft_weight) {
            (DeadlineStrength::Hard, None) => Ok((strength, None)),
            (DeadlineStrength::Soft, Some(weight)) if weight <= MAX_DEADLINE_SOFT_WEIGHT => {
                Ok((strength, Some(weight)))
            }
            _ => Err(ItemDomainError::InvalidDeadlineShape),
        }
    };
    match kind {
        DeadlineKind::None
            if date.is_none()
                && strength.is_none()
                && soft_weight.is_none()
                && (at.is_none() || item_kind == ItemKind::Event) =>
        {
            Ok(NormalizedDeadline {
                kind,
                date: None,
                at,
                strength: None,
                soft_weight: None,
            })
        }
        DeadlineKind::Date
            if date.is_some_and(|value| {
                (1..=MAX_DEADLINE_YEAR).contains(&value.year())
                    && value
                        .succ_opt()
                        .is_some_and(|next| next.year() <= MAX_DEADLINE_YEAR)
            }) && at.is_none() =>
        {
            let (strength, soft_weight) = normalize_strength()?;
            Ok(NormalizedDeadline {
                kind,
                date,
                at: None,
                strength: Some(strength),
                soft_weight,
            })
        }
        DeadlineKind::DateTime if date.is_none() && at.is_some() => {
            let (strength, soft_weight) = normalize_strength()?;
            Ok(NormalizedDeadline {
                kind,
                date: None,
                at,
                strength: Some(strength),
                soft_weight,
            })
        }
        DeadlineKind::None | DeadlineKind::Date | DeadlineKind::DateTime => {
            Err(ItemDomainError::InvalidDeadlineShape)
        }
    }
}

fn resolve_date_deadline(date: NaiveDate, timezone: Tz) -> Result<DateTime<Utc>, ItemDomainError> {
    let next = date
        .succ_opt()
        .ok_or(ItemDomainError::InvalidDeadlineShape)?;
    let local = next
        .and_hms_opt(0, 0, 0)
        .ok_or(ItemDomainError::InvalidDeadlineShape)?;
    match timezone.from_local_datetime(&local) {
        LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, second) => Ok(first.min(second).with_timezone(&Utc)),
        LocalResult::None => Err(ItemDomainError::InvalidDeadlineShape),
    }
}

fn normalize_has_own_effort(
    explicit: Option<bool>,
    flexible_constraints: &mut Value,
) -> Result<bool, ItemDomainError> {
    let object = flexible_constraints
        .as_object_mut()
        .ok_or(ItemDomainError::InvalidFlexibleConstraints)?;
    let legacy = match object.get("has_own_effort") {
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err(ItemDomainError::InvalidFlexibleConstraints),
        None => None,
    };
    if explicit
        .zip(legacy)
        .is_some_and(|(left, right)| left != right)
    {
        return Err(ItemDomainError::ConflictingOwnEffort);
    }
    let resolved = explicit.or(legacy).unwrap_or(false);
    if resolved && legacy.is_none() {
        object.insert("has_own_effort".to_owned(), Value::Bool(true));
    }
    Ok(resolved)
}

struct NormalizedBlocker {
    kind: Option<BlockedReasonKind>,
    item_id: Option<Uuid>,
    reason: Option<String>,
}

fn normalize_blocker(
    item_id: Uuid,
    status: ItemStatus,
    kind: Option<BlockedReasonKind>,
    blocked_by_item_id: Option<Uuid>,
    reason: Option<String>,
) -> Result<NormalizedBlocker, ItemDomainError> {
    let reason = reason
        .map(|value| value.trim().to_owned())
        .map(|value| {
            if value.is_empty()
                || value.chars().count() > MAX_BLOCKED_REASON_CHARS
                || value.chars().any(char::is_control)
            {
                Err(ItemDomainError::InvalidBlockedReason)
            } else {
                Ok(value)
            }
        })
        .transpose()?;
    let valid = match (status, kind, blocked_by_item_id, reason.as_ref()) {
        (ItemStatus::Blocked, Some(BlockedReasonKind::Dependency), Some(blocker_id), _)
            if blocker_id != item_id =>
        {
            true
        }
        (
            ItemStatus::Blocked,
            Some(BlockedReasonKind::Manual | BlockedReasonKind::External),
            None,
            Some(_),
        ) => true,
        (status, None, None, None) if status != ItemStatus::Blocked => true,
        _ => false,
    };
    if !valid {
        return Err(ItemDomainError::InvalidBlockedReason);
    }
    Ok(NormalizedBlocker {
        kind,
        item_id: blocked_by_item_id,
        reason,
    })
}

fn is_legacy_google_task_metadata(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 1
            && object
                .get("google_sync")
                .is_some_and(serde_json::Value::is_object)
    })
}

fn has_legacy_import_evidence(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        ["calendar_event", "calendar_context"]
            .iter()
            .any(|key| object.get(*key).is_some_and(Value::is_object))
    })
}

const fn canonical_kind(value: ItemKind) -> CanonicalItemKind {
    match value {
        ItemKind::Event => CanonicalItemKind::Event,
        ItemKind::Task => CanonicalItemKind::Task,
        ItemKind::Habit => CanonicalItemKind::Habit,
        ItemKind::Routine => CanonicalItemKind::Routine,
        ItemKind::Goal => CanonicalItemKind::Goal,
        ItemKind::Project => CanonicalItemKind::Project,
        ItemKind::Break => CanonicalItemKind::Break,
    }
}

const fn canonical_status(value: ItemStatus) -> CanonicalItemStatus {
    match value {
        ItemStatus::Inbox => CanonicalItemStatus::Inbox,
        ItemStatus::Planned => CanonicalItemStatus::Planned,
        ItemStatus::Scheduled => CanonicalItemStatus::Scheduled,
        ItemStatus::InProgress => CanonicalItemStatus::InProgress,
        ItemStatus::Paused => CanonicalItemStatus::Paused,
        ItemStatus::Completed => CanonicalItemStatus::Completed,
        ItemStatus::Skipped => CanonicalItemStatus::Skipped,
        ItemStatus::Cancelled => CanonicalItemStatus::Cancelled,
        ItemStatus::Blocked => CanonicalItemStatus::Blocked,
    }
}

impl From<NewItem> for ItemFields {
    fn from(value: NewItem) -> Self {
        Self {
            is_sensitive: value.is_sensitive,
            kind: value.kind,
            status: value.status,
            title: value.title,
            notes: value.notes,
            timezone_name: value.timezone_name,
            duration_kind: value.duration_kind,
            duration_seconds: value.duration_seconds,
            duration_min_seconds: value.duration_min_seconds,
            duration_max_seconds: value.duration_max_seconds,
            duration_source: value.duration_source,
            deadline_kind: value.deadline_kind,
            deadline_date: value.deadline_date,
            deadline_at: value.deadline_at,
            deadline_strength: value.deadline_strength,
            deadline_soft_weight: value.deadline_soft_weight,
            earliest_start_at: value.earliest_start_at,
            recurrence: value.recurrence,
            flexible_constraints: value.flexible_constraints,
            has_own_effort: value.has_own_effort,
            split_policy: value.split_policy,
            importance: value.importance,
            urgency: value.urgency,
            parent_id: value.parent_id,
            sibling_order: value.sibling_order,
            blocked_reason_kind: value.blocked_reason_kind,
            blocked_by_item_id: value.blocked_by_item_id,
            blocked_reason: value.blocked_reason,
        }
    }
}

impl From<ReplaceItem> for ItemFields {
    fn from(value: ReplaceItem) -> Self {
        Self {
            is_sensitive: value.is_sensitive,
            kind: value.kind,
            status: value.status,
            title: value.title,
            notes: value.notes,
            timezone_name: value.timezone_name,
            duration_kind: value.duration_kind,
            duration_seconds: value.duration_seconds,
            duration_min_seconds: value.duration_min_seconds,
            duration_max_seconds: value.duration_max_seconds,
            duration_source: value.duration_source,
            deadline_kind: value.deadline_kind,
            deadline_date: value.deadline_date,
            deadline_at: value.deadline_at,
            deadline_strength: value.deadline_strength,
            deadline_soft_weight: value.deadline_soft_weight,
            earliest_start_at: value.earliest_start_at,
            recurrence: value.recurrence,
            flexible_constraints: value.flexible_constraints,
            has_own_effort: value.has_own_effort,
            split_policy: value.split_policy,
            importance: value.importance,
            urgency: value.urgency,
            parent_id: value.parent_id,
            sibling_order: value.sibling_order,
            blocked_reason_kind: value.blocked_reason_kind,
            blocked_by_item_id: value.blocked_by_item_id,
            blocked_reason: value.blocked_reason,
        }
    }
}

const fn default_status() -> ItemStatus {
    ItemStatus::Inbox
}

fn empty_object() -> Value {
    serde_json::json!({})
}

fn deserialize_optional_canonical_datetime<'de, D>(
    deserializer: D,
) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|value| {
            if !is_canonical_rfc3339(&value) {
                return Err(serde::de::Error::custom(
                    "timestamp must use canonical RFC 3339 syntax",
                ));
            }
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| serde::de::Error::custom("timestamp must be a valid RFC 3339 value"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn task(id: Uuid, constraints: Value) -> NewItem {
        NewItem {
            id,
            is_sensitive: false,
            kind: ItemKind::Task,
            status: ItemStatus::Inbox,
            title: "Imported task".to_owned(),
            notes: None,
            timezone_name: "UTC".to_owned(),
            duration_kind: None,
            duration_seconds: None,
            duration_min_seconds: None,
            duration_max_seconds: None,
            duration_source: None,
            deadline_kind: None,
            deadline_date: None,
            deadline_at: None,
            deadline_strength: None,
            deadline_soft_weight: None,
            earliest_start_at: None,
            recurrence: None,
            flexible_constraints: constraints,
            has_own_effort: None,
            split_policy: SplitPolicy::Indivisible,
            importance: 0,
            urgency: 0,
            parent_id: None,
            sibling_order: 0,
            blocked_reason_kind: None,
            blocked_by_item_id: None,
            blocked_reason: None,
        }
    }

    fn replacement(item: &Item, constraints: Value) -> ReplaceItem {
        ReplaceItem {
            is_sensitive: item.is_sensitive,
            kind: item.kind,
            status: ItemStatus::Planned,
            title: item.title.clone(),
            notes: item.notes.clone(),
            timezone_name: item.timezone_name.clone(),
            duration_kind: Some(item.duration_kind),
            duration_seconds: item.duration_seconds,
            duration_min_seconds: item.duration_min_seconds,
            duration_max_seconds: item.duration_max_seconds,
            duration_source: item.duration_source,
            deadline_kind: Some(item.deadline_kind),
            deadline_date: item.deadline_date,
            deadline_at: item.deadline_at,
            deadline_strength: item.deadline_strength,
            deadline_soft_weight: item.deadline_soft_weight,
            earliest_start_at: item.earliest_start_at,
            recurrence: item.recurrence.clone(),
            flexible_constraints: constraints,
            has_own_effort: Some(item.has_own_effort),
            split_policy: item.split_policy.clone(),
            importance: item.importance,
            urgency: item.urgency,
            parent_id: item.parent_id,
            sibling_order: item.sibling_order,
            blocked_reason_kind: item.blocked_reason_kind,
            blocked_by_item_id: item.blocked_by_item_id,
            blocked_reason: item.blocked_reason.clone(),
        }
    }

    #[test]
    fn legacy_scalar_fields_infer_explicit_canonical_shapes() {
        let mut input = task(Uuid::from_u128(10), json!({}));
        input.duration_seconds = Some(1_800);
        input.deadline_at = Some("2026-09-03T12:00:00Z".parse().unwrap());
        let item = Item::new(input, Utc::now()).expect("legacy fields remain authorable");
        assert_eq!(item.duration_kind, DurationKind::Exact);
        assert_eq!(item.duration_min_seconds, Some(1_800));
        assert_eq!(item.duration_max_seconds, Some(1_800));
        assert_eq!(item.duration_source, Some(DurationSource::User));
        assert_eq!(item.deadline_kind, DeadlineKind::DateTime);
        assert_eq!(item.deadline_strength, Some(DeadlineStrength::Hard));
        let response = serde_json::to_value(item).expect("canonical response");
        assert_eq!(response["duration_kind"], "exact");
        assert_eq!(response["deadline_kind"], "date_time");
        assert_eq!(response["has_own_effort"], false);
    }

    #[test]
    fn legacy_google_task_duration_is_local_while_calendar_duration_is_imported() {
        let mut input = task(Uuid::from_u128(102), json!({}));
        input.duration_seconds = Some(1_800);
        let current = Item::new(input, Utc::now()).expect("current item");
        let mut legacy = serde_json::to_value(current).expect("legacy snapshot base");
        let object = legacy.as_object_mut().expect("item object");
        for field in [
            "duration_kind",
            "duration_min_seconds",
            "duration_max_seconds",
            "duration_source",
        ] {
            object.remove(field);
        }
        object.insert(
            "flexible_constraints".to_owned(),
            json!({"google_sync": {"remote_id": "task-1"}}),
        );
        let google_task: Item =
            serde_json::from_value(legacy.clone()).expect("legacy Google Task snapshot");
        assert_eq!(google_task.duration_source, Some(DurationSource::User));

        legacy["flexible_constraints"] = json!({"calendar_context": {}});
        let calendar_item: Item = serde_json::from_value(legacy).expect("legacy Calendar snapshot");
        assert_eq!(
            calendar_item.duration_source,
            Some(DurationSource::Imported)
        );
    }

    #[test]
    fn direct_domain_mutations_canonicalize_storage_clock_precision() {
        let now: DateTime<Utc> = "2023-11-14T22:13:20.123456789Z"
            .parse()
            .expect("valid nanosecond instant");
        let item =
            Item::new(task(Uuid::from_u128(101), json!({})), now).expect("direct construction");
        assert_eq!(item.created_at.timestamp_subsec_nanos(), 123_456_000);
        assert_eq!(item.updated_at, item.created_at);

        let later = now + chrono::Duration::seconds(1);
        let replaced = item
            .replaced(replacement(&item, json!({})), later)
            .expect("direct replacement");
        assert_eq!(replaced.updated_at.timestamp_subsec_nanos(), 123_456_000);
    }

    #[test]
    fn rich_duration_date_deadline_and_own_effort_are_cross_validated() {
        let mut input = task(Uuid::from_u128(11), json!({"has_own_effort": true}));
        input.duration_kind = Some(DurationKind::Range);
        input.duration_seconds = Some(3_600);
        input.duration_min_seconds = Some(1_800);
        input.duration_max_seconds = Some(7_200);
        input.duration_source = Some(DurationSource::Assistant);
        input.deadline_kind = Some(DeadlineKind::Date);
        input.deadline_date = Some(NaiveDate::from_ymd_opt(2026, 9, 3).unwrap());
        input.deadline_strength = Some(DeadlineStrength::Soft);
        input.deadline_soft_weight = Some(73);
        input.has_own_effort = Some(true);
        let item = Item::new(input, Utc::now()).expect("rich structural shape");
        assert_eq!(item.duration_kind, DurationKind::Range);
        assert_eq!(item.deadline_kind, DeadlineKind::Date);
        assert!(item.has_own_effort);

        let mut conflicting = task(Uuid::from_u128(12), json!({"has_own_effort": false}));
        conflicting.has_own_effort = Some(true);
        assert_eq!(
            Item::new(conflicting, Utc::now()),
            Err(ItemDomainError::ConflictingOwnEffort)
        );
    }

    #[test]
    fn date_deadline_is_ordered_and_has_one_canonical_owner() {
        let mut duplicate = task(
            Uuid::from_u128(120),
            json!({"constraints": {"latest_finish": {
                "value": "2026-09-04T00:00:00Z",
                "strength": {"level": "hard"}
            }}}),
        );
        duplicate.deadline_kind = Some(DeadlineKind::Date);
        duplicate.deadline_date = NaiveDate::from_ymd_opt(2026, 9, 3);
        duplicate.deadline_strength = Some(DeadlineStrength::Hard);
        assert!(matches!(
            Item::new(duplicate, Utc::now()),
            Err(ItemDomainError::InvalidSchedulingMetadata(message))
                if message.contains("deadline is defined in both")
        ));

        let mut inverted = task(Uuid::from_u128(121), json!({}));
        inverted.deadline_kind = Some(DeadlineKind::Date);
        inverted.deadline_date = NaiveDate::from_ymd_opt(2026, 9, 3);
        inverted.deadline_strength = Some(DeadlineStrength::Hard);
        inverted.earliest_start_at = Some(
            "2026-09-04T00:00:00Z"
                .parse()
                .expect("valid boundary timestamp"),
        );
        assert_eq!(
            Item::new(inverted, Utc::now()),
            Err(ItemDomainError::InvalidTimeWindow)
        );

        let mut unsupported = task(Uuid::from_u128(122), json!({}));
        unsupported.deadline_kind = Some(DeadlineKind::Date);
        unsupported.deadline_date = NaiveDate::from_ymd_opt(9_999, 12, 31);
        unsupported.deadline_strength = Some(DeadlineStrength::Hard);
        assert_eq!(
            Item::new(unsupported, Utc::now()),
            Err(ItemDomainError::InvalidDeadlineShape)
        );
    }

    #[test]
    fn event_interval_end_is_never_reinterpreted_as_a_deadline() {
        let starts_at: DateTime<Utc> = "2026-09-03T10:00:00Z".parse().unwrap();
        let ends_at: DateTime<Utc> = "2026-09-03T11:00:00Z".parse().unwrap();
        let mut input = task(Uuid::from_u128(13), json!({}));
        input.kind = ItemKind::Event;
        input.status = ItemStatus::Scheduled;
        input.duration_seconds = Some(3_600);
        input.earliest_start_at = Some(starts_at);
        input.deadline_at = Some(ends_at);
        input.flexible_constraints = json!({"dayweave_firm_block": {
            "owned": true,
            "starts_at": starts_at,
            "ends_at": ends_at
        }});
        let item = Item::new(input, Utc::now()).expect("legacy event interval");
        assert_eq!(item.deadline_kind, DeadlineKind::None);
        assert_eq!(item.deadline_at, Some(ends_at));
        assert_eq!(item.deadline_strength, None);

        let mut invalid_snapshot = serde_json::to_value(item).unwrap();
        invalid_snapshot["deadline_kind"] = json!("date_time");
        invalid_snapshot["deadline_strength"] = json!("hard");
        assert!(
            serde_json::from_value::<Item>(invalid_snapshot).is_err(),
            "historical snapshot reads enforce the Event deadline invariant"
        );
    }

    #[test]
    fn blockers_require_a_typed_visible_cause() {
        let id = Uuid::from_u128(14);
        let mut dependency = task(id, json!({}));
        dependency.status = ItemStatus::Blocked;
        dependency.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
        dependency.blocked_by_item_id = Some(Uuid::from_u128(15));
        dependency.blocked_reason = Some("  Waiting for prerequisite  ".to_owned());
        let item = Item::new(dependency, Utc::now()).expect("dependency blocker");
        assert_eq!(
            item.blocked_reason.as_deref(),
            Some("Waiting for prerequisite")
        );

        let mut manual = task(Uuid::from_u128(16), json!({}));
        manual.status = ItemStatus::Blocked;
        manual.blocked_reason_kind = Some(BlockedReasonKind::Manual);
        manual.blocked_reason = Some("   ".to_owned());
        assert_eq!(
            Item::new(manual, Utc::now()),
            Err(ItemDomainError::InvalidBlockedReason)
        );

        let mut unblocked = task(Uuid::from_u128(17), json!({}));
        unblocked.blocked_reason_kind = Some(BlockedReasonKind::External);
        unblocked.blocked_reason = Some("Provider outage".to_owned());
        assert_eq!(
            Item::new(unblocked, Utc::now()),
            Err(ItemDomainError::InvalidBlockedReason)
        );
    }

    #[test]
    fn project_is_an_authorable_structural_container() {
        let mut input = task(Uuid::from_u128(18), json!({"has_own_effort": true}));
        input.kind = ItemKind::Project;
        input.has_own_effort = Some(true);
        let project = Item::new(input, Utc::now()).expect("project");
        assert_eq!(project.kind, ItemKind::Project);
        assert!(project.has_own_effort);
        assert!(project.is_executable);
    }

    #[test]
    fn semantic_containers_require_an_explicit_own_component_to_execute() {
        for (offset, kind) in [ItemKind::Project, ItemKind::Goal, ItemKind::Routine]
            .into_iter()
            .enumerate()
        {
            let mut input = task(Uuid::from_u128(180 + offset as u128), json!({}));
            input.kind = kind;
            let item = Item::new(input, Utc::now()).expect("semantic container");
            assert!(!item.is_executable, "{kind:?} is a container by default");

            let mut historical = serde_json::to_value(item).expect("historical item payload");
            historical["is_executable"] = json!(true);
            let normalized: Item =
                serde_json::from_value(historical).expect("legacy executable projection");
            assert!(
                !normalized.is_executable,
                "legacy snapshots cannot restore executable {kind:?} without own effort"
            );
        }
    }

    #[test]
    fn replacement_can_clear_typed_and_legacy_own_effort_together() {
        let mut input = task(Uuid::from_u128(19), json!({"has_own_effort": true}));
        input.kind = ItemKind::Project;
        let current = Item::new(input, Utc::now()).expect("own-effort item");
        let mut clear = replacement(&current, json!({}));
        clear.has_own_effort = Some(false);
        let replaced = current
            .replaced(clear, Utc::now())
            .expect("clear own effort");
        assert!(!replaced.has_own_effort);
        assert!(!replaced.is_executable);
        assert_eq!(
            replaced.flexible_constraints.get("has_own_effort"),
            None,
            "legacy projection cannot remain true after the typed flag is cleared"
        );
    }

    #[test]
    fn legacy_google_task_metadata_can_only_be_preserved_exactly() {
        let id = Uuid::from_u128(1);
        let marker = json!({
            "google_sync": {
                "account_id": Uuid::from_u128(2),
                "collection_id": Uuid::from_u128(3),
                "remote_id": "remote-task"
            }
        });
        assert!(matches!(
            Item::new(task(id, marker.clone()), Utc::now()),
            Err(ItemDomainError::InvalidSchedulingMetadata(_))
        ));

        let mut legacy = Item::new(task(id, json!({})), Utc::now()).unwrap();
        legacy.flexible_constraints = marker.clone();
        let preserved = legacy
            .replaced(replacement(&legacy, marker.clone()), Utc::now())
            .expect("an ordinary edit may retain exact legacy provider evidence");
        assert_eq!(preserved.flexible_constraints, marker);

        let altered = json!({"google_sync": {"remote_id": "forged"}});
        assert!(matches!(
            legacy.replaced(replacement(&legacy, altered), Utc::now()),
            Err(ItemDomainError::InvalidSchedulingMetadata(_))
        ));
        let stripped = legacy
            .replaced(replacement(&legacy, json!({})), Utc::now())
            .expect("clients may deliberately replace legacy evidence with public metadata");
        assert_eq!(stripped.flexible_constraints, json!({}));
    }

    #[test]
    fn legacy_rows_remain_deserializable_without_write_validation() {
        let mut legacy = Item::new(task(Uuid::from_u128(4), json!({})), Utc::now()).unwrap();
        legacy.flexible_constraints = json!({"removed_legacy_extension": {"value": true}});
        legacy.recurrence = Some(json!({
            "type": "custom",
            "rrule": "FREQ=MONTHLY;BYDAY=1MO,-1FR"
        }));
        let stored = serde_json::to_value(&legacy).unwrap();
        let hydrated: Item =
            serde_json::from_value(stored).expect("legacy read must stay compatible");
        assert_eq!(hydrated.flexible_constraints, legacy.flexible_constraints);
        assert_eq!(hydrated.recurrence, legacy.recurrence);
        assert!(matches!(
            legacy.replaced(
                replacement(&legacy, legacy.flexible_constraints.clone()),
                Utc::now()
            ),
            Err(ItemDomainError::InvalidSchedulingMetadata(_))
        ));
    }

    #[test]
    fn authoring_timestamps_use_the_portable_lexical_grammar() {
        let id = Uuid::from_u128(5);
        let new_item = task(id, json!({}));
        let mut valid = serde_json::to_value(&new_item).expect("new item JSON");
        valid["deadline_at"] = json!("2026-09-03T10:00:00.123456000+18:00");
        assert!(serde_json::from_value::<NewItem>(valid).is_ok());

        for invalid in [
            "2026-09-03 10:00:00Z",
            "2026-09-03T10:00:00z",
            "2026-09-03T10:00:60Z",
            "2026-09-03T10:00:00.1234567890Z",
            "2026-09-03T10:00:00+18:01",
        ] {
            let mut encoded = serde_json::to_value(&new_item).expect("new item JSON");
            encoded["deadline_at"] = json!(invalid);
            let error = serde_json::from_value::<NewItem>(encoded)
                .expect_err("non-portable authoring timestamp must fail");
            assert!(error.to_string().contains("canonical RFC 3339 syntax"));
        }

        let current = Item::new(new_item, Utc::now()).expect("current item");
        let mut replacement =
            serde_json::to_value(replacement(&current, json!({}))).expect("replacement JSON");
        replacement["earliest_start_at"] = json!("2026-09-03 10:00:00Z");
        assert!(serde_json::from_value::<ReplaceItem>(replacement).is_err());

        let mut legacy_read = serde_json::to_value(current).expect("legacy item JSON");
        let legacy_object = legacy_read.as_object_mut().expect("item object");
        legacy_object.remove("deadline_kind");
        legacy_object.remove("deadline_date");
        legacy_object.remove("deadline_strength");
        legacy_object.remove("deadline_soft_weight");
        legacy_read["deadline_at"] = json!("2026-09-03 10:00:00Z");
        assert!(
            serde_json::from_value::<Item>(legacy_read).is_ok(),
            "raw Item reads remain compatible while authoring DTOs are strict"
        );
    }
}
