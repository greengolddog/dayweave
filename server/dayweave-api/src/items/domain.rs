use std::str::FromStr;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

const MAX_TITLE_CHARS: usize = 500;
const MAX_NOTES_CHARS: usize = 100_000;
const MAX_RECURRENCE_BYTES: usize = 16 * 1024;
const MAX_CONSTRAINT_BYTES: usize = 32 * 1024;
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
    pub deadline_at: Option<DateTime<Utc>>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[serde(default = "empty_object")]
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
    pub deadline_at: Option<DateTime<Utc>>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<Object>)]
    pub recurrence: Option<Value>,
    #[serde(default = "empty_object")]
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
        let input = ItemFields::from(input).validate()?;
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
        let input = ItemFields::from(input).validate()?;
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
    #[error("split policy requires a duration and valid positive chunk bounds")]
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
    fn validate(mut self) -> Result<Self, ItemDomainError> {
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
            || serde_json::to_vec(&self.flexible_constraints)
                .map_or(true, |encoded| encoded.len() > MAX_CONSTRAINT_BYTES)
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
        Ok(self)
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
