use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Utc};
use dayweave_core::{
    HabitMissedDecision, HabitOccurrenceValue, RecurrenceOccurrenceIdentity,
    decide_habit_missed_behavior, is_valid_habit_quantity_unit,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::{Uuid, Variant};

pub const MAX_HABIT_NOTE_CHARS: usize = 10_000;
pub const MAX_HABIT_UNIT_CHARS: usize = dayweave_core::MAX_HABIT_QUANTITY_UNIT_CHARS;
pub const MAX_HABIT_QUANTITY: i64 = dayweave_core::MAX_HABIT_QUANTITY;
pub const MAX_HABIT_ACTUAL_SECONDS: u64 = 366 * 24 * 60 * 60;
pub const MIN_HABIT_DATE_YEAR: i32 = 1900;
pub const MAX_HABIT_DATE_YEAR: i32 = 2200;
const MAX_HABIT_TIMEZONE_CHARS: usize = 100;
const MAX_RFC3339_OFFSET_SECONDS: u32 = 18 * 60 * 60;
const MAX_RECURRENCE_BUCKET_ORDINAL: u16 = u16::MAX - 1;
const MAX_CUSTOM_RECURRENCE_SEQUENCE: u32 = 9_999;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitOutcomeStatus {
    Unresolved,
    Partial,
    Completed,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitOutcomeInput {
    pub status: HabitOutcomeStatus,
    pub progress_basis_points: u16,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
    pub actual_seconds: Option<u64>,
    pub note: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitOutcomeCommand {
    pub operation_id: Uuid,
    /// Zero creates the first projection; every correction supplies its exact current revision.
    pub expected_revision: u64,
    pub outcome: HabitOutcomeInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitPauseStartCommand {
    pub operation_id: Uuid,
    pub pause_id: Uuid,
    /// Pause creation requires zero.
    pub expected_revision: u64,
    pub started_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitPauseResumeCommand {
    pub operation_id: Uuid,
    pub expected_revision: u64,
    pub ended_at: DateTime<Utc>,
}

/// Configured behavior captured when an overdue occurrence is reconciled.
/// Later habit edits therefore cannot reinterpret historical scheduling.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitMissedPolicy {
    Skip,
    Carry,
    ReduceFrequency,
    Ask,
}

impl From<dayweave_core::HabitMissedPolicy> for HabitMissedPolicy {
    fn from(value: dayweave_core::HabitMissedPolicy) -> Self {
        match value {
            dayweave_core::HabitMissedPolicy::Skip => Self::Skip,
            dayweave_core::HabitMissedPolicy::Carry => Self::Carry,
            dayweave_core::HabitMissedPolicy::ReduceFrequency => Self::ReduceFrequency,
            dayweave_core::HabitMissedPolicy::Ask => Self::Ask,
        }
    }
}

impl From<HabitMissedPolicy> for dayweave_core::HabitMissedPolicy {
    fn from(value: HabitMissedPolicy) -> Self {
        match value {
            HabitMissedPolicy::Skip => Self::Skip,
            HabitMissedPolicy::Carry => Self::Carry,
            HabitMissedPolicy::ReduceFrequency => Self::ReduceFrequency,
            HabitMissedPolicy::Ask => Self::Ask,
        }
    }
}

/// Server-computed scheduling action for one overdue occurrence.
/// Carry windows and reduction targets are outputs, never client inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HabitMissedResolutionAction {
    DecisionRequired,
    ReductionPending,
    Cancelled {
        reason: HabitMissedCancellationReason,
        resume_action: HabitMissedResumeAction,
    },
    Skip,
    Carry {
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    },
    ReduceFrequency {
        suppressed_planner_occurrence_ids: Vec<Uuid>,
    },
}

/// Why a previously applicable missed decision became scheduling-inactive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitMissedCancellationReason {
    SourceCompleted,
    SourceSkipped,
    SourcePaused,
    SourceObsolete,
}

/// Active decision family retained while a missed resolution is cancelled so
/// a later outcome correction can restore the exact user/policy choice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitMissedResumeAction {
    DecisionRequired,
    Skip,
    Carry,
    ReduceFrequency,
}

/// Current revisioned projection of missed-occurrence handling.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitMissedResolution {
    pub occurrence_evidence_id: Uuid,
    pub habit_id: Uuid,
    pub source_planner_occurrence_id: Uuid,
    pub revision: u64,
    pub configured_policy: HabitMissedPolicy,
    pub action: HabitMissedResolutionAction,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HabitMissedResolution {
    /// Validates a repository projection before it is placed on the wire or
    /// consumed as authoritative scheduling evidence.
    ///
    /// # Errors
    ///
    /// Returns [`HabitDomainError::InvalidMissedResolution`] when identifiers,
    /// revisions, timestamps, or the configured-policy/action transition do
    /// not form one supported projection state.
    #[allow(clippy::match_same_arms, clippy::unnested_or_patterns)]
    pub fn validate(&self) -> Result<(), HabitDomainError> {
        if self.occurrence_evidence_id.is_nil()
            || self.habit_id.is_nil()
            || self.source_planner_occurrence_id.is_nil()
            || self.revision == 0
            || !valid_api_datetime(self.created_at)
            || !valid_api_datetime(self.updated_at)
            || self.updated_at < self.created_at
        {
            return Err(HabitDomainError::InvalidMissedResolution);
        }
        let shape_is_valid = match (&self.configured_policy, &self.action, self.revision) {
            (HabitMissedPolicy::Ask, HabitMissedResolutionAction::DecisionRequired, 1..)
            | (HabitMissedPolicy::Skip, HabitMissedResolutionAction::Skip, 1..)
            | (
                HabitMissedPolicy::ReduceFrequency,
                HabitMissedResolutionAction::ReductionPending,
                1..,
            )
            | (HabitMissedPolicy::Ask, HabitMissedResolutionAction::Skip, 2..)
            | (HabitMissedPolicy::Ask, HabitMissedResolutionAction::ReductionPending, 2..) => true,
            (
                HabitMissedPolicy::Ask,
                HabitMissedResolutionAction::Carry {
                    window_start,
                    window_end,
                },
                2..,
            )
            | (
                HabitMissedPolicy::Carry,
                HabitMissedResolutionAction::Carry {
                    window_start,
                    window_end,
                },
                1..,
            ) => valid_carry_window(*window_start, *window_end, self.updated_at),
            (
                HabitMissedPolicy::Ask,
                HabitMissedResolutionAction::ReduceFrequency {
                    suppressed_planner_occurrence_ids,
                },
                2..,
            )
            | (
                HabitMissedPolicy::ReduceFrequency,
                HabitMissedResolutionAction::ReduceFrequency {
                    suppressed_planner_occurrence_ids,
                },
                1..,
            ) => valid_reduction_targets(
                suppressed_planner_occurrence_ids,
                self.source_planner_occurrence_id,
            ),
            (HabitMissedPolicy::Ask, HabitMissedResolutionAction::Cancelled { .. }, 2..)
            | (
                HabitMissedPolicy::Skip,
                HabitMissedResolutionAction::Cancelled {
                    resume_action: HabitMissedResumeAction::Skip,
                    ..
                },
                2..,
            )
            | (
                HabitMissedPolicy::Carry,
                HabitMissedResolutionAction::Cancelled {
                    resume_action: HabitMissedResumeAction::Carry,
                    ..
                },
                2..,
            )
            | (
                HabitMissedPolicy::ReduceFrequency,
                HabitMissedResolutionAction::Cancelled {
                    resume_action: HabitMissedResumeAction::ReduceFrequency,
                    ..
                },
                2..,
            ) => true,
            _ => false,
        };
        if shape_is_valid {
            Ok(())
        } else {
            Err(HabitDomainError::InvalidMissedResolution)
        }
    }
}

fn valid_carry_window(start: DateTime<Utc>, end: DateTime<Utc>, updated_at: DateTime<Utc>) -> bool {
    valid_api_datetime(start)
        && valid_api_datetime(end)
        && start == updated_at
        && end > start
        && end - start <= Duration::days(366)
}

fn valid_reduction_targets(targets: &[Uuid], source_planner_occurrence_id: Uuid) -> bool {
    targets.len() == 1
        && !targets[0].is_nil()
        && targets[0].get_version_num() == 5
        && targets[0] != source_planner_occurrence_id
}

#[allow(clippy::unnested_or_patterns)] // Keep the audited state-transition matrix explicit.
pub(crate) fn valid_missed_resolution_transition(
    previous: &HabitMissedResolution,
    next: &HabitMissedResolution,
) -> bool {
    if previous.occurrence_evidence_id != next.occurrence_evidence_id
        || previous.habit_id != next.habit_id
        || previous.source_planner_occurrence_id != next.source_planner_occurrence_id
        || previous.configured_policy != next.configured_policy
        || previous.created_at != next.created_at
        || previous
            .revision
            .checked_add(1)
            .is_none_or(|revision| revision != next.revision)
        || next.updated_at < previous.updated_at
        || next.validate().is_err()
    {
        return false;
    }
    matches!(
        (&previous.action, &next.action),
        (
            HabitMissedResolutionAction::DecisionRequired,
            HabitMissedResolutionAction::Skip
                | HabitMissedResolutionAction::Carry { .. }
                | HabitMissedResolutionAction::ReductionPending
                | HabitMissedResolutionAction::ReduceFrequency { .. }
        ) | (
            HabitMissedResolutionAction::DecisionRequired,
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::DecisionRequired,
                ..
            }
        ) | (
            HabitMissedResolutionAction::ReductionPending,
            HabitMissedResolutionAction::ReduceFrequency { .. }
        ) | (
            HabitMissedResolutionAction::ReductionPending,
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::ReduceFrequency,
                ..
            }
        ) | (
            HabitMissedResolutionAction::ReduceFrequency { .. },
            HabitMissedResolutionAction::ReductionPending
        ) | (
            HabitMissedResolutionAction::ReduceFrequency { .. },
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::ReduceFrequency,
                ..
            }
        ) | (
            HabitMissedResolutionAction::Skip,
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::Skip,
                ..
            }
        ) | (
            HabitMissedResolutionAction::Carry { .. },
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::Carry,
                ..
            }
        ) | (
            HabitMissedResolutionAction::Carry { .. },
            HabitMissedResolutionAction::Carry { .. }
        ) | (
            HabitMissedResolutionAction::Carry { .. },
            HabitMissedResolutionAction::DecisionRequired
        ) | (
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::DecisionRequired,
                ..
            },
            HabitMissedResolutionAction::DecisionRequired
        ) | (
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::Skip,
                ..
            },
            HabitMissedResolutionAction::Skip
        ) | (
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::Carry,
                ..
            },
            HabitMissedResolutionAction::Carry { .. }
        ) | (
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::ReduceFrequency,
                ..
            },
            HabitMissedResolutionAction::ReductionPending
                | HabitMissedResolutionAction::ReduceFrequency { .. }
        )
    )
}

pub(crate) fn valid_explicit_missed_cancellation_transition(
    previous: &HabitMissedResolution,
    next: &HabitMissedResolution,
) -> bool {
    previous.occurrence_evidence_id == next.occurrence_evidence_id
        && previous.habit_id == next.habit_id
        && previous.source_planner_occurrence_id == next.source_planner_occurrence_id
        && previous.configured_policy == HabitMissedPolicy::Ask
        && next.configured_policy == HabitMissedPolicy::Ask
        && previous.created_at == next.created_at
        && previous
            .revision
            .checked_add(1)
            .is_some_and(|revision| revision == next.revision)
        && next.updated_at >= previous.updated_at
        && matches!(
            previous.action,
            HabitMissedResolutionAction::DecisionRequired
        )
        && matches!(
            next.action,
            HabitMissedResolutionAction::Cancelled {
                resume_action: HabitMissedResumeAction::Skip
                    | HabitMissedResumeAction::Carry
                    | HabitMissedResumeAction::ReduceFrequency,
                ..
            }
        )
        && next.validate().is_ok()
}

pub(crate) fn recurrence_identity_ordinal(identity: &Value) -> Option<u32> {
    match serde_json::from_value::<RecurrenceOccurrenceIdentity>(identity.clone()).ok()? {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitMissedReconcileCommand {
    pub operation_id: Uuid,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitMissedExplicitAction {
    Skip,
    Carry,
    ReduceFrequency,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitMissedResolveCommand {
    pub operation_id: Uuid,
    pub expected_revision: u64,
    pub action: HabitMissedExplicitAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitMissedReconcileResult {
    pub resolutions: Vec<HabitMissedResolution>,
    pub has_more: bool,
}

pub(crate) fn derive_missed_resolution_action(
    occurrence: &HabitOccurrence,
    policy: HabitMissedPolicy,
    now: DateTime<Utc>,
) -> Result<HabitMissedResolutionAction, HabitDomainError> {
    let recorded_at = now;
    let now = chrono_to_time(recorded_at)?;
    let value = match occurrence.outcome.as_ref() {
        None
        | Some(HabitOutcome {
            status: HabitOutcomeStatus::Unresolved,
            ..
        }) => HabitOccurrenceValue::pending(),
        // Missed scheduling branches only on the unmet lifecycle class. Raw
        // note/quantity/time evidence remains untouched in the occurrence
        // ledger and is intentionally not rematerialized into core's stricter
        // value grammar merely to derive a skip/move exception.
        Some(outcome) if outcome.status == HabitOutcomeStatus::Partial => {
            HabitOccurrenceValue::partial(
                outcome.progress_basis_points,
                None,
                None,
                None,
                chrono_to_time(outcome.occurred_at.min(recorded_at))?,
            )
        }
        Some(_) => return Err(HabitDomainError::InvalidMissedResolution),
    };
    let decision = decide_habit_missed_behavior(
        policy.into(),
        now,
        chrono_to_time(occurrence.evidence.window_start)?,
        chrono_to_time(occurrence.evidence.window_end)?,
        &value,
        &[],
    )
    .map_err(|_| HabitDomainError::InvalidMissedResolution)?;
    match decision {
        HabitMissedDecision::MarkSkipped { .. } => Ok(HabitMissedResolutionAction::Skip),
        HabitMissedDecision::CarryForward {
            window_start,
            window_end,
        } => Ok(HabitMissedResolutionAction::Carry {
            window_start: time_to_chrono(window_start)?,
            window_end: time_to_chrono(window_end)?,
        }),
        HabitMissedDecision::ReduceFrequency { .. } => {
            Ok(HabitMissedResolutionAction::ReductionPending)
        }
        HabitMissedDecision::RequestDecision => Ok(HabitMissedResolutionAction::DecisionRequired),
        HabitMissedDecision::NoAction { .. } => Err(HabitDomainError::InvalidMissedResolution),
    }
}

fn chrono_to_time(value: DateTime<Utc>) -> Result<time::OffsetDateTime, HabitDomainError> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(value.timestamp_micros()) * 1_000)
        .map_err(|_| HabitDomainError::InvalidMissedResolution)
}

fn time_to_chrono(value: time::OffsetDateTime) -> Result<DateTime<Utc>, HabitDomainError> {
    DateTime::from_timestamp(value.unix_timestamp(), value.nanosecond())
        .map(|value| DateTime::from_timestamp_micros(value.timestamp_micros()).unwrap_or(value))
        .ok_or(HabitDomainError::InvalidMissedResolution)
}

impl HabitOutcomeInput {
    /// Validates the status projection, bounded optional evidence, timestamp
    /// precision and mutation time window.
    ///
    /// # Errors
    ///
    /// Returns [`HabitDomainError`] when any field violates the canonical
    /// occurrence outcome contract.
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), HabitDomainError> {
        match self.status {
            HabitOutcomeStatus::Unresolved
                if self.progress_basis_points != 0
                    || self.quantity.is_some()
                    || self.unit.is_some()
                    || self.actual_seconds.is_some()
                    || self.note.is_some() =>
            {
                return Err(HabitDomainError::InvalidOutcomeShape);
            }
            HabitOutcomeStatus::Partial if !(1..10_000).contains(&self.progress_basis_points) => {
                return Err(HabitDomainError::InvalidOutcomeShape);
            }
            HabitOutcomeStatus::Completed if self.progress_basis_points != 10_000 => {
                return Err(HabitDomainError::InvalidOutcomeShape);
            }
            HabitOutcomeStatus::Skipped if self.progress_basis_points >= 10_000 => {
                return Err(HabitDomainError::InvalidOutcomeShape);
            }
            _ => {}
        }
        if self.quantity.is_some() != self.unit.is_some() {
            return Err(HabitDomainError::QuantityUnitPair);
        }
        if let Some(quantity) = self.quantity
            && quantity.unsigned_abs() > MAX_HABIT_QUANTITY as u64
        {
            return Err(HabitDomainError::InvalidQuantity);
        }
        if self
            .unit
            .as_deref()
            .is_some_and(|unit| !is_valid_habit_quantity_unit(unit))
        {
            return Err(HabitDomainError::InvalidUnit);
        }
        if self
            .actual_seconds
            .is_some_and(|value| value > MAX_HABIT_ACTUAL_SECONDS)
        {
            return Err(HabitDomainError::InvalidActualSeconds);
        }
        if let Some(note) = self.note.as_deref() {
            validate_text(note, MAX_HABIT_NOTE_CHARS, true)
                .map_err(|()| HabitDomainError::InvalidNote)?;
        }
        if !self
            .occurred_at
            .timestamp_subsec_nanos()
            .is_multiple_of(1_000)
            || self.occurred_at > now + Duration::minutes(5)
            || self.occurred_at < now - Duration::days(366 * 20)
        {
            return Err(HabitDomainError::InvalidOccurredAt);
        }
        Ok(())
    }
}

fn validate_text(value: &str, max_chars: usize, multiline: bool) -> Result<(), ()> {
    if value.trim().is_empty() || value.chars().count() > max_chars {
        return Err(());
    }
    if value.chars().any(|character| {
        character.is_control() && !(multiline && matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitOutcome {
    pub revision: u64,
    pub status: HabitOutcomeStatus,
    pub progress_basis_points: u16,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
    pub actual_seconds: Option<u64>,
    pub note: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HabitOutcome {
    #[must_use]
    pub fn from_input(input: HabitOutcomeInput, revision: u64, updated_at: DateTime<Utc>) -> Self {
        Self {
            revision,
            status: input.status,
            progress_basis_points: input.progress_basis_points,
            quantity: input.quantity,
            unit: input.unit,
            actual_seconds: input.actual_seconds,
            note: input.note,
            occurred_at: input.occurred_at,
            updated_at,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrenceEvidence {
    /// Ledger identity. This is the UUID accepted by the outcome PUT route.
    pub id: Uuid,
    pub habit_id: Uuid,
    /// Stable scheduler identity retained for schedule/cache joins.
    pub planner_occurrence_id: Uuid,
    pub source_schedule_revision_id: Uuid,
    pub source_item_revision: u64,
    /// `sha256:<lowercase hex>` over canonical recurrence-affecting policy.
    pub policy_fingerprint: String,
    #[schema(value_type = Object)]
    pub identity: Value,
    pub nominal_start: DateTime<Utc>,
    pub nominal_end: DateTime<Utc>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub local_date: NaiveDate,
    pub timezone_name: String,
    pub expected_duration_seconds: Option<u64>,
    pub expected_quantity: Option<i64>,
    pub expected_unit: Option<String>,
}

impl HabitOccurrenceEvidence {
    /// Validates immutable evidence before it can enter or leave a repository.
    ///
    /// Historical evidence is checked without consulting the habit's current recurrence because
    /// that recurrence may have been edited since publication. The exact core identity union,
    /// deterministic UUID version, and self-contained temporal context remain independently
    /// verifiable.
    ///
    /// # Errors
    ///
    /// Returns [`HabitDomainError::InvalidOccurrenceEvidence`] when the evidence cannot have been
    /// emitted by the supported recurrence engine or cannot be decoded by native clients.
    pub fn validate(&self) -> Result<(), HabitDomainError> {
        if self.id.is_nil()
            || self.habit_id.is_nil()
            || self.id == self.planner_occurrence_id
            || self.planner_occurrence_id.get_version_num() != 5
            || self.planner_occurrence_id.get_variant() != Variant::RFC4122
            || self.source_schedule_revision_id.is_nil()
            || self.source_item_revision == 0
            || !(MIN_HABIT_DATE_YEAR..=MAX_HABIT_DATE_YEAR).contains(&self.local_date.year())
            || !valid_policy_fingerprint(&self.policy_fingerprint)
            || !valid_api_datetime(self.nominal_start)
            || !valid_api_datetime(self.nominal_end)
            || !valid_api_datetime(self.window_start)
            || !valid_api_datetime(self.window_end)
            || self.nominal_start >= self.nominal_end
            || self.window_start >= self.window_end
            || self.nominal_start < self.window_start
            || self.nominal_end > self.window_end
            || self
                .expected_duration_seconds
                .is_some_and(|value| value == 0 || value > MAX_HABIT_ACTUAL_SECONDS)
            || self
                .expected_quantity
                .is_some_and(|value| !(1..=MAX_HABIT_QUANTITY).contains(&value))
            || self.expected_quantity.is_some() != self.expected_unit.is_some()
            || self
                .expected_unit
                .as_deref()
                .is_some_and(|unit| !is_valid_habit_quantity_unit(unit))
        {
            return Err(HabitDomainError::InvalidOccurrenceEvidence);
        }

        let timezone = self
            .timezone_name
            .parse::<chrono_tz::Tz>()
            .ok()
            .filter(|_| {
                !self.timezone_name.is_empty()
                    && self.timezone_name.chars().count() <= MAX_HABIT_TIMEZONE_CHARS
                    && !self.timezone_name.chars().any(char::is_control)
            })
            .ok_or(HabitDomainError::InvalidOccurrenceEvidence)?;
        if self.nominal_start.with_timezone(&timezone).date_naive() != self.local_date {
            return Err(HabitDomainError::InvalidOccurrenceEvidence);
        }
        let identity: RecurrenceOccurrenceIdentity = serde_json::from_value(self.identity.clone())
            .map_err(|_| HabitDomainError::InvalidOccurrenceEvidence)?;
        let canonical_identity = serde_json::to_value(identity)
            .map_err(|_| HabitDomainError::InvalidOccurrenceEvidence)?;
        if canonical_identity != self.identity {
            return Err(HabitDomainError::InvalidOccurrenceEvidence);
        }
        let nominal_last_date = self
            .nominal_end
            .checked_sub_signed(Duration::nanoseconds(1))
            .map(|value| value.with_timezone(&timezone).date_naive())
            .ok_or(HabitDomainError::InvalidOccurrenceEvidence)?;
        if !identity_matches_evidence_context(&identity, self.local_date, nominal_last_date) {
            return Err(HabitDomainError::InvalidOccurrenceEvidence);
        }
        Ok(())
    }
}

fn valid_policy_fingerprint(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_api_datetime(value: DateTime<Utc>) -> bool {
    (1..=9_999).contains(&value.year()) && value.timestamp_subsec_nanos().is_multiple_of(1_000)
}

fn identity_matches_evidence_context(
    identity: &RecurrenceOccurrenceIdentity,
    local_date: NaiveDate,
    nominal_last_date: NaiveDate,
) -> bool {
    let calendar_interval_is_local = nominal_last_date == local_date;
    match *identity {
        RecurrenceOccurrenceIdentity::CalendarDay {
            date,
            bucket_ordinal,
        } => {
            bucket_ordinal <= MAX_RECURRENCE_BUCKET_ORDINAL
                && calendar_interval_is_local
                && time_date_to_naive(date) == Some(local_date)
        }
        RecurrenceOccurrenceIdentity::CalendarWeek {
            week_key,
            bucket_ordinal,
        } => naive_to_time_date(local_date).is_some_and(|date| {
            bucket_ordinal <= MAX_RECURRENCE_BUCKET_ORDINAL
                && calendar_interval_is_local
                && week_key
                    .checked_add(6)
                    .is_some_and(|end| (week_key..=end).contains(&date.to_julian_day()))
        }),
        RecurrenceOccurrenceIdentity::CalendarMonth {
            year,
            month,
            bucket_ordinal,
        } => {
            bucket_ordinal <= MAX_RECURRENCE_BUCKET_ORDINAL
                && calendar_interval_is_local
                && local_date.year() == year
                && u8::try_from(local_date.month()) == Ok(month)
        }
        RecurrenceOccurrenceIdentity::RollingMinutes { index, anchor } => {
            u32::try_from(index).is_ok() && valid_habit_recurrence_anchor(anchor)
        }
        RecurrenceOccurrenceIdentity::AfterCompletion { anchor } => {
            valid_habit_recurrence_anchor(anchor)
        }
        RecurrenceOccurrenceIdentity::RollingMonth {
            cycle,
            index,
            anchor,
        } => {
            (0..=i64::from(i32::MAX)).contains(&cycle)
                && index <= MAX_RECURRENCE_BUCKET_ORDINAL
                && valid_habit_recurrence_anchor(anchor)
        }
        RecurrenceOccurrenceIdentity::Custom => false,
        RecurrenceOccurrenceIdentity::CustomRule {
            rule_id,
            sequence,
            date,
        } => {
            sequence <= MAX_CUSTOM_RECURRENCE_SEQUENCE
                && calendar_interval_is_local
                && rule_id.get_version_num() == 5
                && rule_id.get_variant() == Variant::RFC4122
                && time_date_to_naive(date) == Some(local_date)
        }
    }
}

pub(crate) fn valid_habit_recurrence_anchor(value: time::OffsetDateTime) -> bool {
    value.nanosecond().is_multiple_of(1_000)
        && (1..=9_999).contains(&value.year())
        && value.offset().whole_seconds().unsigned_abs() <= MAX_RFC3339_OFFSET_SECONDS
}

fn time_date_to_naive(value: time::Date) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(
        value.year(),
        u32::from(u8::from(value.month())),
        u32::from(value.day()),
    )
}

fn naive_to_time_date(value: NaiveDate) -> Option<time::Date> {
    let month = u8::try_from(value.month())
        .ok()
        .and_then(|month| time::Month::try_from(month).ok())?;
    time::Date::from_calendar_date(value.year(), month, u8::try_from(value.day()).ok()?).ok()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrence {
    pub evidence: HabitOccurrenceEvidence,
    pub outcome: Option<HabitOutcome>,
    #[serde(default)]
    pub missed_resolution: Option<HabitMissedResolution>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitPause {
    pub id: Uuid,
    pub habit_id: Uuid,
    pub revision: u64,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub preserves_streak: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HabitMutation<T> {
    pub value: T,
    pub replayed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Exact full upserts are the durable/offline wire contract.
pub enum HabitDeltaChange {
    OccurrenceUpsert { occurrence: HabitOccurrence },
    PauseUpsert { pause: HabitPause },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HabitDeltaPage {
    pub changes: Vec<HabitDeltaChange>,
    pub watermark: u64,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitAnalyticsBucket {
    Day,
    Week,
    Month,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HabitSupportiveFactCode {
    NoData,
    ActiveStreak,
    StrongAdherence,
    FreshStartAvailable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, ToSchema)]
pub struct HabitAnalyticsTotals {
    pub expected: u64,
    pub eligible: u64,
    pub completed: u64,
    pub partial: u64,
    pub skipped: u64,
    pub missed: u64,
    pub excused: u64,
    pub unresolved: u64,
    pub adherence_basis_points: u16,
    pub actual_seconds_total: u64,
    pub quantity_totals: Vec<HabitQuantityTotal>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, ToSchema)]
pub struct HabitQuantityTotal {
    pub unit: String,
    pub amount: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, ToSchema)]
pub struct HabitTrendBucket {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    #[serde(flatten)]
    pub totals: HabitAnalyticsTotals,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, ToSchema)]
pub struct HabitAnalytics {
    pub habit_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub bucket: HabitAnalyticsBucket,
    #[serde(flatten)]
    pub totals: HabitAnalyticsTotals,
    pub current_streak: u32,
    pub longest_streak: u32,
    pub trends: Vec<HabitTrendBucket>,
    pub supportive_fact_codes: Vec<HabitSupportiveFactCode>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ClassifiedState {
    Completed,
    Partial,
    Skipped,
    Missed,
    Excused,
    #[default]
    Unresolved,
}

pub(crate) fn effective_lifecycle_window(
    occurrence: &HabitOccurrence,
) -> (DateTime<Utc>, DateTime<Utc>) {
    occurrence
        .missed_resolution
        .as_ref()
        .and_then(|resolution| match &resolution.action {
            HabitMissedResolutionAction::Carry {
                window_start,
                window_end,
            } => Some((*window_start, *window_end)),
            _ => None,
        })
        .unwrap_or((
            occurrence.evidence.window_start,
            occurrence.evidence.window_end,
        ))
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HabitAnalyticsLifecycle<'a> {
    effective_reduction_targets: &'a BTreeSet<Uuid>,
    pauses: &'a [HabitPause],
}

impl<'a> HabitAnalyticsLifecycle<'a> {
    pub(crate) fn new(
        effective_reduction_targets: &'a BTreeSet<Uuid>,
        pauses: &'a [HabitPause],
    ) -> Self {
        Self {
            effective_reduction_targets,
            pauses,
        }
    }
}

#[must_use]
pub(crate) fn calculate_analytics(
    habit_id: Uuid,
    occurrences: &[HabitOccurrence],
    lifecycle: HabitAnalyticsLifecycle<'_>,
    start_date: NaiveDate,
    end_date: NaiveDate,
    bucket: HabitAnalyticsBucket,
    now: DateTime<Utc>,
) -> HabitAnalytics {
    let mut totals = HabitAnalyticsTotals::default();
    let mut raw_quantities = BTreeMap::<String, i64>::new();
    let mut adherence_sum = 0_u64;
    let mut buckets =
        BTreeMap::<NaiveDate, (NaiveDate, HabitAnalyticsTotals, BTreeMap<String, i64>, u64)>::new();
    let mut days = BTreeMap::<NaiveDate, Vec<ClassifiedState>>::new();

    for occurrence in occurrences.iter().filter(|value| {
        value.evidence.habit_id == habit_id
            && value.evidence.local_date >= start_date
            && value.evidence.local_date <= end_date
            && !lifecycle
                .effective_reduction_targets
                .contains(&value.evidence.planner_occurrence_id)
    }) {
        let state = classify(occurrence, lifecycle.pauses, now);
        accumulate(
            &mut totals,
            &mut raw_quantities,
            &mut adherence_sum,
            occurrence,
            state,
        );
        let (_, due_at) = effective_lifecycle_window(occurrence);
        if state != ClassifiedState::Excused && due_at <= now {
            days.entry(occurrence.evidence.local_date)
                .or_default()
                .push(state);
        }
        let bucket_start = bucket_start(occurrence.evidence.local_date, bucket);
        let bucket_end = bucket_end(bucket_start, bucket).min(end_date);
        let entry = buckets.entry(bucket_start).or_insert_with(|| {
            (
                bucket_end,
                HabitAnalyticsTotals::default(),
                BTreeMap::new(),
                0,
            )
        });
        accumulate(&mut entry.1, &mut entry.2, &mut entry.3, occurrence, state);
    }
    finish_totals(&mut totals, raw_quantities, adherence_sum);

    let mut trends = Vec::with_capacity(buckets.len());
    for (bucket_start, (bucket_end, mut bucket_totals, quantities, bucket_adherence)) in buckets {
        finish_totals(&mut bucket_totals, quantities, bucket_adherence);
        trends.push(HabitTrendBucket {
            start_date: bucket_start.max(start_date),
            end_date: bucket_end,
            totals: bucket_totals,
        });
    }

    let mut current = 0_u32;
    let mut longest = 0_u32;
    for states in days.values() {
        if states
            .iter()
            .all(|state| *state == ClassifiedState::Completed)
        {
            current = current.saturating_add(1);
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let mut supportive = BTreeSet::new();
    if totals.expected == 0 {
        supportive.insert(HabitSupportiveFactCode::NoData);
    }
    if current > 0 {
        supportive.insert(HabitSupportiveFactCode::ActiveStreak);
    }
    if totals.eligible > 0 && totals.adherence_basis_points >= 8_000 {
        supportive.insert(HabitSupportiveFactCode::StrongAdherence);
    }
    if totals.missed > 0 {
        supportive.insert(HabitSupportiveFactCode::FreshStartAvailable);
    }
    HabitAnalytics {
        habit_id,
        start_date,
        end_date,
        bucket,
        totals,
        current_streak: current,
        longest_streak: longest,
        trends,
        supportive_fact_codes: supportive.into_iter().collect(),
    }
}

fn classify(
    occurrence: &HabitOccurrence,
    pauses: &[HabitPause],
    now: DateTime<Utc>,
) -> ClassifiedState {
    let (window_start, window_end) = effective_lifecycle_window(occurrence);
    if pauses.iter().any(|pause| {
        pause.habit_id == occurrence.evidence.habit_id
            && pause.preserves_streak
            && pause.started_at < window_end
            && pause.ended_at.is_none_or(|ended| ended > window_start)
    }) {
        return ClassifiedState::Excused;
    }
    match occurrence.outcome.as_ref().map(|outcome| outcome.status) {
        Some(HabitOutcomeStatus::Completed) => ClassifiedState::Completed,
        Some(HabitOutcomeStatus::Unresolved | HabitOutcomeStatus::Partial) | None
            if occurrence
                .missed_resolution
                .as_ref()
                .is_some_and(|resolution| {
                    matches!(resolution.action, HabitMissedResolutionAction::Skip)
                }) =>
        {
            ClassifiedState::Skipped
        }
        Some(HabitOutcomeStatus::Partial) => ClassifiedState::Partial,
        Some(HabitOutcomeStatus::Skipped) => ClassifiedState::Skipped,
        Some(HabitOutcomeStatus::Unresolved) | None => match occurrence
            .missed_resolution
            .as_ref()
            .map(|resolution| &resolution.action)
        {
            Some(HabitMissedResolutionAction::Skip) => ClassifiedState::Skipped,
            Some(HabitMissedResolutionAction::Carry { window_end, .. }) if *window_end > now => {
                ClassifiedState::Unresolved
            }
            Some(
                HabitMissedResolutionAction::Carry { .. }
                | HabitMissedResolutionAction::DecisionRequired
                | HabitMissedResolutionAction::ReductionPending
                | HabitMissedResolutionAction::ReduceFrequency { .. },
            )
            | None
                if occurrence.evidence.window_end <= now =>
            {
                ClassifiedState::Missed
            }
            Some(_) | None => ClassifiedState::Unresolved,
        },
    }
}

fn accumulate(
    totals: &mut HabitAnalyticsTotals,
    quantities: &mut BTreeMap<String, i64>,
    adherence_sum: &mut u64,
    occurrence: &HabitOccurrence,
    state: ClassifiedState,
) {
    totals.expected = totals.expected.saturating_add(1);
    match state {
        ClassifiedState::Excused => totals.excused = totals.excused.saturating_add(1),
        ClassifiedState::Completed => {
            totals.eligible = totals.eligible.saturating_add(1);
            totals.completed = totals.completed.saturating_add(1);
            *adherence_sum = adherence_sum.saturating_add(10_000);
        }
        ClassifiedState::Partial => {
            totals.eligible = totals.eligible.saturating_add(1);
            totals.partial = totals.partial.saturating_add(1);
            *adherence_sum = adherence_sum.saturating_add(u64::from(
                occurrence
                    .outcome
                    .as_ref()
                    .map_or(0, |outcome| outcome.progress_basis_points),
            ));
        }
        ClassifiedState::Skipped => {
            totals.eligible = totals.eligible.saturating_add(1);
            totals.skipped = totals.skipped.saturating_add(1);
            *adherence_sum = adherence_sum.saturating_add(u64::from(
                occurrence
                    .outcome
                    .as_ref()
                    .map_or(0, |outcome| outcome.progress_basis_points),
            ));
        }
        ClassifiedState::Missed => {
            totals.eligible = totals.eligible.saturating_add(1);
            totals.missed = totals.missed.saturating_add(1);
        }
        ClassifiedState::Unresolved => {
            totals.eligible = totals.eligible.saturating_add(1);
            totals.unresolved = totals.unresolved.saturating_add(1);
        }
    }
    if let Some(outcome) = occurrence.outcome.as_ref() {
        totals.actual_seconds_total = totals
            .actual_seconds_total
            .saturating_add(outcome.actual_seconds.unwrap_or(0));
        if let (Some(quantity), Some(unit)) = (outcome.quantity, outcome.unit.as_ref()) {
            let entry = quantities.entry(unit.clone()).or_default();
            *entry = entry.saturating_add(quantity);
        }
    }
}

fn finish_totals(
    totals: &mut HabitAnalyticsTotals,
    quantities: BTreeMap<String, i64>,
    adherence_sum: u64,
) {
    let mean = adherence_sum
        .saturating_add(totals.eligible / 2)
        .checked_div(totals.eligible)
        .unwrap_or(0);
    totals.adherence_basis_points = u16::try_from(mean).unwrap_or(10_000).min(10_000);
    totals.quantity_totals = quantities
        .into_iter()
        .map(|(unit, amount)| HabitQuantityTotal { unit, amount })
        .collect();
}

fn bucket_start(date: NaiveDate, bucket: HabitAnalyticsBucket) -> NaiveDate {
    match bucket {
        HabitAnalyticsBucket::Day => date,
        HabitAnalyticsBucket::Week => {
            date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
        }
        HabitAnalyticsBucket::Month => date.with_day(1).unwrap_or(date),
    }
}

fn bucket_end(start: NaiveDate, bucket: HabitAnalyticsBucket) -> NaiveDate {
    match bucket {
        HabitAnalyticsBucket::Day => start,
        HabitAnalyticsBucket::Week => start + Duration::days(6),
        HabitAnalyticsBucket::Month => {
            let next = if start.month() == 12 {
                NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
            };
            next.map_or(start, |date| date - Duration::days(1))
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum HabitDomainError {
    #[error("outcome fields do not match its status")]
    InvalidOutcomeShape,
    #[error("quantity and unit must be provided together")]
    QuantityUnitPair,
    #[error("quantity is outside the supported integer range")]
    InvalidQuantity,
    #[error("unit must be 1-200 non-control characters")]
    InvalidUnit,
    #[error("actual_seconds exceeds the supported bound")]
    InvalidActualSeconds,
    #[error("note must be 1-10000 safe characters")]
    InvalidNote,
    #[error("occurred_at is outside the supported precision or time range")]
    InvalidOccurredAt,
    #[error("habit occurrence evidence has invalid recurrence identity or temporal context")]
    InvalidOccurrenceEvidence,
    #[error("missed-occurrence resolution is invalid")]
    InvalidMissedResolution,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[derive(serde::Deserialize)]
    struct EvidenceFixtureFile {
        schema: String,
        base_evidence: Value,
        valid_cases: Vec<EvidenceFixtureCase>,
        invalid_cases: Vec<EvidenceFixtureCase>,
    }

    #[derive(serde::Deserialize)]
    struct EvidenceFixtureCase {
        name: String,
        patch: Value,
    }

    fn evidence_fixture() -> EvidenceFixtureFile {
        serde_json::from_str(include_str!(
            "../../../../fixtures/habit-protocol/occurrence-evidence-v1.json"
        ))
        .expect("shared habit occurrence evidence fixture must be strict JSON")
    }

    fn patched_evidence(base: &Value, patch: &Value) -> Value {
        let mut result = base
            .as_object()
            .expect("base habit evidence must be an object")
            .clone();
        for (key, value) in patch
            .as_object()
            .expect("habit evidence patch must be an object")
        {
            result.insert(key.clone(), value.clone());
        }
        Value::Object(result)
    }

    fn occurrence(date: NaiveDate, status: Option<HabitOutcomeStatus>) -> HabitOccurrence {
        let start = date.and_hms_opt(8, 0, 0).unwrap().and_utc();
        let habit_id = Uuid::from_u128(1);
        HabitOccurrence {
            evidence: HabitOccurrenceEvidence {
                id: Uuid::new_v4(),
                habit_id,
                planner_occurrence_id: Uuid::new_v5(
                    &habit_id,
                    format!("daily:{date}:0").as_bytes(),
                ),
                source_schedule_revision_id: Uuid::new_v4(),
                source_item_revision: 1,
                policy_fingerprint: format!("sha256:{}", "0".repeat(64)),
                identity: serde_json::json!({
                    "type": "calendar_day",
                    "date": date,
                    "bucket_ordinal": 0
                }),
                nominal_start: start,
                nominal_end: start + Duration::hours(1),
                window_start: start,
                window_end: start + Duration::hours(1),
                local_date: date,
                timezone_name: "Europe/Paris".to_owned(),
                expected_duration_seconds: Some(3600),
                expected_quantity: None,
                expected_unit: None,
            },
            outcome: status.map(|status| HabitOutcome {
                revision: 1,
                progress_basis_points: match status {
                    HabitOutcomeStatus::Completed => 10_000,
                    HabitOutcomeStatus::Partial => 5_000,
                    HabitOutcomeStatus::Unresolved | HabitOutcomeStatus::Skipped => 0,
                },
                status,
                quantity: None,
                unit: None,
                actual_seconds: None,
                note: None,
                occurred_at: start,
                updated_at: start,
            }),
            missed_resolution: None,
        }
    }

    #[test]
    fn shared_occurrence_evidence_fixtures_define_the_server_contract() {
        let fixture = evidence_fixture();
        assert_eq!(
            fixture.schema,
            "dayweave.habit-occurrence-evidence-fixtures/1"
        );
        assert!(!fixture.valid_cases.is_empty());
        assert!(!fixture.invalid_cases.is_empty());
        let mut names = BTreeSet::new();

        for case in &fixture.valid_cases {
            assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
            let raw = patched_evidence(&fixture.base_evidence, &case.patch);
            let evidence: HabitOccurrenceEvidence = serde_json::from_value(raw.clone())
                .unwrap_or_else(|error| panic!("{} did not decode: {error}", case.name));
            evidence
                .validate()
                .unwrap_or_else(|error| panic!("{} did not validate: {error}", case.name));
            assert_eq!(
                serde_json::to_value(evidence).expect("valid evidence must encode"),
                raw,
                "{} did not retain its exact wire value",
                case.name
            );
        }

        for case in &fixture.invalid_cases {
            assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
            let raw = patched_evidence(&fixture.base_evidence, &case.patch);
            let accepted = serde_json::from_value::<HabitOccurrenceEvidence>(raw)
                .is_ok_and(|evidence| evidence.validate().is_ok());
            assert!(!accepted, "{} unexpectedly passed", case.name);
        }
    }

    #[test]
    fn recurrence_evidence_enforces_native_identity_and_date_bounds() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();
        let evidence = occurrence(date, None).evidence;
        assert!(evidence.validate().is_ok());

        let validates_identity = |identity: Value| {
            let mut candidate = evidence.clone();
            candidate.identity = identity;
            candidate.validate().is_ok()
        };
        assert!(validates_identity(serde_json::json!({
            "type": "calendar_day",
            "date": date,
            "bucket_ordinal": 65_534
        })));
        assert!(!validates_identity(serde_json::json!({
            "type": "calendar_day",
            "date": date,
            "bucket_ordinal": 65_535
        })));
        assert!(validates_identity(serde_json::json!({
            "type": "rolling_month",
            "cycle": 2_147_483_647_i64,
            "index": 65_534,
            "anchor": "0001-01-01T00:00:00Z"
        })));
        assert!(!validates_identity(serde_json::json!({
            "type": "rolling_month",
            "cycle": 0,
            "index": 65_535,
            "anchor": "0001-01-01T00:00:00Z"
        })));
        assert!(validates_identity(serde_json::json!({
            "type": "rolling_minutes",
            "index": 4_294_967_295_u64,
            "anchor": "9999-12-31T23:59:59.999999Z"
        })));
        assert!(!validates_identity(serde_json::json!({
            "type": "rolling_minutes",
            "index": 0,
            "anchor": "0000-01-01T00:00:00Z"
        })));
        assert!(validates_identity(serde_json::json!({
            "type": "rolling_minutes",
            "index": 0,
            "anchor": "2026-09-04T08:00:00+18:00"
        })));
        assert!(!validates_identity(serde_json::json!({
            "type": "rolling_minutes",
            "index": 0,
            "anchor": "2026-09-04T08:00:00+18:01"
        })));
        assert!(!validates_identity(serde_json::json!({
            "type": "rolling_minutes",
            "index": 0,
            "anchor": "2026-09-04T08:00:00.000000001Z"
        })));

        let rule_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"bounded-custom-rule");
        assert!(validates_identity(serde_json::json!({
            "type": "custom_rule",
            "rule_id": rule_id,
            "sequence": 9_999,
            "date": date
        })));
        assert!(!validates_identity(serde_json::json!({
            "type": "custom_rule",
            "rule_id": rule_id,
            "sequence": 10_000,
            "date": date
        })));

        for year in [MIN_HABIT_DATE_YEAR, MAX_HABIT_DATE_YEAR] {
            assert!(
                occurrence(NaiveDate::from_ymd_opt(year, 6, 1).unwrap(), None)
                    .evidence
                    .validate()
                    .is_ok()
            );
        }
        for year in [MIN_HABIT_DATE_YEAR - 1, MAX_HABIT_DATE_YEAR + 1] {
            assert!(
                occurrence(NaiveDate::from_ymd_opt(year, 6, 1).unwrap(), None)
                    .evidence
                    .validate()
                    .is_err()
            );
        }

        let mut expanded_window_year = evidence;
        expanded_window_year.window_start = NaiveDate::from_ymd_opt(0, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        assert!(expanded_window_year.validate().is_err());
    }

    #[test]
    fn recurrence_evidence_requires_rfc4122_v5_ids_and_canonical_identity_json() {
        fn with_ncs_variant(value: Uuid) -> Uuid {
            let mut bytes = *value.as_bytes();
            bytes[8] &= 0x3f;
            Uuid::from_bytes(bytes)
        }

        let date = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();
        let evidence = occurrence(date, None).evidence;

        let mut wrong_planner_variant = evidence.clone();
        wrong_planner_variant.planner_occurrence_id =
            with_ncs_variant(wrong_planner_variant.planner_occurrence_id);
        assert_eq!(
            wrong_planner_variant
                .planner_occurrence_id
                .get_version_num(),
            5
        );
        assert_ne!(
            wrong_planner_variant.planner_occurrence_id.get_variant(),
            Variant::RFC4122
        );
        assert!(wrong_planner_variant.validate().is_err());

        let rule_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"variant-bound-custom-rule");
        let mut wrong_rule_variant = evidence.clone();
        wrong_rule_variant.identity = serde_json::json!({
            "type": "custom_rule",
            "rule_id": with_ncs_variant(rule_id),
            "sequence": 0,
            "date": date
        });
        assert!(wrong_rule_variant.validate().is_err());

        let noncanonical_anchor = serde_json::json!({
            "type": "rolling_minutes",
            "index": 0,
            "anchor": "2026-09-04 08:00:00.1234560000z"
        });
        assert!(
            serde_json::from_value::<RecurrenceOccurrenceIdentity>(noncanonical_anchor.clone())
                .is_ok(),
            "the typed decoder remains permissive, so evidence must enforce its canonical encoding"
        );
        let mut noncanonical = evidence;
        noncanonical.identity = noncanonical_anchor;
        assert!(noncanonical.validate().is_err());
    }

    #[test]
    fn analytics_partitions_eligible_occurrences_and_rounds_integer_adherence() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let values = vec![
            occurrence(start, Some(HabitOutcomeStatus::Completed)),
            occurrence(start + Duration::days(1), Some(HabitOutcomeStatus::Partial)),
            occurrence(start + Duration::days(2), None),
        ];
        let analytics = calculate_analytics(
            Uuid::from_u128(1),
            &values,
            HabitAnalyticsLifecycle::new(&BTreeSet::new(), &[]),
            start,
            start + Duration::days(2),
            HabitAnalyticsBucket::Day,
            start.and_hms_opt(23, 0, 0).unwrap().and_utc() + Duration::days(3),
        );
        assert_eq!(analytics.totals.expected, 3);
        assert_eq!(analytics.totals.eligible, 3);
        assert_eq!(analytics.totals.completed, 1);
        assert_eq!(analytics.totals.partial, 1);
        assert_eq!(analytics.totals.missed, 1);
        assert_eq!(analytics.totals.adherence_basis_points, 5_000);
        assert_eq!(analytics.longest_streak, 1);
    }

    #[test]
    fn analytics_removes_effective_reduction_targets_from_success_demand() {
        let source_date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut source = occurrence(source_date, None);
        let target = occurrence(source_date + Duration::days(1), None);
        source.missed_resolution = Some(HabitMissedResolution {
            occurrence_evidence_id: source.evidence.id,
            habit_id: source.evidence.habit_id,
            source_planner_occurrence_id: source.evidence.planner_occurrence_id,
            revision: 1,
            configured_policy: HabitMissedPolicy::ReduceFrequency,
            action: HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids: vec![target.evidence.planner_occurrence_id],
            },
            created_at: source.evidence.window_end,
            updated_at: source.evidence.window_end,
        });
        let effective_targets = BTreeSet::from([target.evidence.planner_occurrence_id]);

        let analytics = calculate_analytics(
            source.evidence.habit_id,
            &[source, target],
            HabitAnalyticsLifecycle::new(&effective_targets, &[]),
            source_date,
            source_date + Duration::days(1),
            HabitAnalyticsBucket::Day,
            source_date.and_hms_opt(12, 0, 0).unwrap().and_utc() + Duration::days(2),
        );

        assert_eq!(analytics.totals.expected, 1);
        assert_eq!(analytics.totals.eligible, 1);
        assert_eq!(analytics.totals.missed, 1);
        assert_eq!(analytics.trends.len(), 1);
    }

    #[test]
    fn active_carry_uses_its_derived_window_for_pauses_and_streak_deadline() {
        let first_date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let completed = occurrence(first_date, Some(HabitOutcomeStatus::Completed));
        let mut carried = occurrence(first_date + Duration::days(1), None);
        let carry_start = carried.evidence.window_end + Duration::minutes(1);
        let carry_end = carry_start + Duration::days(1);
        carried.missed_resolution = Some(HabitMissedResolution {
            occurrence_evidence_id: carried.evidence.id,
            habit_id: carried.evidence.habit_id,
            source_planner_occurrence_id: carried.evidence.planner_occurrence_id,
            revision: 1,
            configured_policy: HabitMissedPolicy::Carry,
            action: HabitMissedResolutionAction::Carry {
                window_start: carry_start,
                window_end: carry_end,
            },
            created_at: carry_start,
            updated_at: carry_start,
        });
        let now = carry_start + Duration::hours(1);

        let analytics = calculate_analytics(
            completed.evidence.habit_id,
            &[completed.clone(), carried.clone()],
            HabitAnalyticsLifecycle::new(&BTreeSet::new(), &[]),
            first_date,
            first_date + Duration::days(1),
            HabitAnalyticsBucket::Day,
            now,
        );
        assert_eq!(analytics.totals.unresolved, 1);
        assert_eq!(analytics.current_streak, 1);
        assert_eq!(analytics.longest_streak, 1);

        let preserving_pause = HabitPause {
            id: Uuid::new_v4(),
            habit_id: carried.evidence.habit_id,
            revision: 1,
            started_at: carry_start + Duration::minutes(10),
            ended_at: Some(carry_start + Duration::minutes(20)),
            preserves_streak: true,
            created_at: carry_start + Duration::minutes(10),
            updated_at: carry_start + Duration::minutes(20),
        };
        let paused = calculate_analytics(
            completed.evidence.habit_id,
            &[completed, carried],
            HabitAnalyticsLifecycle::new(&BTreeSet::new(), &[preserving_pause]),
            first_date,
            first_date + Duration::days(1),
            HabitAnalyticsBucket::Day,
            now,
        );
        assert_eq!(paused.totals.excused, 1);
        assert_eq!(paused.totals.eligible, 1);
        assert_eq!(paused.current_streak, 1);
    }

    #[test]
    fn missed_cancellation_transitions_preserve_the_exact_resume_family() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 8, 0, 0).unwrap();
        let base = HabitMissedResolution {
            occurrence_evidence_id: Uuid::from_u128(10),
            habit_id: Uuid::from_u128(11),
            source_planner_occurrence_id: Uuid::from_u128(12),
            revision: 1,
            configured_policy: HabitMissedPolicy::Ask,
            action: HabitMissedResolutionAction::DecisionRequired,
            created_at: now,
            updated_at: now,
        };
        let automatic = HabitMissedResolution {
            revision: 2,
            action: HabitMissedResolutionAction::Cancelled {
                reason: HabitMissedCancellationReason::SourcePaused,
                resume_action: HabitMissedResumeAction::DecisionRequired,
            },
            updated_at: now + Duration::seconds(1),
            ..base.clone()
        };
        assert!(valid_missed_resolution_transition(&base, &automatic));
        assert!(!valid_explicit_missed_cancellation_transition(
            &base, &automatic
        ));
        let explicit = HabitMissedResolution {
            action: HabitMissedResolutionAction::Cancelled {
                reason: HabitMissedCancellationReason::SourceObsolete,
                resume_action: HabitMissedResumeAction::Carry,
            },
            ..automatic.clone()
        };
        assert!(!valid_missed_resolution_transition(&base, &explicit));
        assert!(valid_explicit_missed_cancellation_transition(
            &base, &explicit
        ));

        let skip = HabitMissedResolution {
            configured_policy: HabitMissedPolicy::Skip,
            action: HabitMissedResolutionAction::Skip,
            ..base
        };
        let cancelled_skip = HabitMissedResolution {
            revision: 2,
            action: HabitMissedResolutionAction::Cancelled {
                reason: HabitMissedCancellationReason::SourceCompleted,
                resume_action: HabitMissedResumeAction::Skip,
            },
            updated_at: now + Duration::seconds(1),
            ..skip.clone()
        };
        assert!(valid_missed_resolution_transition(&skip, &cancelled_skip));
        let wrong_family = HabitMissedResolution {
            action: HabitMissedResolutionAction::Cancelled {
                reason: HabitMissedCancellationReason::SourceCompleted,
                resume_action: HabitMissedResumeAction::Carry,
            },
            ..cancelled_skip
        };
        assert!(!valid_missed_resolution_transition(&skip, &wrong_family));
    }

    #[test]
    fn frequency_reduction_requires_a_v5_planner_target() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 8, 0, 0).unwrap();
        let source_planner_occurrence_id =
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"missed-reduction-source");
        let mut resolution = HabitMissedResolution {
            occurrence_evidence_id: Uuid::from_u128(10),
            habit_id: Uuid::from_u128(11),
            source_planner_occurrence_id,
            revision: 1,
            configured_policy: HabitMissedPolicy::ReduceFrequency,
            action: HabitMissedResolutionAction::ReduceFrequency {
                suppressed_planner_occurrence_ids: vec![Uuid::new_v4()],
            },
            created_at: now,
            updated_at: now,
        };
        assert!(resolution.validate().is_err());
        resolution.action = HabitMissedResolutionAction::ReduceFrequency {
            suppressed_planner_occurrence_ids: vec![Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                b"missed-reduction-target",
            )],
        };
        assert!(resolution.validate().is_ok());
    }

    #[test]
    fn preserving_pause_excuses_an_overlap_without_breaking_streak() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let value = occurrence(date, None);
        let pause = HabitPause {
            id: Uuid::new_v4(),
            habit_id: value.evidence.habit_id,
            revision: 1,
            started_at: value.evidence.window_start,
            ended_at: Some(value.evidence.window_end),
            preserves_streak: true,
            created_at: value.evidence.window_start,
            updated_at: value.evidence.window_end,
        };
        let analytics = calculate_analytics(
            value.evidence.habit_id,
            &[value],
            HabitAnalyticsLifecycle::new(&BTreeSet::new(), &[pause]),
            date,
            date,
            HabitAnalyticsBucket::Day,
            date.and_hms_opt(23, 0, 0).unwrap().and_utc(),
        );
        assert_eq!(analytics.totals.expected, 1);
        assert_eq!(analytics.totals.excused, 1);
        assert_eq!(analytics.totals.eligible, 0);
    }
}
