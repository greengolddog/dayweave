//! Deterministic habit occurrence lifecycle and analytics primitives.
//!
//! The types in this module deliberately contain no storage, timezone database,
//! wall-clock, or messaging dependencies. Callers supply resolved occurrence
//! windows and local dates, then persist the returned transition preimage and
//! projection in one transaction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Date, Duration, OffsetDateTime};

use crate::{DayOfWeek, ItemId, OccurrenceId};

/// One hundred percent expressed in basis points.
pub const HABIT_BASIS_POINTS_SCALE: u16 = 10_000;

/// Maximum UTF-8 size of an occurrence note accepted by the pure domain.
pub const MAX_HABIT_OCCURRENCE_NOTE_BYTES: usize = 16 * 1024;

/// Maximum UTF-8 size of a quantitative unit label.
pub const MAX_HABIT_QUANTITY_UNIT_BYTES: usize = 128;

/// Largest history accepted by one analytics projection.
pub const MAX_HABIT_ANALYTICS_OCCURRENCES: usize = 1_000_000;

/// Largest inclusive local-date range accepted by one analytics projection.
pub const MAX_HABIT_ANALYTICS_RANGE_DAYS: i64 = 36_600;

/// Quantitative progress for one habit occurrence.
///
/// `completed_units` is cumulative, not a delta. The target and unit are a
/// snapshot so later edits to a habit do not reinterpret historical progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitQuantityProgress {
    pub completed_units: u64,
    pub target_units: u64,
    pub unit: String,
}

impl HabitQuantityProgress {
    fn validate_common(&self) -> Result<(), HabitOccurrenceError> {
        if self.completed_units == 0 || self.target_units == 0 {
            return Err(HabitOccurrenceError::InvalidQuantity);
        }
        if self.completed_units > i64::MAX as u64 || self.target_units > i64::MAX as u64 {
            return Err(HabitOccurrenceError::InvalidQuantity);
        }
        if self.unit.is_empty()
            || self.unit.trim() != self.unit
            || self.unit.len() > MAX_HABIT_QUANTITY_UNIT_BYTES
            || self.unit.chars().any(char::is_control)
        {
            return Err(HabitOccurrenceError::InvalidQuantityUnit);
        }
        Ok(())
    }

    fn validate_partial(&self) -> Result<(), HabitOccurrenceError> {
        self.validate_common()?;
        if self.completed_units >= self.target_units {
            return Err(HabitOccurrenceError::InvalidPartialQuantity);
        }
        Ok(())
    }

    fn validate_completed(&self) -> Result<(), HabitOccurrenceError> {
        self.validate_common()?;
        if self.completed_units < self.target_units {
            return Err(HabitOccurrenceError::IncompleteCompletedQuantity);
        }
        Ok(())
    }

    fn basis_points(&self) -> u16 {
        let scaled = u128::from(self.completed_units)
            .saturating_mul(u128::from(HABIT_BASIS_POINTS_SCALE))
            / u128::from(self.target_units);
        u16::try_from(scaled.min(u128::from(HABIT_BASIS_POINTS_SCALE)))
            .expect("a value capped to the basis-point scale fits in u16")
    }
}

/// Why an occurrence is represented as skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitSkipReason {
    User,
    MissedPolicy,
}

/// Current outcome projection for one generated occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HabitOccurrenceOutcome {
    Pending,
    Partial {
        quantity: HabitQuantityProgress,
    },
    Completed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantity: Option<HabitQuantityProgress>,
    },
    Skipped {
        reason: HabitSkipReason,
    },
}

impl HabitOccurrenceOutcome {
    /// Validates quantitative invariants without consulting mutable habit data.
    ///
    /// # Errors
    ///
    /// Returns a quantity error when partial progress reaches the target, a
    /// completed quantitative result is below its target, or a unit is unsafe.
    pub fn validate(&self) -> Result<(), HabitOccurrenceError> {
        match self {
            Self::Partial { quantity } => quantity.validate_partial(),
            Self::Completed {
                quantity: Some(quantity),
            } => quantity.validate_completed(),
            Self::Pending | Self::Completed { quantity: None } | Self::Skipped { .. } => Ok(()),
        }
    }

    fn is_recorded(&self) -> bool {
        !matches!(self, Self::Pending)
    }

    fn adherence_basis_points(&self) -> u16 {
        match self {
            Self::Completed { .. } => HABIT_BASIS_POINTS_SCALE,
            Self::Partial { quantity } => quantity.basis_points(),
            Self::Pending | Self::Skipped { .. } => 0,
        }
    }
}

/// User-visible value of an occurrence at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrenceValue {
    pub outcome: HabitOccurrenceOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the outcome actually occurred. A later correction keeps this
    /// distinct from the command's recording time.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub effective_at: Option<OffsetDateTime>,
}

impl Default for HabitOccurrenceValue {
    fn default() -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Pending,
            note: None,
            effective_at: None,
        }
    }
}

impl HabitOccurrenceValue {
    /// Constructs an empty, not-yet-recorded occurrence value.
    #[must_use]
    pub fn pending() -> Self {
        Self::default()
    }

    /// Constructs a cumulative partial-quantity value.
    #[must_use]
    pub fn partial(
        quantity: HabitQuantityProgress,
        note: Option<String>,
        effective_at: OffsetDateTime,
    ) -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Partial { quantity },
            note,
            effective_at: Some(effective_at),
        }
    }

    /// Constructs a completed value, with optional quantitative evidence.
    #[must_use]
    pub fn completed(
        quantity: Option<HabitQuantityProgress>,
        note: Option<String>,
        effective_at: OffsetDateTime,
    ) -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Completed { quantity },
            note,
            effective_at: Some(effective_at),
        }
    }

    /// Constructs a skipped value.
    #[must_use]
    pub fn skipped(
        reason: HabitSkipReason,
        note: Option<String>,
        effective_at: OffsetDateTime,
    ) -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Skipped { reason },
            note,
            effective_at: Some(effective_at),
        }
    }

    fn validate(&self, recorded_at: OffsetDateTime) -> Result<(), HabitOccurrenceError> {
        self.outcome.validate()?;
        validate_note(self.note.as_deref())?;
        match (&self.outcome, self.effective_at) {
            (HabitOccurrenceOutcome::Pending, None) => Ok(()),
            (HabitOccurrenceOutcome::Pending, Some(_)) => {
                Err(HabitOccurrenceError::PendingHasEffectiveTime)
            }
            (_, Some(effective_at)) if effective_at <= recorded_at => Ok(()),
            (_, Some(_)) => Err(HabitOccurrenceError::EffectiveTimeInFuture),
            (_, None) => Err(HabitOccurrenceError::RecordedOutcomeMissingEffectiveTime),
        }
    }
}

/// Mutable projection of a stable generated habit occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrenceRecord {
    pub habit_id: ItemId,
    pub occurrence_id: OccurrenceId,
    #[serde(with = "date_serde")]
    pub local_date: Date,
    pub revision: u64,
    pub value: HabitOccurrenceValue,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl HabitOccurrenceRecord {
    /// Creates the first pending revision of a server-materialized occurrence.
    ///
    /// # Errors
    ///
    /// Returns an error for a nil habit or occurrence identifier.
    pub fn new(
        habit_id: ItemId,
        occurrence_id: OccurrenceId,
        local_date: Date,
        created_at: OffsetDateTime,
    ) -> Result<Self, HabitOccurrenceError> {
        if habit_id.0.is_nil() {
            return Err(HabitOccurrenceError::InvalidHabitId);
        }
        if occurrence_id.0.is_nil() {
            return Err(HabitOccurrenceError::InvalidOccurrenceId);
        }
        Ok(Self {
            habit_id,
            occurrence_id,
            local_date,
            revision: 1,
            value: HabitOccurrenceValue::pending(),
            created_at,
            updated_at: created_at,
        })
    }

    /// Validates a record loaded from storage before applying a command.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, revision, timestamps, or value.
    pub fn validate(&self) -> Result<(), HabitOccurrenceError> {
        if self.habit_id.0.is_nil() {
            return Err(HabitOccurrenceError::InvalidHabitId);
        }
        if self.occurrence_id.0.is_nil() {
            return Err(HabitOccurrenceError::InvalidOccurrenceId);
        }
        if self.revision == 0 {
            return Err(HabitOccurrenceError::InvalidRevision);
        }
        if self.updated_at < self.created_at {
            return Err(HabitOccurrenceError::TimestampRegression);
        }
        self.value.validate(self.updated_at)
    }
}

/// Whether a command is ordinary forward recording or an explicit correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HabitOccurrenceCommandKind {
    Record { value: HabitOccurrenceValue },
    Correct { value: HabitOccurrenceValue },
}

/// Fully explicit input to the deterministic occurrence command handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrenceCommand {
    pub expected_revision: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub kind: HabitOccurrenceCommandKind,
}

/// Audit-friendly classification of a successful lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitOccurrenceTransitionKind {
    Recorded,
    Progressed,
    Corrected,
    Reopened,
}

/// Successful transition with the complete inverse preimage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrenceTransition {
    pub kind: HabitOccurrenceTransitionKind,
    pub previous: HabitOccurrenceRecord,
    pub current: HabitOccurrenceRecord,
}

/// Applies one occurrence mutation without I/O or an implicit clock.
///
/// Ordinary recording is monotonic: pending occurrences can be recorded, and
/// cumulative partial progress can advance or complete. Reopening, decreasing
/// quantity, changing a target/unit, or changing a terminal result requires an
/// explicit correction. The returned preimage is suitable for audit and undo.
///
/// # Errors
///
/// Returns a validation, revision-conflict, timestamp, overflow, no-op, or
/// illegal-transition error.
pub fn apply_habit_occurrence_command(
    current: &HabitOccurrenceRecord,
    command: HabitOccurrenceCommand,
) -> Result<HabitOccurrenceTransition, HabitOccurrenceError> {
    current.validate()?;
    if command.expected_revision != current.revision {
        return Err(HabitOccurrenceError::RevisionConflict {
            expected: command.expected_revision,
            actual: current.revision,
        });
    }
    if command.recorded_at < current.updated_at {
        return Err(HabitOccurrenceError::TimestampRegression);
    }

    let (next_value, transition_kind) = match command.kind {
        HabitOccurrenceCommandKind::Record { value } => {
            value.validate(command.recorded_at)?;
            validate_forward_record(&current.value.outcome, &value.outcome)?;
            let kind = if matches!(current.value.outcome, HabitOccurrenceOutcome::Pending) {
                HabitOccurrenceTransitionKind::Recorded
            } else {
                HabitOccurrenceTransitionKind::Progressed
            };
            (value, kind)
        }
        HabitOccurrenceCommandKind::Correct { value } => {
            if !current.value.outcome.is_recorded() {
                return Err(HabitOccurrenceError::CorrectionRequiresRecordedOutcome);
            }
            value.validate(command.recorded_at)?;
            let kind = if matches!(value.outcome, HabitOccurrenceOutcome::Pending) {
                HabitOccurrenceTransitionKind::Reopened
            } else {
                HabitOccurrenceTransitionKind::Corrected
            };
            (value, kind)
        }
    };
    if next_value == current.value {
        return Err(HabitOccurrenceError::NoChange);
    }

    let mut next = current.clone();
    next.revision = current
        .revision
        .checked_add(1)
        .ok_or(HabitOccurrenceError::RevisionOverflow)?;
    next.value = next_value;
    next.updated_at = command.recorded_at;
    Ok(HabitOccurrenceTransition {
        kind: transition_kind,
        previous: current.clone(),
        current: next,
    })
}

fn validate_forward_record(
    current: &HabitOccurrenceOutcome,
    next: &HabitOccurrenceOutcome,
) -> Result<(), HabitOccurrenceError> {
    match (current, next) {
        (HabitOccurrenceOutcome::Pending, next) if next.is_recorded() => Ok(()),
        (
            HabitOccurrenceOutcome::Partial { quantity: previous },
            HabitOccurrenceOutcome::Partial { quantity: next }
            | HabitOccurrenceOutcome::Completed {
                quantity: Some(next),
            },
        ) => {
            if previous.unit != next.unit {
                return Err(HabitOccurrenceError::QuantityUnitChanged);
            }
            if previous.target_units != next.target_units {
                return Err(HabitOccurrenceError::QuantityTargetChanged);
            }
            if next.completed_units <= previous.completed_units {
                return Err(HabitOccurrenceError::QuantityDidNotAdvance);
            }
            Ok(())
        }
        (
            HabitOccurrenceOutcome::Partial { .. },
            HabitOccurrenceOutcome::Completed { quantity: None },
        ) => Err(HabitOccurrenceError::QuantityEvidenceRemoved),
        _ => Err(HabitOccurrenceError::InvalidForwardTransition),
    }
}

fn validate_note(note: Option<&str>) -> Result<(), HabitOccurrenceError> {
    if let Some(note) = note
        && (note.trim().is_empty()
            || note.len() > MAX_HABIT_OCCURRENCE_NOTE_BYTES
            || note.contains('\0'))
    {
        return Err(HabitOccurrenceError::InvalidNote);
    }
    Ok(())
}

/// Errors returned by occurrence lifecycle commands and value validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HabitOccurrenceError {
    #[error("habit identifier cannot be nil")]
    InvalidHabitId,
    #[error("occurrence identifier cannot be nil")]
    InvalidOccurrenceId,
    #[error("occurrence revision must be positive")]
    InvalidRevision,
    #[error("expected occurrence revision {expected}, actual revision {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("occurrence revision overflow")]
    RevisionOverflow,
    #[error("recording time cannot move backwards")]
    TimestampRegression,
    #[error("quantity and target must be positive signed-64-bit values")]
    InvalidQuantity,
    #[error("quantity unit must be trimmed, printable, non-empty, and bounded")]
    InvalidQuantityUnit,
    #[error("partial quantity must remain below its target")]
    InvalidPartialQuantity,
    #[error("completed quantitative progress must reach its target")]
    IncompleteCompletedQuantity,
    #[error("occurrence note must be non-empty, NUL-free, and bounded")]
    InvalidNote,
    #[error("a pending outcome cannot have an effective time")]
    PendingHasEffectiveTime,
    #[error("a recorded outcome requires an effective time")]
    RecordedOutcomeMissingEffectiveTime,
    #[error("effective time cannot be later than recording time")]
    EffectiveTimeInFuture,
    #[error("ordinary recording does not permit that transition")]
    InvalidForwardTransition,
    #[error("ordinary progress cannot change its quantity unit")]
    QuantityUnitChanged,
    #[error("ordinary progress cannot change its quantity target")]
    QuantityTargetChanged,
    #[error("ordinary cumulative quantity must increase")]
    QuantityDidNotAdvance,
    #[error("ordinary completion cannot discard partial quantity evidence")]
    QuantityEvidenceRemoved,
    #[error("a correction requires an already-recorded outcome")]
    CorrectionRequiresRecordedOutcome,
    #[error("command does not change the occurrence")]
    NoChange,
}

/// Half-open pause interval. An absent end represents an indefinite pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitPauseInterval {
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub end: Option<OffsetDateTime>,
}

impl HabitPauseInterval {
    fn validate(self) -> Result<(), HabitPolicyError> {
        if self.end.is_some_and(|end| end <= self.start) {
            return Err(HabitPolicyError::InvalidPauseInterval);
        }
        Ok(())
    }

    fn overlaps(self, start: OffsetDateTime, end: OffsetDateTime) -> bool {
        self.start < end && self.end.is_none_or(|pause_end| pause_end > start)
    }
}

/// Analytics treatment of an occurrence whose resolved window is inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitOccurrenceEligibility {
    Eligible,
    PausedProtected,
    PausedUnprotected,
}

/// Resolves pause overlap and whether it is excluded from adherence/streaks.
///
/// # Errors
///
/// Returns an error for an empty occurrence window or malformed pause interval.
pub fn habit_occurrence_eligibility(
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
    pauses: &[HabitPauseInterval],
    preserves_statistics_when_paused: bool,
) -> Result<HabitOccurrenceEligibility, HabitPolicyError> {
    validate_window(window_start, window_end)?;
    let mut overlaps_pause = false;
    for pause in pauses {
        pause.validate()?;
        overlaps_pause |= pause.overlaps(window_start, window_end);
    }
    Ok(match (overlaps_pause, preserves_statistics_when_paused) {
        (false, _) => HabitOccurrenceEligibility::Eligible,
        (true, true) => HabitOccurrenceEligibility::PausedProtected,
        (true, false) => HabitOccurrenceEligibility::PausedUnprotected,
    })
}

/// Configured action for an unmet occurrence after its window closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitMissedPolicy {
    Skip,
    Carry,
    ReduceFrequency,
    Ask,
}

/// Reason that a missed-policy evaluation intentionally changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitMissedNoActionReason {
    WindowOpen,
    Paused,
    AlreadyCompleted,
    AlreadySkipped,
}

/// Pure decision emitted for an overdue unmet occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HabitMissedDecision {
    NoAction { reason: HabitMissedNoActionReason },
    MarkSkipped,
    CarryForward,
    ReduceFrequency,
    RequestDecision,
}

/// Evaluates configured missed behavior without mutating the occurrence.
///
/// A pause suppresses missed handling regardless of its analytics policy.
/// Partial progress is still unmet and therefore follows the configured policy
/// after the window closes; its quantity remains available to a carry command.
///
/// # Errors
///
/// Returns an error for an invalid outcome, occurrence window, or pause.
pub fn decide_habit_missed_behavior(
    policy: HabitMissedPolicy,
    as_of: OffsetDateTime,
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
    outcome: &HabitOccurrenceOutcome,
    pauses: &[HabitPauseInterval],
) -> Result<HabitMissedDecision, HabitPolicyError> {
    validate_window(window_start, window_end)?;
    outcome
        .validate()
        .map_err(HabitPolicyError::InvalidOutcome)?;
    for pause in pauses {
        pause.validate()?;
    }
    match outcome {
        HabitOccurrenceOutcome::Completed { .. } => {
            return Ok(HabitMissedDecision::NoAction {
                reason: HabitMissedNoActionReason::AlreadyCompleted,
            });
        }
        HabitOccurrenceOutcome::Skipped { .. } => {
            return Ok(HabitMissedDecision::NoAction {
                reason: HabitMissedNoActionReason::AlreadySkipped,
            });
        }
        HabitOccurrenceOutcome::Pending | HabitOccurrenceOutcome::Partial { .. } => {}
    }
    if pauses
        .iter()
        .any(|pause| pause.overlaps(window_start, window_end))
    {
        return Ok(HabitMissedDecision::NoAction {
            reason: HabitMissedNoActionReason::Paused,
        });
    }
    if as_of < window_end {
        return Ok(HabitMissedDecision::NoAction {
            reason: HabitMissedNoActionReason::WindowOpen,
        });
    }
    Ok(match policy {
        HabitMissedPolicy::Skip => HabitMissedDecision::MarkSkipped,
        HabitMissedPolicy::Carry => HabitMissedDecision::CarryForward,
        HabitMissedPolicy::ReduceFrequency => HabitMissedDecision::ReduceFrequency,
        HabitMissedPolicy::Ask => HabitMissedDecision::RequestDecision,
    })
}

fn validate_window(
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
) -> Result<(), HabitPolicyError> {
    if window_start >= window_end {
        Err(HabitPolicyError::InvalidOccurrenceWindow)
    } else {
        Ok(())
    }
}

/// Errors from pause and missed-policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HabitPolicyError {
    #[error("occurrence window must have positive duration")]
    InvalidOccurrenceWindow,
    #[error("pause interval end must be later than its start")]
    InvalidPauseInterval,
    #[error("invalid occurrence outcome: {0}")]
    InvalidOutcome(HabitOccurrenceError),
}

/// One expected occurrence supplied to the analytics projector.
///
/// The caller resolves `local_date` using the habit's IANA timezone and supplies
/// its exact window separately. This avoids treating a 23- or 25-hour DST day as
/// a fixed UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitAnalyticsOccurrence {
    pub occurrence_id: OccurrenceId,
    #[serde(with = "date_serde")]
    pub local_date: Date,
    #[serde(with = "time::serde::rfc3339")]
    pub window_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub window_end: OffsetDateTime,
    pub outcome: HabitOccurrenceOutcome,
}

/// Calendar unit used for deterministic trend buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitTrendGranularity {
    Day,
    Week,
    Month,
}

/// Complete explicit input for one analytics projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitAnalyticsInput {
    #[serde(with = "date_serde")]
    pub range_start: Date,
    #[serde(with = "date_serde")]
    pub range_end: Date,
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    #[serde(with = "date_serde")]
    pub as_of_local_date: Date,
    pub trend_granularity: HabitTrendGranularity,
    pub week_starts_on: DayOfWeek,
    pub preserves_statistics_when_paused: bool,
    #[serde(default)]
    pub pauses: Vec<HabitPauseInterval>,
    #[serde(default)]
    pub occurrences: Vec<HabitAnalyticsOccurrence>,
}

/// Integer occurrence counts for an analytics range or trend bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitAnalyticsCounts {
    /// Expected occurrences whose window has closed.
    pub due: u32,
    /// Due occurrences included in the adherence denominator.
    pub eligible: u32,
    /// Due occurrences overlapping any pause, whether protected or not.
    pub paused: u32,
    /// Paused occurrences removed from adherence and streak calculations.
    pub protected_paused: u32,
    /// Outcome counts below are over eligible occurrences only.
    pub completed: u32,
    pub partial: u32,
    pub skipped: u32,
    pub pending: u32,
    /// Subset of `skipped` produced by the configured missed policy.
    pub missed_policy_skips: u32,
}

/// One continuous local-calendar trend bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitTrendBucket {
    #[serde(with = "date_serde")]
    pub start_date: Date,
    pub counts: HabitAnalyticsCounts,
    /// `None` means the bucket has no eligible due occurrence.
    pub adherence_basis_points: Option<u16>,
}

/// Stable facts that clients can render with supportive localized copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitSupportiveFactCode {
    NoDueOccurrences,
    PausedOccurrencesProtected,
    PartialProgressRecorded,
    FullAdherence,
    CurrentStreak,
    PersonalBest,
    ImprovingTrend,
    NextOccurrenceOpportunity,
}

/// A supportive fact and its optional integer value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitSupportiveFact {
    pub code: HabitSupportiveFactCode,
    pub value: Option<u32>,
}

/// Deterministic analytics projection for a habit and local-date range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitAnalytics {
    pub counts: HabitAnalyticsCounts,
    /// Equal-weight occurrence adherence. Completed occurrences contribute
    /// 10,000; partial occurrences contribute their quantity fraction; pending
    /// and skipped occurrences contribute zero. Division rounds down.
    pub adherence_basis_points: Option<u16>,
    /// Consecutive successful eligible habit dates at the end of the range.
    pub current_streak: u32,
    /// Longest run of successful eligible habit dates in the range.
    pub longest_streak: u32,
    pub trend_buckets: Vec<HabitTrendBucket>,
    /// Stable, non-punitive fact codes in deterministic display priority.
    pub supportive_facts: Vec<HabitSupportiveFact>,
}

/// Projects counts, adherence, streaks, continuous calendar trends, and facts.
///
/// A successful streak date requires every eligible due occurrence on that
/// date to be completed. Calendar dates with no eligible due occurrence do not
/// extend or break a streak, which handles weekday-only habits and protected
/// pauses without pretending that skipped calendar days were failures.
///
/// # Errors
///
/// Returns an error for malformed ranges, duplicate identities, invalid windows,
/// invalid outcomes or pauses, arithmetic overflow, or calendar overflow.
pub fn calculate_habit_analytics(
    input: &HabitAnalyticsInput,
) -> Result<HabitAnalytics, HabitAnalyticsError> {
    validate_analytics_input(input)?;
    let effective_end = input.range_end.min(input.as_of_local_date);
    let mut trend = if effective_end < input.range_start {
        BTreeMap::new()
    } else {
        empty_trend_buckets(
            input.range_start,
            effective_end,
            input.trend_granularity,
            input.week_starts_on,
        )?
    };
    let mut total = MutableTally::default();
    let mut streak_dates = BTreeMap::<Date, bool>::new();

    for occurrence in &input.occurrences {
        if occurrence.local_date < input.range_start
            || occurrence.local_date > effective_end
            || occurrence.window_end > input.as_of
        {
            continue;
        }
        let eligibility = habit_occurrence_eligibility(
            occurrence.window_start,
            occurrence.window_end,
            &input.pauses,
            input.preserves_statistics_when_paused,
        )
        .map_err(HabitAnalyticsError::Policy)?;
        total.observe(&occurrence.outcome, eligibility)?;
        let bucket_start = calendar_bucket_start(
            occurrence.local_date,
            input.trend_granularity,
            input.week_starts_on,
        )?;
        trend
            .get_mut(&bucket_start)
            .ok_or(HabitAnalyticsError::CalendarOverflow)?
            .observe(&occurrence.outcome, eligibility)?;
        if eligibility != HabitOccurrenceEligibility::PausedProtected {
            let successful = matches!(occurrence.outcome, HabitOccurrenceOutcome::Completed { .. });
            streak_dates
                .entry(occurrence.local_date)
                .and_modify(|all_successful| *all_successful &= successful)
                .or_insert(successful);
        }
    }

    let (current_streak, longest_streak) = streaks(&streak_dates)?;
    let adherence_basis_points = total.adherence_basis_points()?;
    let counts = total.counts;
    let trend_buckets = trend
        .into_iter()
        .map(|(start_date, tally)| {
            Ok(HabitTrendBucket {
                start_date,
                counts: tally.counts,
                adherence_basis_points: tally.adherence_basis_points()?,
            })
        })
        .collect::<Result<Vec<_>, HabitAnalyticsError>>()?;
    let supportive_facts = supportive_facts(
        counts,
        adherence_basis_points,
        current_streak,
        longest_streak,
        &trend_buckets,
    );
    Ok(HabitAnalytics {
        counts,
        adherence_basis_points,
        current_streak,
        longest_streak,
        trend_buckets,
        supportive_facts,
    })
}

fn validate_analytics_input(input: &HabitAnalyticsInput) -> Result<(), HabitAnalyticsError> {
    if input.range_end < input.range_start {
        return Err(HabitAnalyticsError::InvalidRange);
    }
    let inclusive_days = i64::from(input.range_end.to_julian_day())
        - i64::from(input.range_start.to_julian_day())
        + 1;
    if inclusive_days > MAX_HABIT_ANALYTICS_RANGE_DAYS {
        return Err(HabitAnalyticsError::RangeTooLarge);
    }
    if input.occurrences.len() > MAX_HABIT_ANALYTICS_OCCURRENCES {
        return Err(HabitAnalyticsError::TooManyOccurrences);
    }
    for pause in &input.pauses {
        pause.validate().map_err(HabitAnalyticsError::Policy)?;
    }
    let mut identities = BTreeSet::new();
    for occurrence in &input.occurrences {
        if occurrence.occurrence_id.0.is_nil() {
            return Err(HabitAnalyticsError::InvalidOccurrenceId);
        }
        if !identities.insert(occurrence.occurrence_id) {
            return Err(HabitAnalyticsError::DuplicateOccurrence(
                occurrence.occurrence_id,
            ));
        }
        validate_window(occurrence.window_start, occurrence.window_end)
            .map_err(HabitAnalyticsError::Policy)?;
        occurrence
            .outcome
            .validate()
            .map_err(HabitAnalyticsError::InvalidOutcome)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct MutableTally {
    counts: HabitAnalyticsCounts,
    adherence_sum: u64,
}

impl MutableTally {
    fn observe(
        &mut self,
        outcome: &HabitOccurrenceOutcome,
        eligibility: HabitOccurrenceEligibility,
    ) -> Result<(), HabitAnalyticsError> {
        increment(&mut self.counts.due)?;
        if eligibility != HabitOccurrenceEligibility::Eligible {
            increment(&mut self.counts.paused)?;
        }
        if eligibility == HabitOccurrenceEligibility::PausedProtected {
            increment(&mut self.counts.protected_paused)?;
            return Ok(());
        }
        increment(&mut self.counts.eligible)?;
        match outcome {
            HabitOccurrenceOutcome::Pending => increment(&mut self.counts.pending)?,
            HabitOccurrenceOutcome::Partial { .. } => increment(&mut self.counts.partial)?,
            HabitOccurrenceOutcome::Completed { .. } => increment(&mut self.counts.completed)?,
            HabitOccurrenceOutcome::Skipped { reason } => {
                increment(&mut self.counts.skipped)?;
                if *reason == HabitSkipReason::MissedPolicy {
                    increment(&mut self.counts.missed_policy_skips)?;
                }
            }
        }
        self.adherence_sum = self
            .adherence_sum
            .checked_add(u64::from(outcome.adherence_basis_points()))
            .ok_or(HabitAnalyticsError::ArithmeticOverflow)?;
        Ok(())
    }

    fn adherence_basis_points(self) -> Result<Option<u16>, HabitAnalyticsError> {
        if self.counts.eligible == 0 {
            return Ok(None);
        }
        let value = self.adherence_sum / u64::from(self.counts.eligible);
        u16::try_from(value)
            .map(Some)
            .map_err(|_| HabitAnalyticsError::ArithmeticOverflow)
    }
}

fn increment(value: &mut u32) -> Result<(), HabitAnalyticsError> {
    *value = value
        .checked_add(1)
        .ok_or(HabitAnalyticsError::ArithmeticOverflow)?;
    Ok(())
}

fn streaks(dates: &BTreeMap<Date, bool>) -> Result<(u32, u32), HabitAnalyticsError> {
    let mut current = 0_u32;
    let mut longest = 0_u32;
    for successful in dates.values() {
        if *successful {
            current = current
                .checked_add(1)
                .ok_or(HabitAnalyticsError::ArithmeticOverflow)?;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    Ok((current, longest))
}

fn supportive_facts(
    counts: HabitAnalyticsCounts,
    adherence_basis_points: Option<u16>,
    current_streak: u32,
    longest_streak: u32,
    trend: &[HabitTrendBucket],
) -> Vec<HabitSupportiveFact> {
    let mut facts = Vec::new();
    if counts.eligible == 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::NoDueOccurrences,
            value: None,
        });
    }
    if counts.protected_paused > 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::PausedOccurrencesProtected,
            value: Some(counts.protected_paused),
        });
    }
    if counts.partial > 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::PartialProgressRecorded,
            value: Some(counts.partial),
        });
    }
    if adherence_basis_points == Some(HABIT_BASIS_POINTS_SCALE) {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::FullAdherence,
            value: None,
        });
    }
    if current_streak > 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::CurrentStreak,
            value: Some(current_streak),
        });
    }
    if longest_streak > current_streak && longest_streak > 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::PersonalBest,
            value: Some(longest_streak),
        });
    }
    let mut nonempty = trend
        .iter()
        .filter_map(|bucket| bucket.adherence_basis_points);
    let mut previous = nonempty.next();
    let mut latest_pair = None;
    for value in nonempty {
        latest_pair = previous.map(|prior| (prior, value));
        previous = Some(value);
    }
    if latest_pair.is_some_and(|(prior, latest)| latest > prior) {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::ImprovingTrend,
            value: None,
        });
    }
    if counts.eligible > 0 && current_streak == 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::NextOccurrenceOpportunity,
            value: None,
        });
    }
    facts
}

fn empty_trend_buckets(
    range_start: Date,
    range_end: Date,
    granularity: HabitTrendGranularity,
    week_starts_on: DayOfWeek,
) -> Result<BTreeMap<Date, MutableTally>, HabitAnalyticsError> {
    let first = calendar_bucket_start(range_start, granularity, week_starts_on)?;
    let last = calendar_bucket_start(range_end, granularity, week_starts_on)?;
    let mut result = BTreeMap::new();
    let mut cursor = first;
    loop {
        result.insert(cursor, MutableTally::default());
        if cursor == last {
            return Ok(result);
        }
        cursor = next_bucket_start(cursor, granularity)?;
        if cursor > last {
            return Err(HabitAnalyticsError::CalendarOverflow);
        }
    }
}

fn calendar_bucket_start(
    date: Date,
    granularity: HabitTrendGranularity,
    week_starts_on: DayOfWeek,
) -> Result<Date, HabitAnalyticsError> {
    match granularity {
        HabitTrendGranularity::Day => Ok(date),
        HabitTrendGranularity::Week => {
            let current = weekday_index_time(date.weekday());
            let first = weekday_index(week_starts_on);
            let offset = i64::from((current + 7 - first) % 7);
            date.checked_sub(Duration::days(offset))
                .ok_or(HabitAnalyticsError::CalendarOverflow)
        }
        HabitTrendGranularity::Month => Date::from_calendar_date(date.year(), date.month(), 1)
            .map_err(|_| HabitAnalyticsError::CalendarOverflow),
    }
}

fn next_bucket_start(
    start: Date,
    granularity: HabitTrendGranularity,
) -> Result<Date, HabitAnalyticsError> {
    match granularity {
        HabitTrendGranularity::Day => start
            .next_day()
            .ok_or(HabitAnalyticsError::CalendarOverflow),
        HabitTrendGranularity::Week => start
            .checked_add(Duration::days(7))
            .ok_or(HabitAnalyticsError::CalendarOverflow),
        HabitTrendGranularity::Month => {
            let (year, month) = if start.month() == time::Month::December {
                (
                    start
                        .year()
                        .checked_add(1)
                        .ok_or(HabitAnalyticsError::CalendarOverflow)?,
                    time::Month::January,
                )
            } else {
                (start.year(), start.month().next())
            };
            Date::from_calendar_date(year, month, 1)
                .map_err(|_| HabitAnalyticsError::CalendarOverflow)
        }
    }
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

const fn weekday_index_time(day: time::Weekday) -> u8 {
    match day {
        time::Weekday::Monday => 0,
        time::Weekday::Tuesday => 1,
        time::Weekday::Wednesday => 2,
        time::Weekday::Thursday => 3,
        time::Weekday::Friday => 4,
        time::Weekday::Saturday => 5,
        time::Weekday::Sunday => 6,
    }
}

/// Analytics projection errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HabitAnalyticsError {
    #[error("analytics range end cannot precede its start")]
    InvalidRange,
    #[error("analytics range exceeds {MAX_HABIT_ANALYTICS_RANGE_DAYS} days")]
    RangeTooLarge,
    #[error("analytics occurrence count exceeds {MAX_HABIT_ANALYTICS_OCCURRENCES}")]
    TooManyOccurrences,
    #[error("analytics occurrence identifier cannot be nil")]
    InvalidOccurrenceId,
    #[error("duplicate analytics occurrence {0}")]
    DuplicateOccurrence(OccurrenceId),
    #[error("invalid analytics outcome: {0}")]
    InvalidOutcome(HabitOccurrenceError),
    #[error("invalid analytics policy input: {0}")]
    Policy(HabitPolicyError),
    #[error("analytics arithmetic overflow")]
    ArithmeticOverflow,
    #[error("analytics calendar calculation overflow")]
    CalendarOverflow,
}

mod date_serde {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    use time::{Date, macros::format_description};

    const FORMAT: &[time::format_description::FormatItem<'_>] =
        format_description!("[year]-[month]-[day]");

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S>(date: &Date, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&date.format(FORMAT).map_err(serde::ser::Error::custom)?)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Date, D::Error>
    where
        D: Deserializer<'de>,
    {
        Date::parse(&String::deserialize(deserializer)?, FORMAT).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use time::macros::{date, datetime};
    use uuid::Uuid;

    use super::*;

    const CREATED: OffsetDateTime = datetime!(2026-03-01 8:00 UTC);
    const HABIT_ID: ItemId = ItemId(Uuid::from_u128(1));
    const OCCURRENCE_ID: OccurrenceId = OccurrenceId(Uuid::from_u128(2));

    fn quantity(completed_units: u64, target_units: u64) -> HabitQuantityProgress {
        HabitQuantityProgress {
            completed_units,
            target_units,
            unit: "glasses".to_owned(),
        }
    }

    fn record() -> HabitOccurrenceRecord {
        HabitOccurrenceRecord::new(HABIT_ID, OCCURRENCE_ID, date!(2026 - 03 - 01), CREATED).unwrap()
    }

    fn command(
        current: &HabitOccurrenceRecord,
        at: OffsetDateTime,
        kind: HabitOccurrenceCommandKind,
    ) -> HabitOccurrenceCommand {
        HabitOccurrenceCommand {
            expected_revision: current.revision,
            recorded_at: at,
            kind,
        }
    }

    fn analytics_occurrence(
        id: u128,
        local_date: Date,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        outcome: HabitOccurrenceOutcome,
    ) -> HabitAnalyticsOccurrence {
        HabitAnalyticsOccurrence {
            occurrence_id: OccurrenceId(Uuid::from_u128(id)),
            local_date,
            window_start,
            window_end,
            outcome,
        }
    }

    #[test]
    fn ordinary_partial_progress_is_monotonic_and_preserves_its_preimage() {
        let pending = record();
        let first = apply_habit_occurrence_command(
            &pending,
            command(
                &pending,
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        quantity(2, 8),
                        Some("Morning".to_owned()),
                        datetime!(2026-03-01 8:55 UTC),
                    ),
                },
            ),
        )
        .unwrap();
        assert_eq!(first.kind, HabitOccurrenceTransitionKind::Recorded);
        assert_eq!(first.previous, pending);
        assert_eq!(first.current.revision, 2);

        let second = apply_habit_occurrence_command(
            &first.current,
            command(
                &first.current,
                datetime!(2026-03-01 12:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        quantity(5, 8),
                        Some("Lunch".to_owned()),
                        datetime!(2026-03-01 11:55 UTC),
                    ),
                },
            ),
        )
        .unwrap();
        assert_eq!(second.kind, HabitOccurrenceTransitionKind::Progressed);

        let completed = apply_habit_occurrence_command(
            &second.current,
            command(
                &second.current,
                datetime!(2026-03-01 20:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::completed(
                        Some(quantity(8, 8)),
                        None,
                        datetime!(2026-03-01 19:58 UTC),
                    ),
                },
            ),
        )
        .unwrap();
        assert_eq!(completed.kind, HabitOccurrenceTransitionKind::Progressed);
        assert!(matches!(
            completed.current.value.outcome,
            HabitOccurrenceOutcome::Completed { .. }
        ));
    }

    #[test]
    fn revision_conflicts_and_nonadvancing_partial_progress_fail_closed() {
        let pending = record();
        let stale = HabitOccurrenceCommand {
            expected_revision: 9,
            recorded_at: datetime!(2026-03-01 9:00 UTC),
            kind: HabitOccurrenceCommandKind::Record {
                value: HabitOccurrenceValue::completed(None, None, datetime!(2026-03-01 9:00 UTC)),
            },
        };
        assert_eq!(
            apply_habit_occurrence_command(&pending, stale),
            Err(HabitOccurrenceError::RevisionConflict {
                expected: 9,
                actual: 1,
            })
        );

        let partial = apply_habit_occurrence_command(
            &pending,
            command(
                &pending,
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        quantity(2, 8),
                        None,
                        datetime!(2026-03-01 9:00 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;
        let result = apply_habit_occurrence_command(
            &partial,
            command(
                &partial,
                datetime!(2026-03-01 10:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        quantity(2, 8),
                        None,
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        );
        assert_eq!(result, Err(HabitOccurrenceError::QuantityDidNotAdvance));
    }

    #[test]
    fn partial_and_completed_quantity_invariants_are_strict() {
        assert_eq!(
            HabitOccurrenceOutcome::Partial {
                quantity: quantity(8, 8)
            }
            .validate(),
            Err(HabitOccurrenceError::InvalidPartialQuantity)
        );
        assert_eq!(
            HabitOccurrenceOutcome::Completed {
                quantity: Some(quantity(7, 8))
            }
            .validate(),
            Err(HabitOccurrenceError::IncompleteCompletedQuantity)
        );
        assert_eq!(
            HabitOccurrenceOutcome::Partial {
                quantity: quantity(0, 8)
            }
            .validate(),
            Err(HabitOccurrenceError::InvalidQuantity)
        );
    }

    #[test]
    fn explicit_correction_can_change_terminal_state_and_reopen() {
        let pending = record();
        let completed = apply_habit_occurrence_command(
            &pending,
            command(
                &pending,
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::completed(
                        None,
                        Some("Initially done".to_owned()),
                        datetime!(2026-03-01 8:50 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;
        let corrected = apply_habit_occurrence_command(
            &completed,
            command(
                &completed,
                datetime!(2026-03-02 9:00 UTC),
                HabitOccurrenceCommandKind::Correct {
                    value: HabitOccurrenceValue::skipped(
                        HabitSkipReason::User,
                        Some("Corrected the wrong day".to_owned()),
                        datetime!(2026-03-01 8:50 UTC),
                    ),
                },
            ),
        )
        .unwrap();
        assert_eq!(corrected.kind, HabitOccurrenceTransitionKind::Corrected);
        assert_eq!(corrected.previous, completed);

        let reopened = apply_habit_occurrence_command(
            &corrected.current,
            command(
                &corrected.current,
                datetime!(2026-03-02 10:00 UTC),
                HabitOccurrenceCommandKind::Correct {
                    value: HabitOccurrenceValue::pending(),
                },
            ),
        )
        .unwrap();
        assert_eq!(reopened.kind, HabitOccurrenceTransitionKind::Reopened);
        assert_eq!(reopened.current.value, HabitOccurrenceValue::pending());
    }

    #[test]
    fn ordinary_commands_cannot_rewrite_terminal_results_or_remove_quantity() {
        let pending = record();
        let partial = apply_habit_occurrence_command(
            &pending,
            command(
                &pending,
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        quantity(2, 8),
                        None,
                        datetime!(2026-03-01 9:00 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;
        let removes_quantity = apply_habit_occurrence_command(
            &partial,
            command(
                &partial,
                datetime!(2026-03-01 10:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::completed(
                        None,
                        None,
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        );
        assert_eq!(
            removes_quantity,
            Err(HabitOccurrenceError::QuantityEvidenceRemoved)
        );
    }

    #[test]
    fn pause_eligibility_uses_half_open_windows_and_configured_protection() {
        let pause = HabitPauseInterval {
            start: datetime!(2026-03-01 10:00 UTC),
            end: Some(datetime!(2026-03-01 12:00 UTC)),
        };
        assert_eq!(
            habit_occurrence_eligibility(
                datetime!(2026-03-01 9:00 UTC),
                datetime!(2026-03-01 10:00 UTC),
                &[pause],
                true,
            ),
            Ok(HabitOccurrenceEligibility::Eligible)
        );
        assert_eq!(
            habit_occurrence_eligibility(
                datetime!(2026-03-01 9:30 UTC),
                datetime!(2026-03-01 10:30 UTC),
                &[pause],
                true,
            ),
            Ok(HabitOccurrenceEligibility::PausedProtected)
        );
        assert_eq!(
            habit_occurrence_eligibility(
                datetime!(2026-03-01 9:30 UTC),
                datetime!(2026-03-01 10:30 UTC),
                &[pause],
                false,
            ),
            Ok(HabitOccurrenceEligibility::PausedUnprotected)
        );
    }

    #[test]
    fn every_missed_policy_emits_a_distinct_deterministic_decision() {
        let start = datetime!(2026-03-01 8:00 UTC);
        let end = datetime!(2026-03-01 9:00 UTC);
        let as_of = datetime!(2026-03-01 10:00 UTC);
        let pending = HabitOccurrenceOutcome::Pending;
        let cases = [
            (HabitMissedPolicy::Skip, HabitMissedDecision::MarkSkipped),
            (HabitMissedPolicy::Carry, HabitMissedDecision::CarryForward),
            (
                HabitMissedPolicy::ReduceFrequency,
                HabitMissedDecision::ReduceFrequency,
            ),
            (HabitMissedPolicy::Ask, HabitMissedDecision::RequestDecision),
        ];
        for (policy, expected) in cases {
            assert_eq!(
                decide_habit_missed_behavior(policy, as_of, start, end, &pending, &[]),
                Ok(expected)
            );
        }

        let paused = [HabitPauseInterval {
            start,
            end: Some(end),
        }];
        assert_eq!(
            decide_habit_missed_behavior(
                HabitMissedPolicy::Skip,
                as_of,
                start,
                end,
                &pending,
                &paused,
            ),
            Ok(HabitMissedDecision::NoAction {
                reason: HabitMissedNoActionReason::Paused,
            })
        );
        assert_eq!(
            decide_habit_missed_behavior(
                HabitMissedPolicy::Skip,
                datetime!(2026-03-01 8:59 UTC),
                start,
                end,
                &pending,
                &[],
            ),
            Ok(HabitMissedDecision::NoAction {
                reason: HabitMissedNoActionReason::WindowOpen,
            })
        );
    }

    #[test]
    fn integer_adherence_counts_partial_quantity_without_floats() {
        let input = HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 05),
            as_of: datetime!(2026-03-06 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 06),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses: Vec::new(),
            occurrences: vec![
                analytics_occurrence(
                    10,
                    date!(2026 - 03 - 01),
                    datetime!(2026-03-01 8:00 UTC),
                    datetime!(2026-03-01 9:00 UTC),
                    HabitOccurrenceOutcome::Completed { quantity: None },
                ),
                analytics_occurrence(
                    11,
                    date!(2026 - 03 - 02),
                    datetime!(2026-03-02 8:00 UTC),
                    datetime!(2026-03-02 9:00 UTC),
                    HabitOccurrenceOutcome::Completed { quantity: None },
                ),
                analytics_occurrence(
                    12,
                    date!(2026 - 03 - 03),
                    datetime!(2026-03-03 8:00 UTC),
                    datetime!(2026-03-03 9:00 UTC),
                    HabitOccurrenceOutcome::Partial {
                        quantity: quantity(1, 2),
                    },
                ),
                analytics_occurrence(
                    13,
                    date!(2026 - 03 - 04),
                    datetime!(2026-03-04 8:00 UTC),
                    datetime!(2026-03-04 9:00 UTC),
                    HabitOccurrenceOutcome::Skipped {
                        reason: HabitSkipReason::MissedPolicy,
                    },
                ),
                analytics_occurrence(
                    14,
                    date!(2026 - 03 - 05),
                    datetime!(2026-03-05 8:00 UTC),
                    datetime!(2026-03-05 9:00 UTC),
                    HabitOccurrenceOutcome::Pending,
                ),
            ],
        };
        let analytics = calculate_habit_analytics(&input).unwrap();
        assert_eq!(analytics.counts.due, 5);
        assert_eq!(analytics.counts.eligible, 5);
        assert_eq!(analytics.counts.completed, 2);
        assert_eq!(analytics.counts.partial, 1);
        assert_eq!(analytics.counts.skipped, 1);
        assert_eq!(analytics.counts.missed_policy_skips, 1);
        assert_eq!(analytics.counts.pending, 1);
        assert_eq!(analytics.adherence_basis_points, Some(5_000));
        assert_eq!(analytics.current_streak, 0);
        assert_eq!(analytics.longest_streak, 2);
    }

    #[test]
    fn protected_pauses_do_not_break_streak_or_reduce_adherence() {
        let pauses = vec![HabitPauseInterval {
            start: datetime!(2026-03-02 0:00 UTC),
            end: Some(datetime!(2026-03-03 0:00 UTC)),
        }];
        let base = HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 03),
            as_of: datetime!(2026-03-04 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 04),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses,
            occurrences: vec![
                analytics_occurrence(
                    20,
                    date!(2026 - 03 - 01),
                    datetime!(2026-03-01 8:00 UTC),
                    datetime!(2026-03-01 9:00 UTC),
                    HabitOccurrenceOutcome::Completed { quantity: None },
                ),
                analytics_occurrence(
                    21,
                    date!(2026 - 03 - 02),
                    datetime!(2026-03-02 8:00 UTC),
                    datetime!(2026-03-02 9:00 UTC),
                    HabitOccurrenceOutcome::Pending,
                ),
                analytics_occurrence(
                    22,
                    date!(2026 - 03 - 03),
                    datetime!(2026-03-03 8:00 UTC),
                    datetime!(2026-03-03 9:00 UTC),
                    HabitOccurrenceOutcome::Completed { quantity: None },
                ),
            ],
        };
        let protected = calculate_habit_analytics(&base).unwrap();
        assert_eq!(protected.counts.due, 3);
        assert_eq!(protected.counts.eligible, 2);
        assert_eq!(protected.counts.protected_paused, 1);
        assert_eq!(protected.adherence_basis_points, Some(10_000));
        assert_eq!(protected.current_streak, 2);
        assert_eq!(protected.longest_streak, 2);

        let unprotected = calculate_habit_analytics(&HabitAnalyticsInput {
            preserves_statistics_when_paused: false,
            ..base
        })
        .unwrap();
        assert_eq!(unprotected.counts.eligible, 3);
        assert_eq!(unprotected.counts.protected_paused, 0);
        assert_eq!(unprotected.adherence_basis_points, Some(6_666));
        assert_eq!(unprotected.current_streak, 1);
        assert_eq!(unprotected.longest_streak, 1);
    }

    #[test]
    fn supplied_local_dates_keep_dst_days_in_their_intended_calendar_bucket() {
        let input = HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 29),
            range_end: date!(2026 - 03 - 30),
            as_of: datetime!(2026-03-31 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 31),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses: Vec::new(),
            occurrences: vec![
                // Europe/Paris spring-forward day: two local midnights are only
                // 23 elapsed hours apart. Bucketing uses the supplied date.
                analytics_occurrence(
                    30,
                    date!(2026 - 03 - 29),
                    datetime!(2026-03-28 23:00 UTC),
                    datetime!(2026-03-29 22:00 UTC),
                    HabitOccurrenceOutcome::Completed { quantity: None },
                ),
                analytics_occurrence(
                    31,
                    date!(2026 - 03 - 30),
                    datetime!(2026-03-29 22:00 UTC),
                    datetime!(2026-03-30 22:00 UTC),
                    HabitOccurrenceOutcome::Completed { quantity: None },
                ),
            ],
        };
        let analytics = calculate_habit_analytics(&input).unwrap();
        assert_eq!(analytics.trend_buckets.len(), 2);
        assert_eq!(analytics.trend_buckets[0].start_date, date!(2026 - 03 - 29));
        assert_eq!(analytics.trend_buckets[0].counts.completed, 1);
        assert_eq!(analytics.trend_buckets[1].start_date, date!(2026 - 03 - 30));
    }

    #[test]
    fn analytics_uses_corrected_projection_and_continuous_calendar_buckets() {
        let pending = record();
        let recorded = apply_habit_occurrence_command(
            &pending,
            command(
                &pending,
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::skipped(
                        HabitSkipReason::User,
                        None,
                        datetime!(2026-03-01 9:00 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;
        let corrected = apply_habit_occurrence_command(
            &recorded,
            command(
                &recorded,
                datetime!(2026-03-02 9:00 UTC),
                HabitOccurrenceCommandKind::Correct {
                    value: HabitOccurrenceValue::completed(
                        None,
                        Some("Found the correct log".to_owned()),
                        datetime!(2026-03-01 9:00 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;
        let input = HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 03),
            as_of: datetime!(2026-03-04 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 04),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses: Vec::new(),
            occurrences: vec![analytics_occurrence(
                40,
                corrected.local_date,
                datetime!(2026-03-01 8:00 UTC),
                datetime!(2026-03-01 9:00 UTC),
                corrected.value.outcome,
            )],
        };
        let analytics = calculate_habit_analytics(&input).unwrap();
        assert_eq!(analytics.counts.completed, 1);
        assert_eq!(analytics.counts.skipped, 0);
        assert_eq!(analytics.trend_buckets.len(), 3);
        assert_eq!(analytics.trend_buckets[1].counts.due, 0);
        assert!(analytics.supportive_facts.contains(&HabitSupportiveFact {
            code: HabitSupportiveFactCode::FullAdherence,
            value: None,
        }));
    }

    #[test]
    fn weekly_and_monthly_bucket_boundaries_are_local_calendar_boundaries() {
        assert_eq!(
            calendar_bucket_start(
                date!(2026 - 09 - 06),
                HabitTrendGranularity::Week,
                DayOfWeek::Monday,
            ),
            Ok(date!(2026 - 08 - 31))
        );
        assert_eq!(
            calendar_bucket_start(
                date!(2026 - 09 - 06),
                HabitTrendGranularity::Week,
                DayOfWeek::Sunday,
            ),
            Ok(date!(2026 - 09 - 06))
        );
        assert_eq!(
            calendar_bucket_start(
                date!(2026 - 09 - 30),
                HabitTrendGranularity::Month,
                DayOfWeek::Monday,
            ),
            Ok(date!(2026 - 09 - 01))
        );
    }

    #[test]
    fn duplicate_occurrences_and_invalid_pauses_are_rejected() {
        let duplicate = analytics_occurrence(
            50,
            date!(2026 - 03 - 01),
            datetime!(2026-03-01 8:00 UTC),
            datetime!(2026-03-01 9:00 UTC),
            HabitOccurrenceOutcome::Pending,
        );
        let input = HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 01),
            as_of: datetime!(2026-03-02 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 02),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses: Vec::new(),
            occurrences: vec![duplicate.clone(), duplicate],
        };
        assert_eq!(
            calculate_habit_analytics(&input),
            Err(HabitAnalyticsError::DuplicateOccurrence(OccurrenceId(
                Uuid::from_u128(50)
            )))
        );

        assert_eq!(
            habit_occurrence_eligibility(
                datetime!(2026-03-01 8:00 UTC),
                datetime!(2026-03-01 9:00 UTC),
                &[HabitPauseInterval {
                    start: datetime!(2026-03-01 10:00 UTC),
                    end: Some(datetime!(2026-03-01 10:00 UTC)),
                }],
                true,
            ),
            Err(HabitPolicyError::InvalidPauseInterval)
        );
    }
}
