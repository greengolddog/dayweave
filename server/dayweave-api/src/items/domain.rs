use std::str::FromStr;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use dayweave_compose::{
    CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy, MAX_RECURRENCE_BYTES,
    MAX_SCHEDULING_METADATA_BYTES, SchedulingMetadataInput, is_canonical_rfc3339,
    validate_scheduling_metadata,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

const MAX_TITLE_CHARS: usize = 500;
const MAX_NOTES_CHARS: usize = 100_000;
const MAX_DURATION_SECONDS: u32 = 366 * 24 * 60 * 60;
const MAX_SIBLING_ORDER: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Event,
    Task,
    Habit,
    Routine,
    Goal,
    Break,
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
}

impl ItemStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped | Self::Cancelled)
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
    pub duration_seconds: Option<u32>,
    /// Optional canonical latest finish in the portable RFC 3339 and microsecond contract.
    #[serde(default, deserialize_with = "deserialize_optional_canonical_datetime")]
    pub deadline_at: Option<DateTime<Utc>>,
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
    #[serde(default)]
    pub split_policy: SplitPolicy,
    #[serde(default)]
    pub importance: u8,
    #[serde(default)]
    pub urgency: u8,
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub sibling_order: u32,
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
    pub duration_seconds: Option<u32>,
    /// Optional canonical latest finish in the portable RFC 3339 and microsecond contract.
    #[serde(default, deserialize_with = "deserialize_optional_canonical_datetime")]
    pub deadline_at: Option<DateTime<Utc>>,
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
    #[serde(default)]
    pub split_policy: SplitPolicy,
    pub importance: u8,
    pub urgency: u8,
    pub parent_id: Option<Uuid>,
    pub sibling_order: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
pub struct Item {
    pub id: Uuid,
    pub is_sensitive: bool,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub title: String,
    pub notes: Option<String>,
    pub timezone_name: String,
    pub duration_seconds: Option<u32>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[schema(value_type = Object)]
    pub flexible_constraints: Value,
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
}

impl Item {
    /// Validates and constructs a new canonical item.
    ///
    /// # Errors
    ///
    /// Returns a field-specific domain error for an invalid item contract.
    pub fn new(input: NewItem, now: DateTime<Utc>) -> Result<Self, ItemDomainError> {
        let id = input.id;
        let input = ItemFields::from(input).validate(id, None)?;
        Ok(Self {
            id,
            is_sensitive: input.is_sensitive,
            kind: input.kind,
            status: input.status,
            title: input.title,
            notes: input.notes,
            timezone_name: input.timezone_name,
            duration_seconds: input.duration_seconds,
            deadline_at: input.deadline_at,
            earliest_start_at: input.earliest_start_at,
            recurrence: input.recurrence,
            flexible_constraints: input.flexible_constraints,
            split_policy: input.split_policy,
            importance: input.importance,
            urgency: input.urgency,
            parent_id: input.parent_id,
            sibling_order: input.sibling_order,
            is_executable: true,
            revision: 1,
            created_at: now,
            updated_at: now,
            completed_at: (input.status == ItemStatus::Completed).then_some(now),
            deleted_at: None,
        })
    }

    pub(crate) fn replaced(
        &self,
        input: ReplaceItem,
        now: DateTime<Utc>,
    ) -> Result<Self, ItemDomainError> {
        let input = ItemFields::from(input).validate(self.id, Some(&self.flexible_constraints))?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(ItemDomainError::RevisionOverflow)?;
        Ok(Self {
            id: self.id,
            is_sensitive: input.is_sensitive,
            kind: input.kind,
            status: input.status,
            title: input.title,
            notes: input.notes,
            timezone_name: input.timezone_name,
            duration_seconds: input.duration_seconds,
            deadline_at: input.deadline_at,
            earliest_start_at: input.earliest_start_at,
            recurrence: input.recurrence,
            flexible_constraints: input.flexible_constraints,
            split_policy: input.split_policy,
            importance: input.importance,
            urgency: input.urgency,
            parent_id: input.parent_id,
            sibling_order: input.sibling_order,
            is_executable: self.is_executable,
            revision,
            created_at: self.created_at,
            updated_at: now,
            completed_at: if input.status == ItemStatus::Completed {
                self.completed_at.or(Some(now))
            } else {
                None
            },
            deleted_at: self.deleted_at,
        })
    }

    pub(crate) fn trashed(&self, now: DateTime<Utc>) -> Result<Self, ItemDomainError> {
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
        is_executable: bool,
        now: DateTime<Utc>,
    ) -> Result<Self, ItemDomainError> {
        let mut item = self.clone();
        item.revision = item
            .revision
            .checked_add(1)
            .ok_or(ItemDomainError::RevisionOverflow)?;
        item.updated_at = now;
        item.is_executable = is_executable;
        Ok(item)
    }
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
    duration_seconds: Option<u32>,
    deadline_at: Option<DateTime<Utc>>,
    earliest_start_at: Option<DateTime<Utc>>,
    recurrence: Option<Value>,
    flexible_constraints: Value,
    split_policy: SplitPolicy,
    importance: u8,
    urgency: u8,
    parent_id: Option<Uuid>,
    sibling_order: u32,
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
        if Tz::from_str(&self.timezone_name).is_err() {
            return Err(ItemDomainError::InvalidTimezone);
        }
        if self
            .duration_seconds
            .is_some_and(|value| value == 0 || value > MAX_DURATION_SECONDS)
        {
            return Err(ItemDomainError::InvalidDuration);
        }
        if self
            .earliest_start_at
            .zip(self.deadline_at)
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
            deadline_at: self.deadline_at,
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

fn is_legacy_google_task_metadata(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 1
            && object
                .get("google_sync")
                .is_some_and(serde_json::Value::is_object)
    })
}

const fn canonical_kind(value: ItemKind) -> CanonicalItemKind {
    match value {
        ItemKind::Event => CanonicalItemKind::Event,
        ItemKind::Task => CanonicalItemKind::Task,
        ItemKind::Habit => CanonicalItemKind::Habit,
        ItemKind::Routine => CanonicalItemKind::Routine,
        ItemKind::Goal => CanonicalItemKind::Goal,
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
            duration_seconds: value.duration_seconds,
            deadline_at: value.deadline_at,
            earliest_start_at: value.earliest_start_at,
            recurrence: value.recurrence,
            flexible_constraints: value.flexible_constraints,
            split_policy: value.split_policy,
            importance: value.importance,
            urgency: value.urgency,
            parent_id: value.parent_id,
            sibling_order: value.sibling_order,
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
            duration_seconds: value.duration_seconds,
            deadline_at: value.deadline_at,
            earliest_start_at: value.earliest_start_at,
            recurrence: value.recurrence,
            flexible_constraints: value.flexible_constraints,
            split_policy: value.split_policy,
            importance: value.importance,
            urgency: value.urgency,
            parent_id: value.parent_id,
            sibling_order: value.sibling_order,
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
            duration_seconds: None,
            deadline_at: None,
            earliest_start_at: None,
            recurrence: None,
            flexible_constraints: constraints,
            split_policy: SplitPolicy::Indivisible,
            importance: 0,
            urgency: 0,
            parent_id: None,
            sibling_order: 0,
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
            duration_seconds: item.duration_seconds,
            deadline_at: item.deadline_at,
            earliest_start_at: item.earliest_start_at,
            recurrence: item.recurrence.clone(),
            flexible_constraints: constraints,
            split_policy: item.split_policy.clone(),
            importance: item.importance,
            urgency: item.urgency,
            parent_id: item.parent_id,
            sibling_order: item.sibling_order,
        }
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
        legacy_read["deadline_at"] = json!("2026-09-03 10:00:00Z");
        assert!(
            serde_json::from_value::<Item>(legacy_read).is_ok(),
            "raw Item reads remain compatible while authoring DTOs are strict"
        );
    }
}
