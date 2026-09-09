use std::collections::HashSet;

use chrono::{DateTime, Utc};
use dayweave_core::{
    MAX_PROGRESS_BASIS_POINTS, MAX_PROGRESS_COMPONENTS, MAX_PROGRESS_NAME_SCALARS,
    MAX_PROGRESS_SECONDS, MAX_PROGRESS_UNIT_SCALARS, is_valid_progress_label,
    parse_progress_decimal,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemProgressComponent {
    pub id: Uuid,
    pub name: String,
    pub value: ItemProgressValue,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ItemProgressValue {
    Percentage {
        basis_points: u16,
    },
    Time {
        elapsed_seconds: u64,
        #[serde(deserialize_with = "required_nullable")]
        #[schema(required)]
        remaining_seconds: Option<u64>,
    },
    Quantity {
        current: String,
        unit: String,
        #[serde(deserialize_with = "required_nullable")]
        #[schema(required)]
        target: Option<ItemProgressTarget>,
    },
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemProgressTarget {
    pub value: String,
    pub direction: ItemProgressDirection,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemProgressDirection {
    AtLeast,
    AtMost,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemProgressCommand {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub expected_item_revision: u64,
    pub expected_progress_revision: u64,
    pub components: Vec<ItemProgressComponent>,
}

impl ItemProgressCommand {
    /// Validates only explicit progress authority; lifecycle and execution are unrelated.
    ///
    /// # Errors
    /// Returns a content-free validation error for unsupported or malformed commands.
    pub fn validate(&self, item_id: Uuid) -> Result<(), ItemProgressError> {
        if self.schema_version != 1
            || item_id.is_nil()
            || self.operation_id.is_nil()
            || self.expected_item_revision == 0
            || self.expected_item_revision > i64::MAX as u64
            || self.expected_progress_revision > i64::MAX as u64
            || self.components.len() > MAX_PROGRESS_COMPONENTS
        {
            return Err(ItemProgressError::Invalid);
        }
        let mut seen = HashSet::new();
        for component in &self.components {
            if component.id.is_nil()
                || !seen.insert(component.id)
                || !is_valid_progress_label(&component.name, MAX_PROGRESS_NAME_SCALARS)
            {
                return Err(ItemProgressError::Invalid);
            }
            let valid = match &component.value {
                ItemProgressValue::Percentage { basis_points } => {
                    *basis_points <= MAX_PROGRESS_BASIS_POINTS
                }
                ItemProgressValue::Time {
                    elapsed_seconds,
                    remaining_seconds,
                } => {
                    *elapsed_seconds <= MAX_PROGRESS_SECONDS
                        && remaining_seconds.is_none_or(|value| value <= MAX_PROGRESS_SECONDS)
                }
                ItemProgressValue::Quantity {
                    current,
                    unit,
                    target,
                } => {
                    parse_progress_decimal(current).is_ok()
                        && is_valid_progress_label(unit, MAX_PROGRESS_UNIT_SCALARS)
                        && target
                            .as_ref()
                            .is_none_or(|target| parse_progress_decimal(&target.value).is_ok())
                }
            };
            if !valid {
                return Err(ItemProgressError::Invalid);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemProgressSnapshot {
    pub schema_version: u16,
    pub item_id: Uuid,
    pub item_revision: u64,
    pub revision: u64,
    pub components: Vec<ItemProgressComponent>,
    #[schema(required)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl ItemProgressSnapshot {
    pub(crate) fn empty(item_id: Uuid, item_revision: u64) -> Self {
        Self {
            schema_version: 1,
            item_id,
            item_revision,
            revision: 0,
            components: Vec::new(),
            updated_at: None,
        }
    }

    pub(crate) fn replaced(
        &self,
        command: &ItemProgressCommand,
        now: DateTime<Utc>,
    ) -> Result<Self, ItemProgressError> {
        if self.item_revision != command.expected_item_revision {
            return Err(ItemProgressError::ItemStale);
        }
        if self.revision != command.expected_progress_revision {
            return Err(ItemProgressError::ProgressStale);
        }
        let revision = self
            .revision
            .checked_add(1)
            .filter(|value| i64::try_from(*value).is_ok())
            .ok_or(ItemProgressError::Unavailable)?;
        Ok(Self {
            components: command.components.clone(),
            revision,
            updated_at: Some(now),
            ..self.clone()
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemProgressMutation {
    pub operation_id: Uuid,
    pub replayed: bool,
    pub progress: ItemProgressSnapshot,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ItemProgressError {
    #[error("the item progress command is invalid")]
    Invalid,
    #[error("the canonical item revision changed")]
    ItemStale,
    #[error("the independent progress revision changed")]
    ProgressStale,
    #[error("the canonical item was not found")]
    ItemMissing,
    #[error("the operation identity belongs to a different request")]
    OperationReused,
    #[error("item progress authority is unavailable")]
    Unavailable,
}
