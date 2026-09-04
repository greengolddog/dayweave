//! Deterministic habit occurrence lifecycle and analytics primitives.
//!
//! The types in this module deliberately contain no storage, timezone database,
//! wall-clock, or messaging dependencies. Callers supply resolved occurrence
//! windows and local dates, then persist the returned transition preimage and
//! projection in one transaction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;
use time::{Date, Duration, OffsetDateTime};

use crate::{
    DayOfWeek, HabitMissedPolicy, HabitSpec, ItemId, Occurrence, OccurrenceId, OccurrenceState,
    RecurrenceException, RecurrenceExceptionAction, RecurrenceExceptionSelector,
    RecurrenceMoveSource,
};

/// One hundred percent expressed in basis points.
pub const HABIT_BASIS_POINTS_SCALE: u16 = 10_000;

/// Maximum number of Unicode scalar values in an occurrence note.
pub const MAX_HABIT_OCCURRENCE_NOTE_CHARS: usize = 10_000;

/// Legacy byte-oriented note bound retained for source compatibility.
pub const MAX_HABIT_OCCURRENCE_NOTE_BYTES: usize = 16 * 1_024;

/// Maximum number of Unicode scalar values in a quantitative unit label.
pub const MAX_HABIT_QUANTITY_UNIT_CHARS: usize = 200;

/// Legacy byte-oriented unit bound retained for source compatibility.
pub const MAX_HABIT_QUANTITY_UNIT_BYTES: usize = 128;

/// Largest absolute quantitative value accepted as occurrence evidence.
pub const MAX_HABIT_QUANTITY: i64 = 1_000_000_000_000;

/// Largest actual elapsed time accepted for one occurrence (366 days).
pub const MAX_HABIT_ACTUAL_SECONDS: u64 = 366 * 24 * 60 * 60;

/// Largest history accepted by one analytics projection.
pub const MAX_HABIT_ANALYTICS_OCCURRENCES: usize = 1_000_000;

/// Largest pause history accepted by one analytics projection.
pub const MAX_HABIT_ANALYTICS_PAUSES: usize = 100_000;

/// Largest inclusive local-date range accepted by one analytics projection.
pub const MAX_HABIT_ANALYTICS_RANGE_DAYS: i64 = 36_600;

/// Optional named quantitative evidence for one habit occurrence.
///
/// This measurement is intentionally independent from normalized percentage
/// progress. Its unit is stored with the value so analytics never mix unlike
/// quantities. The expected target/unit belong to the immutable generated
/// occurrence evidence, not to this mutable outcome projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HabitQuantityProgress {
    pub amount: i64,
    pub unit: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HabitQuantityWire {
    Current(CurrentHabitQuantityWire),
    Legacy(LegacyHabitQuantityWire),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentHabitQuantityWire {
    amount: i64,
    unit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyHabitQuantityWire {
    completed_units: u64,
    target_units: u64,
    unit: String,
}

struct DecodedHabitQuantity {
    value: HabitQuantityProgress,
    legacy_target_units: Option<u64>,
}

impl HabitQuantityWire {
    fn decode(self) -> Result<DecodedHabitQuantity, &'static str> {
        match self {
            Self::Current(value) => Ok(DecodedHabitQuantity {
                value: HabitQuantityProgress {
                    amount: value.amount,
                    unit: value.unit,
                },
                legacy_target_units: None,
            }),
            Self::Legacy(value) => {
                if value.completed_units == 0
                    || value.target_units == 0
                    || value.target_units > i64::MAX as u64
                {
                    return Err(
                        "legacy habit quantity requires positive completed and target units",
                    );
                }
                let amount = i64::try_from(value.completed_units)
                    .map_err(|_| "legacy habit quantity exceeds the signed range")?;
                Ok(DecodedHabitQuantity {
                    value: HabitQuantityProgress {
                        amount,
                        unit: value.unit,
                    },
                    legacy_target_units: Some(value.target_units),
                })
            }
        }
    }
}

impl<'de> Deserialize<'de> for HabitQuantityProgress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        HabitQuantityWire::deserialize(deserializer)?
            .decode()
            .map(|decoded| decoded.value)
            .map_err(D::Error::custom)
    }
}

impl HabitQuantityProgress {
    fn validate(&self) -> Result<(), HabitOccurrenceError> {
        if self.amount.unsigned_abs() > MAX_HABIT_QUANTITY as u64 {
            return Err(HabitOccurrenceError::InvalidQuantity);
        }
        if !valid_text(&self.unit, MAX_HABIT_QUANTITY_UNIT_CHARS, true) {
            return Err(HabitOccurrenceError::InvalidQuantityUnit);
        }
        Ok(())
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HabitOccurrenceOutcome {
    Pending,
    Partial,
    Completed,
    Skipped { reason: HabitSkipReason },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum HabitOutcomeWire {
    Pending,
    Partial {
        #[serde(default)]
        quantity: Option<HabitQuantityWire>,
    },
    Completed {
        #[serde(default)]
        quantity: Option<HabitQuantityWire>,
    },
    Skipped {
        reason: HabitSkipReason,
    },
}

impl HabitOutcomeWire {
    fn decode(
        self,
    ) -> Result<
        (
            HabitOccurrenceOutcome,
            Option<DecodedHabitQuantity>,
            Option<u16>,
        ),
        &'static str,
    > {
        match self {
            Self::Pending => Ok((HabitOccurrenceOutcome::Pending, None, Some(0))),
            Self::Partial { quantity } => {
                let quantity = quantity.map(HabitQuantityWire::decode).transpose()?;
                let inferred = quantity
                    .as_ref()
                    .and_then(|quantity| quantity.legacy_target_units)
                    .map(|target| {
                        let completed = u64::try_from(
                            quantity
                                .as_ref()
                                .expect("legacy target came from this quantity")
                                .value
                                .amount,
                        )
                        .map_err(|_| "legacy partial quantity must be positive")?;
                        if completed >= target {
                            return Err("legacy partial quantity must remain below its target");
                        }
                        let scaled =
                            completed.saturating_mul(u64::from(HABIT_BASIS_POINTS_SCALE)) / target;
                        Ok(
                            u16::try_from(scaled.clamp(1, u64::from(HABIT_BASIS_POINTS_SCALE - 1)))
                                .expect("clamped basis points fit u16"),
                        )
                    })
                    .transpose()?;
                Ok((HabitOccurrenceOutcome::Partial, quantity, inferred))
            }
            Self::Completed { quantity } => {
                let quantity = quantity.map(HabitQuantityWire::decode).transpose()?;
                if let Some((completed, target)) = quantity.as_ref().and_then(|quantity| {
                    quantity
                        .legacy_target_units
                        .map(|target| (quantity.value.amount, target))
                }) {
                    let completed = u64::try_from(completed)
                        .map_err(|_| "legacy completed quantity must be positive")?;
                    if completed < target {
                        return Err("legacy completed quantity must reach its target");
                    }
                }
                Ok((
                    HabitOccurrenceOutcome::Completed,
                    quantity,
                    Some(HABIT_BASIS_POINTS_SCALE),
                ))
            }
            Self::Skipped { reason } => {
                Ok((HabitOccurrenceOutcome::Skipped { reason }, None, Some(0)))
            }
        }
    }
}

impl<'de> Deserialize<'de> for HabitOccurrenceOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        HabitOutcomeWire::deserialize(deserializer)?
            .decode()
            .map(|(outcome, _, _)| outcome)
            .map_err(D::Error::custom)
    }
}

impl HabitOccurrenceOutcome {
    fn is_recorded(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// User-visible value of an occurrence at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrenceValue {
    pub outcome: HabitOccurrenceOutcome,
    /// Normalized completion percentage. Percentage, quantity, and elapsed
    /// time are orthogonal evidence and may be recorded independently.
    pub progress_basis_points: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<HabitQuantityProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the outcome actually occurred. A later correction can keep this
    /// distinct from the command's recording time.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub occurred_at: Option<OffsetDateTime>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HabitOccurrenceValueWire {
    outcome: HabitOutcomeWire,
    #[serde(default)]
    progress_basis_points: Option<u16>,
    #[serde(default)]
    quantity: Option<HabitQuantityWire>,
    #[serde(default)]
    actual_seconds: Option<u64>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default, alias = "effective_at", with = "time::serde::rfc3339::option")]
    occurred_at: Option<OffsetDateTime>,
}

impl<'de> Deserialize<'de> for HabitOccurrenceValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = HabitOccurrenceValueWire::deserialize(deserializer)?;
        let (outcome, embedded_quantity, inferred_progress) =
            wire.outcome.decode().map_err(D::Error::custom)?;
        if embedded_quantity.is_some() && wire.quantity.is_some() {
            return Err(D::Error::custom(
                "habit quantity cannot be present in both outcome and value",
            ));
        }
        let quantity = wire
            .quantity
            .map(HabitQuantityWire::decode)
            .transpose()
            .map_err(D::Error::custom)?
            .or(embedded_quantity)
            .map(|decoded| decoded.value);
        let progress_basis_points = wire
            .progress_basis_points
            .or(inferred_progress)
            .ok_or_else(|| D::Error::custom("partial habit value requires explicit progress"))?;
        Ok(Self {
            outcome,
            progress_basis_points,
            quantity,
            actual_seconds: wire.actual_seconds,
            note: wire.note,
            occurred_at: wire.occurred_at,
        })
    }
}

impl Default for HabitOccurrenceValue {
    fn default() -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Pending,
            progress_basis_points: 0,
            quantity: None,
            actual_seconds: None,
            note: None,
            occurred_at: None,
        }
    }
}

impl HabitOccurrenceValue {
    /// Constructs an empty, not-yet-recorded occurrence value.
    #[must_use]
    pub fn pending() -> Self {
        Self::default()
    }

    /// Constructs a partial value from independent normalized and raw evidence.
    #[must_use]
    pub fn partial(
        progress_basis_points: u16,
        quantity: Option<HabitQuantityProgress>,
        actual_seconds: Option<u64>,
        note: Option<String>,
        occurred_at: OffsetDateTime,
    ) -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Partial,
            progress_basis_points,
            quantity,
            actual_seconds,
            note,
            occurred_at: Some(occurred_at),
        }
    }

    /// Constructs a completed value, with optional quantitative evidence.
    #[must_use]
    pub fn completed(
        quantity: Option<HabitQuantityProgress>,
        actual_seconds: Option<u64>,
        note: Option<String>,
        occurred_at: OffsetDateTime,
    ) -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Completed,
            progress_basis_points: HABIT_BASIS_POINTS_SCALE,
            quantity,
            actual_seconds,
            note,
            occurred_at: Some(occurred_at),
        }
    }

    /// Constructs a skipped value.
    #[must_use]
    pub fn skipped(
        reason: HabitSkipReason,
        progress_basis_points: u16,
        quantity: Option<HabitQuantityProgress>,
        actual_seconds: Option<u64>,
        note: Option<String>,
        occurred_at: OffsetDateTime,
    ) -> Self {
        Self {
            outcome: HabitOccurrenceOutcome::Skipped { reason },
            progress_basis_points,
            quantity,
            actual_seconds,
            note,
            occurred_at: Some(occurred_at),
        }
    }

    /// Validates all status/evidence invariants against an explicit recording time.
    ///
    /// # Errors
    ///
    /// Returns a shape, evidence, or timestamp error when the projection cannot
    /// be stored as one internally consistent occurrence revision.
    pub fn validate(&self, recorded_at: OffsetDateTime) -> Result<(), HabitOccurrenceError> {
        if let Some(quantity) = &self.quantity {
            quantity.validate()?;
        }
        if self
            .actual_seconds
            .is_some_and(|seconds| seconds > MAX_HABIT_ACTUAL_SECONDS)
        {
            return Err(HabitOccurrenceError::InvalidActualSeconds);
        }
        validate_note(self.note.as_deref())?;
        match self.outcome {
            HabitOccurrenceOutcome::Pending
                if self.progress_basis_points != 0
                    || self.quantity.is_some()
                    || self.actual_seconds.is_some()
                    || self.note.is_some() =>
            {
                return Err(HabitOccurrenceError::PendingHasEvidence);
            }
            HabitOccurrenceOutcome::Partial
                if !(1..HABIT_BASIS_POINTS_SCALE).contains(&self.progress_basis_points) =>
            {
                return Err(HabitOccurrenceError::InvalidPartialProgress);
            }
            HabitOccurrenceOutcome::Completed
                if self.progress_basis_points != HABIT_BASIS_POINTS_SCALE =>
            {
                return Err(HabitOccurrenceError::InvalidCompletedProgress);
            }
            HabitOccurrenceOutcome::Skipped { .. }
                if self.progress_basis_points >= HABIT_BASIS_POINTS_SCALE =>
            {
                return Err(HabitOccurrenceError::InvalidSkippedProgress);
            }
            _ => {}
        }
        match (&self.outcome, self.occurred_at) {
            (HabitOccurrenceOutcome::Pending, None) => Ok(()),
            (HabitOccurrenceOutcome::Pending, Some(_)) => {
                Err(HabitOccurrenceError::PendingHasOccurredTime)
            }
            (_, Some(occurred_at)) if occurred_at <= recorded_at => Ok(()),
            (_, Some(_)) => Err(HabitOccurrenceError::OccurredTimeInFuture),
            (_, None) => Err(HabitOccurrenceError::RecordedOutcomeMissingOccurredTime),
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
/// normalized partial progress can advance, complete, or become skipped while
/// retaining its evidence. Reopening, decreasing normalized progress, removing
/// evidence, or changing a terminal result requires an explicit correction.
/// The returned preimage is suitable for audit and undo.
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
            validate_forward_record(&current.value, &value)?;
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
    current: &HabitOccurrenceValue,
    next: &HabitOccurrenceValue,
) -> Result<(), HabitOccurrenceError> {
    match (current.outcome, next.outcome) {
        (HabitOccurrenceOutcome::Pending, outcome) if outcome.is_recorded() => Ok(()),
        (HabitOccurrenceOutcome::Partial, HabitOccurrenceOutcome::Partial) => {
            if next.progress_basis_points <= current.progress_basis_points {
                return Err(HabitOccurrenceError::ProgressDidNotAdvance);
            }
            validate_evidence_preserved(current, next)
        }
        (HabitOccurrenceOutcome::Partial, HabitOccurrenceOutcome::Completed) => {
            validate_evidence_preserved(current, next)
        }
        (HabitOccurrenceOutcome::Partial, HabitOccurrenceOutcome::Skipped { .. }) => {
            if next.progress_basis_points < current.progress_basis_points {
                return Err(HabitOccurrenceError::ProgressRegressed);
            }
            validate_evidence_preserved(current, next)
        }
        _ => Err(HabitOccurrenceError::InvalidForwardTransition),
    }
}

fn validate_evidence_preserved(
    current: &HabitOccurrenceValue,
    next: &HabitOccurrenceValue,
) -> Result<(), HabitOccurrenceError> {
    if let Some(previous) = &current.quantity {
        let Some(next) = &next.quantity else {
            return Err(HabitOccurrenceError::QuantityEvidenceRemoved);
        };
        if previous.unit != next.unit {
            return Err(HabitOccurrenceError::QuantityUnitChanged);
        }
        if previous.amount != next.amount {
            return Err(HabitOccurrenceError::QuantityEvidenceChanged);
        }
    }
    if let Some(previous) = current.actual_seconds {
        let Some(next) = next.actual_seconds else {
            return Err(HabitOccurrenceError::ActualSecondsEvidenceRemoved);
        };
        if next < previous {
            return Err(HabitOccurrenceError::ActualSecondsRegressed);
        }
    }
    if let Some(previous) = &current.note {
        let Some(next) = &next.note else {
            return Err(HabitOccurrenceError::NoteEvidenceRemoved);
        };
        if previous != next {
            return Err(HabitOccurrenceError::NoteEvidenceChanged);
        }
    }
    Ok(())
}

fn validate_note(note: Option<&str>) -> Result<(), HabitOccurrenceError> {
    if let Some(note) = note
        && !valid_text(note, MAX_HABIT_OCCURRENCE_NOTE_CHARS, false)
    {
        return Err(HabitOccurrenceError::InvalidNote);
    }
    Ok(())
}

fn valid_text(value: &str, max_chars: usize, require_trimmed: bool) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && (!require_trimmed || value.trim() == value)
        && !value.chars().any(char::is_control)
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
    #[error("quantity is outside the supported signed integer range")]
    InvalidQuantity,
    #[error("quantity unit must be printable, non-empty, and bounded")]
    InvalidQuantityUnit,
    #[error("actual elapsed seconds exceed the supported bound")]
    InvalidActualSeconds,
    #[error("occurrence note must be non-empty, safe, and bounded")]
    InvalidNote,
    #[error("a pending outcome cannot retain progress or user evidence")]
    PendingHasEvidence,
    #[error("partial progress must be between 1 and 9,999 basis points")]
    InvalidPartialProgress,
    #[error("completed progress must be exactly 10,000 basis points")]
    InvalidCompletedProgress,
    #[error("skipped progress must remain below 10,000 basis points")]
    InvalidSkippedProgress,
    #[error("a pending outcome cannot have an occurrence time")]
    PendingHasOccurredTime,
    #[error("a recorded outcome requires an occurrence time")]
    RecordedOutcomeMissingOccurredTime,
    #[error("occurrence time cannot be later than recording time")]
    OccurredTimeInFuture,
    #[error("ordinary recording does not permit that transition")]
    InvalidForwardTransition,
    #[error("ordinary partial progress must increase")]
    ProgressDidNotAdvance,
    #[error("ordinary recording cannot decrease normalized progress")]
    ProgressRegressed,
    #[error("ordinary progress cannot change its quantity unit")]
    QuantityUnitChanged,
    #[error("ordinary progress cannot rewrite a recorded quantity")]
    QuantityEvidenceChanged,
    #[error("ordinary recording cannot discard quantity evidence")]
    QuantityEvidenceRemoved,
    #[error("ordinary recording cannot discard elapsed-time evidence")]
    ActualSecondsEvidenceRemoved,
    #[error("ordinary elapsed time cannot decrease")]
    ActualSecondsRegressed,
    #[error("ordinary recording cannot discard note evidence")]
    NoteEvidenceRemoved,
    #[error("ordinary recording cannot rewrite a recorded note")]
    NoteEvidenceChanged,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HabitMissedDecision {
    NoAction {
        reason: HabitMissedNoActionReason,
    },
    /// Complete replacement projection ready for an optimistic record command.
    MarkSkipped {
        value: HabitOccurrenceValue,
    },
    /// Concrete replacement window for moving the same stable occurrence.
    CarryForward {
        #[serde(with = "time::serde::rfc3339")]
        window_start: OffsetDateTime,
        #[serde(with = "time::serde::rfc3339")]
        window_end: OffsetDateTime,
    },
    /// Concrete deterministic reduction: suppress exactly this many upcoming
    /// generated occurrences through ordinary skip exceptions.
    ReduceFrequency {
        skip_next_occurrences: u16,
    },
    RequestDecision,
}

/// Evaluates configured missed behavior without mutating the occurrence.
///
/// A pause suppresses missed handling regardless of its analytics policy.
/// Partial progress is still unmet and therefore follows the configured policy
/// after the window closes. A skip decision retains its normalized, quantity,
/// elapsed-time, and note evidence verbatim and only changes status/reason/time.
///
/// # Errors
///
/// Returns an error for an invalid value, occurrence window, or pause.
pub fn decide_habit_missed_behavior(
    policy: HabitMissedPolicy,
    as_of: OffsetDateTime,
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
    value: &HabitOccurrenceValue,
    pauses: &[HabitPauseInterval],
) -> Result<HabitMissedDecision, HabitPolicyError> {
    validate_window(window_start, window_end)?;
    value
        .validate(as_of)
        .map_err(HabitPolicyError::InvalidOutcome)?;
    for pause in pauses {
        pause.validate()?;
    }
    match value.outcome {
        HabitOccurrenceOutcome::Completed => {
            return Ok(HabitMissedDecision::NoAction {
                reason: HabitMissedNoActionReason::AlreadyCompleted,
            });
        }
        HabitOccurrenceOutcome::Skipped { .. } => {
            return Ok(HabitMissedDecision::NoAction {
                reason: HabitMissedNoActionReason::AlreadySkipped,
            });
        }
        HabitOccurrenceOutcome::Pending | HabitOccurrenceOutcome::Partial => {}
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
        HabitMissedPolicy::Skip => HabitMissedDecision::MarkSkipped {
            value: HabitOccurrenceValue::skipped(
                HabitSkipReason::MissedPolicy,
                value.progress_basis_points,
                value.quantity.clone(),
                value.actual_seconds,
                value.note.clone(),
                as_of,
            ),
        },
        HabitMissedPolicy::Carry => {
            let duration = window_end - window_start;
            let carried_end = as_of
                .checked_add(duration)
                .ok_or(HabitPolicyError::TimestampOverflow)?;
            HabitMissedDecision::CarryForward {
                window_start: as_of,
                window_end: carried_end,
            }
        }
        HabitMissedPolicy::ReduceFrequency => HabitMissedDecision::ReduceFrequency {
            skip_next_occurrences: 1,
        },
        HabitMissedPolicy::Ask => HabitMissedDecision::RequestDecision,
    })
}

/// Evaluates missed handling using the policy persisted on the habit.
///
/// # Errors
///
/// Returns the same bounded validation errors as
/// [`decide_habit_missed_behavior`].
pub fn decide_configured_habit_missed_behavior(
    habit: &HabitSpec,
    as_of: OffsetDateTime,
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
    value: &HabitOccurrenceValue,
    pauses: &[HabitPauseInterval],
) -> Result<HabitMissedDecision, HabitPolicyError> {
    decide_habit_missed_behavior(
        habit.missed_policy,
        as_of,
        window_start,
        window_end,
        value,
        pauses,
    )
}

/// Converts a missed-policy decision into exact recurrence exceptions.
///
/// Carry moves the same stable occurrence identity to the decision's concrete
/// replacement window. Reduce-frequency skips the requested number of next
/// generated occurrences in nominal order. Mark-skipped suppresses the missed
/// occurrence itself; ask/no-action decisions make no scheduling change.
/// Every returned move includes the immutable source proof that recurrence
/// expansion independently rederives before accepting it.
///
/// # Errors
///
/// Returns an error for invalid source evidence, an invalid carry window, a
/// zero reduction, or too few materialized future occurrences.
pub fn materialize_habit_missed_scheduling_decision(
    item_revision: u64,
    missed: &Occurrence,
    decision: &HabitMissedDecision,
    occurrences: &[Occurrence],
) -> Result<Vec<RecurrenceException>, HabitPolicyError> {
    match decision {
        HabitMissedDecision::NoAction { .. } | HabitMissedDecision::RequestDecision => {
            Ok(Vec::new())
        }
        HabitMissedDecision::MarkSkipped { .. } => {
            validate_missed_occurrence_source(item_revision, missed)?;
            Ok(vec![skip_exception(missed.series_item_id, missed.id)])
        }
        HabitMissedDecision::CarryForward {
            window_start,
            window_end,
        } => {
            validate_missed_occurrence_source(item_revision, missed)?;
            validate_window(*window_start, *window_end)?;
            Ok(vec![RecurrenceException {
                item_id: missed.series_item_id,
                selector: RecurrenceExceptionSelector::Occurrence { id: missed.id },
                action: RecurrenceExceptionAction::Move {
                    start: *window_start,
                    end: *window_end,
                    source: RecurrenceMoveSource {
                        item_revision,
                        identity: missed.identity,
                        nominal_start: missed.nominal_start,
                        nominal_end: missed.nominal_end,
                        local_date: missed.local_date,
                        ordinal: missed.ordinal,
                    },
                },
            }])
        }
        HabitMissedDecision::ReduceFrequency {
            skip_next_occurrences,
        } => {
            validate_missed_occurrence_source(item_revision, missed)?;
            if *skip_next_occurrences == 0 {
                return Err(HabitPolicyError::InvalidFrequencyReduction);
            }
            let mut upcoming = occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.series_item_id == missed.series_item_id
                        && occurrence.id != missed.id
                        && (occurrence.nominal_start, occurrence.ordinal, occurrence.id)
                            > (missed.nominal_start, missed.ordinal, missed.id)
                        && occurrence.state == OccurrenceState::Generated
                })
                .collect::<Vec<_>>();
            upcoming.sort_by_key(|occurrence| {
                (occurrence.nominal_start, occurrence.ordinal, occurrence.id)
            });
            let count = usize::from(*skip_next_occurrences);
            if upcoming.len() < count {
                return Err(HabitPolicyError::InsufficientUpcomingOccurrences);
            }
            Ok(upcoming
                .into_iter()
                .take(count)
                .map(|occurrence| skip_exception(missed.series_item_id, occurrence.id))
                .collect())
        }
    }
}

fn validate_missed_occurrence_source(
    item_revision: u64,
    occurrence: &Occurrence,
) -> Result<(), HabitPolicyError> {
    if item_revision == 0
        || occurrence.series_item_id.0.is_nil()
        || occurrence.id.0.is_nil()
        || occurrence.nominal_start >= occurrence.nominal_end
        || occurrence.state != OccurrenceState::Generated
    {
        return Err(HabitPolicyError::InvalidOccurrenceEvidence);
    }
    Ok(())
}

const fn skip_exception(item_id: ItemId, occurrence_id: OccurrenceId) -> RecurrenceException {
    RecurrenceException {
        item_id,
        selector: RecurrenceExceptionSelector::Occurrence { id: occurrence_id },
        action: RecurrenceExceptionAction::Skip,
    }
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
    #[error("missed-policy scheduling window exceeds the supported timestamp range")]
    TimestampOverflow,
    #[error("missed-policy scheduling requires a valid generated occurrence preimage")]
    InvalidOccurrenceEvidence,
    #[error("missed-policy frequency reduction must skip at least one occurrence")]
    InvalidFrequencyReduction,
    #[error("missed-policy frequency reduction lacks enough upcoming occurrences")]
    InsufficientUpcomingOccurrences,
}

/// One expected occurrence supplied to the analytics projector.
///
/// The caller resolves `local_date` using the habit's IANA timezone and supplies
/// its exact window separately. This avoids treating a 23- or 25-hour DST day as
/// a fixed UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HabitAnalyticsOccurrence {
    pub occurrence_id: OccurrenceId,
    #[serde(with = "date_serde")]
    pub local_date: Date,
    #[serde(with = "time::serde::rfc3339")]
    pub window_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub window_end: OffsetDateTime,
    /// Current validated lifecycle projection for this occurrence.
    pub value: HabitOccurrenceValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HabitAnalyticsOccurrenceWire {
    occurrence_id: OccurrenceId,
    #[serde(with = "date_serde")]
    local_date: Date,
    #[serde(with = "time::serde::rfc3339")]
    window_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    window_end: OffsetDateTime,
    #[serde(default)]
    value: Option<HabitOccurrenceValue>,
    #[serde(default)]
    outcome: Option<HabitOutcomeWire>,
}

impl<'de> Deserialize<'de> for HabitAnalyticsOccurrence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = HabitAnalyticsOccurrenceWire::deserialize(deserializer)?;
        let value = match (wire.value, wire.outcome) {
            (Some(value), None) => value,
            (None, Some(outcome)) => {
                let (outcome, quantity, progress_basis_points) =
                    outcome.decode().map_err(D::Error::custom)?;
                HabitOccurrenceValue {
                    outcome,
                    progress_basis_points: progress_basis_points.ok_or_else(|| {
                        D::Error::custom("legacy partial analytics outcome requires quantity")
                    })?,
                    quantity: quantity.map(|decoded| decoded.value),
                    actual_seconds: None,
                    note: None,
                    occurred_at: (!matches!(outcome, HabitOccurrenceOutcome::Pending))
                        .then_some(wire.window_end),
                }
            }
            (Some(_), Some(_)) => {
                return Err(D::Error::custom(
                    "analytics occurrence cannot contain both value and legacy outcome",
                ));
            }
            (None, None) => {
                return Err(D::Error::custom(
                    "analytics occurrence requires value or legacy outcome",
                ));
            }
        };
        Ok(Self {
            occurrence_id: wire.occurrence_id,
            local_date: wire.local_date,
            window_start: wire.window_start,
            window_end: wire.window_end,
            value,
        })
    }
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
    /// Actual elapsed time recorded for all due occurrences in this bucket,
    /// including evidence on protected paused occurrences.
    #[serde(default)]
    pub actual_seconds_total: u64,
    /// Quantitative evidence grouped deterministically by exact unit label.
    #[serde(default)]
    pub quantity_totals: Vec<HabitQuantityTotal>,
}

/// Sum of occurrence quantity evidence for one exact unit label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HabitQuantityTotal {
    pub unit: String,
    pub amount: i64,
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
    /// Equal-weight occurrence adherence. Every eligible occurrence contributes
    /// its explicit normalized progress, including retained partial progress on
    /// a skipped result. Division rounds to nearest (half upward) without
    /// floating-point arithmetic.
    pub adherence_basis_points: Option<u16>,
    /// Actual elapsed time recorded across all due occurrences, including
    /// protected paused occurrences.
    #[serde(default)]
    pub actual_seconds_total: u64,
    /// Quantitative evidence grouped deterministically by exact unit label.
    #[serde(default)]
    pub quantity_totals: Vec<HabitQuantityTotal>,
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
    let eligibilities = analytics_eligibilities(input, effective_end);

    for occurrence in &input.occurrences {
        if occurrence.local_date < input.range_start
            || occurrence.local_date > effective_end
            || occurrence.window_end > input.as_of
        {
            continue;
        }
        let eligibility = eligibilities
            .get(&occurrence.occurrence_id)
            .copied()
            .ok_or(HabitAnalyticsError::CalendarOverflow)?;
        total.observe(&occurrence.value, eligibility)?;
        let bucket_start = calendar_bucket_start(
            occurrence.local_date,
            input.trend_granularity,
            input.week_starts_on,
        )?;
        trend
            .get_mut(&bucket_start)
            .ok_or(HabitAnalyticsError::CalendarOverflow)?
            .observe(&occurrence.value, eligibility)?;
        if eligibility != HabitOccurrenceEligibility::PausedProtected {
            let successful = matches!(occurrence.value.outcome, HabitOccurrenceOutcome::Completed);
            streak_dates
                .entry(occurrence.local_date)
                .and_modify(|all_successful| *all_successful &= successful)
                .or_insert(successful);
        }
    }

    let (current_streak, longest_streak) = streaks(&streak_dates)?;
    let adherence_basis_points = total.adherence_basis_points()?;
    let counts = total.counts;
    let actual_seconds_total = total.actual_seconds_total;
    let quantity_totals = total.quantity_totals();
    let partial_progress_count = total.partial_progress_count;
    let trend_buckets = trend
        .into_iter()
        .map(|(start_date, tally)| {
            Ok(HabitTrendBucket {
                start_date,
                counts: tally.counts,
                adherence_basis_points: tally.adherence_basis_points()?,
                actual_seconds_total: tally.actual_seconds_total,
                quantity_totals: tally.quantity_totals(),
            })
        })
        .collect::<Result<Vec<_>, HabitAnalyticsError>>()?;
    let supportive_facts = supportive_facts(
        counts,
        adherence_basis_points,
        current_streak,
        partial_progress_count,
        &trend_buckets,
    );
    Ok(HabitAnalytics {
        counts,
        adherence_basis_points,
        actual_seconds_total,
        quantity_totals,
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
    if input.pauses.len() > MAX_HABIT_ANALYTICS_PAUSES {
        return Err(HabitAnalyticsError::TooManyPauses);
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
            .value
            .validate(input.as_of)
            .map_err(HabitAnalyticsError::InvalidOutcome)?;
    }
    Ok(())
}

fn analytics_eligibilities(
    input: &HabitAnalyticsInput,
    effective_end: Date,
) -> BTreeMap<OccurrenceId, HabitOccurrenceEligibility> {
    let pauses = merged_pauses(&input.pauses);
    let mut occurrences = input
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.local_date >= input.range_start
                && occurrence.local_date <= effective_end
                && occurrence.window_end <= input.as_of
        })
        .collect::<Vec<_>>();
    occurrences.sort_by_key(|occurrence| {
        (
            occurrence.window_start,
            occurrence.window_end,
            occurrence.occurrence_id,
        )
    });
    let mut pause_index = 0_usize;
    let mut result = BTreeMap::new();
    for occurrence in occurrences {
        while pauses
            .get(pause_index)
            .is_some_and(|pause| pause.end.is_some_and(|end| end <= occurrence.window_start))
        {
            pause_index += 1;
        }
        let overlaps = pauses.get(pause_index).is_some_and(|pause| {
            pause.start < occurrence.window_end
                && pause.end.is_none_or(|end| end > occurrence.window_start)
        });
        let eligibility = match (overlaps, input.preserves_statistics_when_paused) {
            (false, _) => HabitOccurrenceEligibility::Eligible,
            (true, true) => HabitOccurrenceEligibility::PausedProtected,
            (true, false) => HabitOccurrenceEligibility::PausedUnprotected,
        };
        result.insert(occurrence.occurrence_id, eligibility);
    }
    result
}

fn merged_pauses(pauses: &[HabitPauseInterval]) -> Vec<HabitPauseInterval> {
    let mut sorted = pauses.to_vec();
    sorted.sort_by_key(|pause| (pause.start, pause.end));
    let mut merged: Vec<HabitPauseInterval> = Vec::new();
    for pause in sorted {
        let Some(last) = merged.last_mut() else {
            merged.push(pause);
            continue;
        };
        let touches = last.end.is_none_or(|end| pause.start <= end);
        if touches {
            last.end = match (last.end, pause.end) {
                (None, _) | (_, None) => None,
                (Some(left), Some(right)) => Some(left.max(right)),
            };
        } else {
            merged.push(pause);
        }
    }
    merged
}

#[derive(Debug, Clone, Default)]
struct MutableTally {
    counts: HabitAnalyticsCounts,
    adherence_sum: u64,
    actual_seconds_total: u64,
    quantity_totals: BTreeMap<String, i64>,
    partial_progress_count: u32,
}

impl MutableTally {
    fn observe(
        &mut self,
        value: &HabitOccurrenceValue,
        eligibility: HabitOccurrenceEligibility,
    ) -> Result<(), HabitAnalyticsError> {
        increment(&mut self.counts.due)?;
        self.actual_seconds_total = self
            .actual_seconds_total
            .checked_add(value.actual_seconds.unwrap_or(0))
            .ok_or(HabitAnalyticsError::ArithmeticOverflow)?;
        if let Some(quantity) = &value.quantity {
            let total = self
                .quantity_totals
                .entry(quantity.unit.clone())
                .or_default();
            *total = total
                .checked_add(quantity.amount)
                .ok_or(HabitAnalyticsError::ArithmeticOverflow)?;
        }
        if eligibility != HabitOccurrenceEligibility::Eligible {
            increment(&mut self.counts.paused)?;
        }
        if eligibility == HabitOccurrenceEligibility::PausedProtected {
            increment(&mut self.counts.protected_paused)?;
            return Ok(());
        }
        increment(&mut self.counts.eligible)?;
        match value.outcome {
            HabitOccurrenceOutcome::Pending => increment(&mut self.counts.pending)?,
            HabitOccurrenceOutcome::Partial => increment(&mut self.counts.partial)?,
            HabitOccurrenceOutcome::Completed => increment(&mut self.counts.completed)?,
            HabitOccurrenceOutcome::Skipped { reason } => {
                increment(&mut self.counts.skipped)?;
                if reason == HabitSkipReason::MissedPolicy {
                    increment(&mut self.counts.missed_policy_skips)?;
                }
            }
        }
        if (1..HABIT_BASIS_POINTS_SCALE).contains(&value.progress_basis_points) {
            increment(&mut self.partial_progress_count)?;
        }
        self.adherence_sum = self
            .adherence_sum
            .checked_add(u64::from(value.progress_basis_points))
            .ok_or(HabitAnalyticsError::ArithmeticOverflow)?;
        Ok(())
    }

    fn adherence_basis_points(&self) -> Result<Option<u16>, HabitAnalyticsError> {
        if self.counts.eligible == 0 {
            return Ok(None);
        }
        let denominator = u64::from(self.counts.eligible);
        let value = self
            .adherence_sum
            .checked_add(denominator / 2)
            .ok_or(HabitAnalyticsError::ArithmeticOverflow)?
            / denominator;
        u16::try_from(value)
            .map(Some)
            .map_err(|_| HabitAnalyticsError::ArithmeticOverflow)
    }

    fn quantity_totals(&self) -> Vec<HabitQuantityTotal> {
        self.quantity_totals
            .iter()
            .map(|(unit, amount)| HabitQuantityTotal {
                unit: unit.clone(),
                amount: *amount,
            })
            .collect()
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
    partial_progress_count: u32,
    trend: &[HabitTrendBucket],
) -> Vec<HabitSupportiveFact> {
    let mut facts = Vec::new();
    if counts.due == 0 {
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
    if partial_progress_count > 0 {
        facts.push(HabitSupportiveFact {
            code: HabitSupportiveFactCode::PartialProgressRecorded,
            value: Some(partial_progress_count),
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
    let latest_pair = trend.len().checked_sub(2).and_then(|index| {
        trend[index]
            .adherence_basis_points
            .zip(trend[index + 1].adherence_basis_points)
    });
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
    #[error("analytics pause count exceeds {MAX_HABIT_ANALYTICS_PAUSES}")]
    TooManyPauses,
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

    fn quantity(amount: i64) -> HabitQuantityProgress {
        HabitQuantityProgress {
            amount,
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
            value: match outcome {
                HabitOccurrenceOutcome::Pending => HabitOccurrenceValue::pending(),
                HabitOccurrenceOutcome::Partial => {
                    HabitOccurrenceValue::partial(5_000, None, None, None, window_end)
                }
                HabitOccurrenceOutcome::Completed => {
                    HabitOccurrenceValue::completed(None, None, None, window_end)
                }
                HabitOccurrenceOutcome::Skipped { reason } => {
                    HabitOccurrenceValue::skipped(reason, 0, None, None, None, window_end)
                }
            },
        }
    }

    fn analytics_occurrence_with_value(
        id: u128,
        local_date: Date,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        value: HabitOccurrenceValue,
    ) -> HabitAnalyticsOccurrence {
        HabitAnalyticsOccurrence {
            occurrence_id: OccurrenceId(Uuid::from_u128(id)),
            local_date,
            window_start,
            window_end,
            value,
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
                        2_500,
                        Some(quantity(2)),
                        Some(600),
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
                        6_250,
                        Some(quantity(2)),
                        Some(1_200),
                        Some("Morning".to_owned()),
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
                        Some(quantity(2)),
                        Some(1_800),
                        Some("Morning".to_owned()),
                        datetime!(2026-03-01 19:58 UTC),
                    ),
                },
            ),
        )
        .unwrap();
        assert_eq!(completed.kind, HabitOccurrenceTransitionKind::Progressed);
        assert!(matches!(
            completed.current.value.outcome,
            HabitOccurrenceOutcome::Completed
        ));
    }

    #[test]
    fn revision_conflicts_and_nonadvancing_partial_progress_fail_closed() {
        let pending = record();
        let stale = HabitOccurrenceCommand {
            expected_revision: 9,
            recorded_at: datetime!(2026-03-01 9:00 UTC),
            kind: HabitOccurrenceCommandKind::Record {
                value: HabitOccurrenceValue::completed(
                    None,
                    None,
                    None,
                    datetime!(2026-03-01 9:00 UTC),
                ),
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
                        2_500,
                        Some(quantity(2)),
                        None,
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
                        2_500,
                        Some(quantity(3)),
                        Some(60),
                        None,
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        );
        assert_eq!(result, Err(HabitOccurrenceError::ProgressDidNotAdvance));
    }

    #[test]
    fn status_and_evidence_shape_invariants_are_strict() {
        assert_eq!(
            HabitOccurrenceValue::partial(
                0,
                Some(quantity(8)),
                None,
                None,
                datetime!(2026-03-01 9:00 UTC),
            )
            .validate(datetime!(2026-03-01 9:00 UTC)),
            Err(HabitOccurrenceError::InvalidPartialProgress)
        );
        assert_eq!(
            HabitOccurrenceValue {
                outcome: HabitOccurrenceOutcome::Completed,
                progress_basis_points: 9_999,
                quantity: None,
                actual_seconds: None,
                note: None,
                occurred_at: Some(datetime!(2026-03-01 9:00 UTC)),
            }
            .validate(datetime!(2026-03-01 9:00 UTC)),
            Err(HabitOccurrenceError::InvalidCompletedProgress)
        );
        assert_eq!(
            HabitOccurrenceValue {
                outcome: HabitOccurrenceOutcome::Pending,
                progress_basis_points: 1,
                quantity: None,
                actual_seconds: None,
                note: None,
                occurred_at: None,
            }
            .validate(datetime!(2026-03-01 9:00 UTC)),
            Err(HabitOccurrenceError::PendingHasEvidence)
        );
        assert_eq!(
            HabitOccurrenceValue::partial(
                1,
                None,
                Some(MAX_HABIT_ACTUAL_SECONDS + 1),
                None,
                datetime!(2026-03-01 9:00 UTC),
            )
            .validate(datetime!(2026-03-01 9:00 UTC)),
            Err(HabitOccurrenceError::InvalidActualSeconds)
        );
        for note in ["line\nfeed", "tab\tvalue", "escape\u{1b}", "delete\u{7f}"] {
            assert_eq!(
                HabitOccurrenceValue::partial(
                    1,
                    None,
                    None,
                    Some(note.to_owned()),
                    datetime!(2026-03-01 9:00 UTC),
                )
                .validate(datetime!(2026-03-01 9:00 UTC)),
                Err(HabitOccurrenceError::InvalidNote),
            );
        }
        for unit in [
            " unit",
            "unit ",
            "line\nfeed",
            "escape\u{1b}",
            "delete\u{7f}",
        ] {
            assert_eq!(
                HabitOccurrenceValue::partial(
                    1,
                    Some(HabitQuantityProgress {
                        amount: 1,
                        unit: unit.to_owned(),
                    }),
                    None,
                    None,
                    datetime!(2026-03-01 9:00 UTC),
                )
                .validate(datetime!(2026-03-01 9:00 UTC)),
                Err(HabitOccurrenceError::InvalidQuantityUnit),
            );
        }
    }

    #[test]
    fn percentage_quantity_and_elapsed_time_are_independent_evidence() {
        let at = datetime!(2026-03-01 9:00 UTC);
        assert_eq!(
            HabitOccurrenceValue::partial(3_333, None, None, None, at).validate(at),
            Ok(())
        );
        assert_eq!(
            HabitOccurrenceValue::partial(5_000, None, Some(900), None, at).validate(at),
            Ok(())
        );
        assert_eq!(
            HabitOccurrenceValue::partial(1, Some(quantity(0)), None, None, at).validate(at),
            Ok(())
        );
        assert_eq!(
            HabitOccurrenceValue::completed(
                Some(HabitQuantityProgress {
                    amount: -25,
                    unit: "net minutes".to_owned(),
                }),
                None,
                None,
                at,
            )
            .validate(at),
            Ok(())
        );
        assert_eq!(
            HabitOccurrenceValue::skipped(
                HabitSkipReason::User,
                9_999,
                None,
                Some(10),
                Some("Partial work still counts".to_owned()),
                at,
            )
            .validate(at),
            Ok(())
        );
        assert_eq!(
            HabitOccurrenceValue::skipped(
                HabitSkipReason::User,
                HABIT_BASIS_POINTS_SCALE,
                None,
                None,
                None,
                at,
            )
            .validate(at),
            Err(HabitOccurrenceError::InvalidSkippedProgress)
        );
        assert_eq!(
            HabitOccurrenceValue::partial(
                1,
                Some(HabitQuantityProgress {
                    amount: i64::MIN,
                    unit: "units".to_owned(),
                }),
                None,
                None,
                at,
            )
            .validate(at),
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
                        Some(900),
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
                        0,
                        None,
                        None,
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
                        2_500,
                        Some(quantity(2)),
                        Some(300),
                        Some("original".to_owned()),
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

        let rewrites_quantity = apply_habit_occurrence_command(
            &partial,
            command(
                &partial,
                datetime!(2026-03-01 10:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        5_000,
                        Some(quantity(3)),
                        Some(300),
                        None,
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        );
        assert_eq!(
            rewrites_quantity,
            Err(HabitOccurrenceError::QuantityEvidenceChanged)
        );

        let mut rewrites_note = partial.value.clone();
        rewrites_note.progress_basis_points = 5_000;
        rewrites_note.note = Some("replacement".to_owned());
        assert_eq!(
            apply_habit_occurrence_command(
                &partial,
                command(
                    &partial,
                    datetime!(2026-03-01 10:00 UTC),
                    HabitOccurrenceCommandKind::Record {
                        value: rewrites_note,
                    },
                ),
            ),
            Err(HabitOccurrenceError::NoteEvidenceChanged),
        );
    }

    #[test]
    fn ordinary_partial_skip_preserves_evidence_but_correction_may_remove_it() {
        let pending = record();
        let partial = apply_habit_occurrence_command(
            &pending,
            command(
                &pending,
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::partial(
                        4_000,
                        Some(quantity(3)),
                        Some(720),
                        Some("Three rounds".to_owned()),
                        datetime!(2026-03-01 8:55 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;

        let dropping_elapsed = apply_habit_occurrence_command(
            &partial,
            command(
                &partial,
                datetime!(2026-03-01 10:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::skipped(
                        HabitSkipReason::User,
                        4_000,
                        Some(quantity(3)),
                        None,
                        Some("Three rounds".to_owned()),
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        );
        assert_eq!(
            dropping_elapsed,
            Err(HabitOccurrenceError::ActualSecondsEvidenceRemoved)
        );

        let skipped = apply_habit_occurrence_command(
            &partial,
            command(
                &partial,
                datetime!(2026-03-01 10:00 UTC),
                HabitOccurrenceCommandKind::Record {
                    value: HabitOccurrenceValue::skipped(
                        HabitSkipReason::User,
                        4_000,
                        Some(quantity(3)),
                        Some(720),
                        Some("Three rounds".to_owned()),
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        )
        .unwrap()
        .current;
        let corrected = apply_habit_occurrence_command(
            &skipped,
            command(
                &skipped,
                datetime!(2026-03-01 11:00 UTC),
                HabitOccurrenceCommandKind::Correct {
                    value: HabitOccurrenceValue::skipped(
                        HabitSkipReason::User,
                        0,
                        None,
                        None,
                        None,
                        datetime!(2026-03-01 10:00 UTC),
                    ),
                },
            ),
        )
        .unwrap();
        assert_eq!(corrected.kind, HabitOccurrenceTransitionKind::Corrected);
        assert_eq!(corrected.current.value.progress_basis_points, 0);
        assert_eq!(corrected.current.value.quantity, None);
        assert_eq!(corrected.current.value.actual_seconds, None);
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
        let pending = HabitOccurrenceValue::pending();
        let cases = [
            (
                HabitMissedPolicy::Skip,
                HabitMissedDecision::MarkSkipped {
                    value: HabitOccurrenceValue::skipped(
                        HabitSkipReason::MissedPolicy,
                        0,
                        None,
                        None,
                        None,
                        as_of,
                    ),
                },
            ),
            (
                HabitMissedPolicy::Carry,
                HabitMissedDecision::CarryForward {
                    window_start: as_of,
                    window_end: datetime!(2026-03-01 11:00 UTC),
                },
            ),
            (
                HabitMissedPolicy::ReduceFrequency,
                HabitMissedDecision::ReduceFrequency {
                    skip_next_occurrences: 1,
                },
            ),
            (HabitMissedPolicy::Ask, HabitMissedDecision::RequestDecision),
        ];
        for (policy, expected) in cases {
            assert_eq!(
                decide_habit_missed_behavior(policy, as_of, start, end, &pending, &[]),
                Ok(expected)
            );
        }

        let configured = HabitSpec {
            recurrence: crate::Recurrence::Daily { times_per_day: 1 },
            target: None,
            preserves_streak_when_paused: true,
            missed_policy: HabitMissedPolicy::Carry,
            minimum_spacing: crate::Minutes(90),
        };
        assert_eq!(
            decide_configured_habit_missed_behavior(&configured, as_of, start, end, &pending, &[],),
            Ok(HabitMissedDecision::CarryForward {
                window_start: as_of,
                window_end: datetime!(2026-03-01 11:00 UTC),
            }),
        );

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
    fn missed_partial_skip_preserves_every_piece_of_progress_evidence() {
        let start = datetime!(2026-03-01 8:00 UTC);
        let end = datetime!(2026-03-01 9:00 UTC);
        let as_of = datetime!(2026-03-01 10:00 UTC);
        let partial = HabitOccurrenceValue::partial(
            4_250,
            Some(HabitQuantityProgress {
                amount: -3,
                unit: "minutes under target".to_owned(),
            }),
            Some(1_530),
            Some("Kept the effort".to_owned()),
            datetime!(2026-03-01 8:45 UTC),
        );

        let decision =
            decide_habit_missed_behavior(HabitMissedPolicy::Skip, as_of, start, end, &partial, &[])
                .unwrap();
        let HabitMissedDecision::MarkSkipped { value } = decision else {
            panic!("skip policy must produce a skipped projection");
        };
        assert_eq!(
            value.outcome,
            HabitOccurrenceOutcome::Skipped {
                reason: HabitSkipReason::MissedPolicy,
            }
        );
        assert_eq!(value.progress_basis_points, 4_250);
        assert_eq!(value.quantity, partial.quantity);
        assert_eq!(value.actual_seconds, partial.actual_seconds);
        assert_eq!(value.note, partial.note);
        assert_eq!(value.occurred_at, Some(as_of));
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
                    HabitOccurrenceOutcome::Completed,
                ),
                analytics_occurrence(
                    11,
                    date!(2026 - 03 - 02),
                    datetime!(2026-03-02 8:00 UTC),
                    datetime!(2026-03-02 9:00 UTC),
                    HabitOccurrenceOutcome::Completed,
                ),
                analytics_occurrence(
                    12,
                    date!(2026 - 03 - 03),
                    datetime!(2026-03-03 8:00 UTC),
                    datetime!(2026-03-03 9:00 UTC),
                    HabitOccurrenceOutcome::Partial,
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
    fn analytics_sums_time_and_signed_quantities_and_credits_skipped_progress() {
        let pauses = vec![HabitPauseInterval {
            start: datetime!(2026-03-04 0:00 UTC),
            end: Some(datetime!(2026-03-05 0:00 UTC)),
        }];
        let occurrences = vec![
            analytics_occurrence_with_value(
                60,
                date!(2026 - 03 - 01),
                datetime!(2026-03-01 8:00 UTC),
                datetime!(2026-03-01 9:00 UTC),
                HabitOccurrenceValue::completed(
                    Some(quantity(2)),
                    Some(600),
                    None,
                    datetime!(2026-03-01 9:00 UTC),
                ),
            ),
            analytics_occurrence_with_value(
                61,
                date!(2026 - 03 - 02),
                datetime!(2026-03-02 8:00 UTC),
                datetime!(2026-03-02 9:00 UTC),
                HabitOccurrenceValue::partial(
                    2_500,
                    Some(quantity(3)),
                    Some(300),
                    None,
                    datetime!(2026-03-02 9:00 UTC),
                ),
            ),
            analytics_occurrence_with_value(
                62,
                date!(2026 - 03 - 03),
                datetime!(2026-03-03 8:00 UTC),
                datetime!(2026-03-03 9:00 UTC),
                HabitOccurrenceValue::skipped(
                    HabitSkipReason::MissedPolicy,
                    5_000,
                    Some(quantity(-1)),
                    Some(120),
                    Some("Retained partial".to_owned()),
                    datetime!(2026-03-03 9:00 UTC),
                ),
            ),
            analytics_occurrence_with_value(
                63,
                date!(2026 - 03 - 04),
                datetime!(2026-03-04 8:00 UTC),
                datetime!(2026-03-04 9:00 UTC),
                HabitOccurrenceValue::completed(
                    Some(HabitQuantityProgress {
                        amount: 5,
                        unit: "minutes".to_owned(),
                    }),
                    Some(60),
                    None,
                    datetime!(2026-03-04 9:00 UTC),
                ),
            ),
        ];
        let analytics = calculate_habit_analytics(&HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 04),
            as_of: datetime!(2026-03-05 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 05),
            trend_granularity: HabitTrendGranularity::Week,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses,
            occurrences,
        })
        .unwrap();

        assert_eq!(analytics.counts.due, 4);
        assert_eq!(analytics.counts.eligible, 3);
        assert_eq!(analytics.counts.protected_paused, 1);
        assert_eq!(analytics.adherence_basis_points, Some(5_833));
        assert_eq!(analytics.actual_seconds_total, 1_080);
        assert_eq!(
            analytics.quantity_totals,
            vec![
                HabitQuantityTotal {
                    unit: "glasses".to_owned(),
                    amount: 4,
                },
                HabitQuantityTotal {
                    unit: "minutes".to_owned(),
                    amount: 5,
                },
            ]
        );
        assert_eq!(analytics.trend_buckets.len(), 2);
        assert_eq!(analytics.trend_buckets[0].actual_seconds_total, 600);
        assert_eq!(analytics.trend_buckets[1].actual_seconds_total, 480);
        assert!(analytics.supportive_facts.contains(&HabitSupportiveFact {
            code: HabitSupportiveFactCode::PartialProgressRecorded,
            value: Some(2),
        }));
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
                    HabitOccurrenceOutcome::Completed,
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
                    HabitOccurrenceOutcome::Completed,
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
        assert_eq!(unprotected.adherence_basis_points, Some(6_667));
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
                    HabitOccurrenceOutcome::Completed,
                ),
                analytics_occurrence(
                    31,
                    date!(2026 - 03 - 30),
                    datetime!(2026-03-29 22:00 UTC),
                    datetime!(2026-03-30 22:00 UTC),
                    HabitOccurrenceOutcome::Completed,
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
                        0,
                        None,
                        None,
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
                        Some(600),
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
            occurrences: vec![analytics_occurrence_with_value(
                40,
                corrected.local_date,
                datetime!(2026-03-01 8:00 UTC),
                datetime!(2026-03-01 9:00 UTC),
                corrected.value,
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

    #[test]
    fn legacy_occurrence_and_analytics_json_remain_readable_but_strict() {
        let legacy = serde_json::json!({
            "outcome": {
                "type": "partial",
                "quantity": {
                    "completed_units": 1,
                    "target_units": 20_000,
                    "unit": "pages"
                }
            },
            "note": "kept",
            "effective_at": "2026-03-01T08:55:00Z"
        });
        let decoded: HabitOccurrenceValue = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.outcome, HabitOccurrenceOutcome::Partial);
        assert_eq!(decoded.progress_basis_points, 1);
        assert_eq!(
            decoded.quantity,
            Some(HabitQuantityProgress {
                amount: 1,
                unit: "pages".to_owned(),
            })
        );
        assert_eq!(decoded.note.as_deref(), Some("kept"));
        assert_eq!(decoded.occurred_at, Some(datetime!(2026-03-01 8:55 UTC)));
        decoded.validate(datetime!(2026-03-01 9:00 UTC)).unwrap();

        let encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(encoded["outcome"], serde_json::json!({"type": "partial"}));
        assert_eq!(encoded["progress_basis_points"], 1);
        assert_eq!(encoded["quantity"]["amount"], 1);
        assert!(encoded.get("effective_at").is_none());

        let legacy_analytics: HabitAnalyticsOccurrence =
            serde_json::from_value(serde_json::json!({
                "occurrence_id": OCCURRENCE_ID,
                "local_date": "2026-03-01",
                "window_start": "2026-03-01T08:00:00Z",
                "window_end": "2026-03-01T09:00:00Z",
                "outcome": {"type": "completed"}
            }))
            .unwrap();
        assert_eq!(
            legacy_analytics.value,
            HabitOccurrenceValue::completed(None, None, None, datetime!(2026-03-01 9:00 UTC),)
        );

        for invalid in [
            serde_json::json!({
                "outcome": {
                    "type": "partial",
                    "quantity": {
                        "completed_units": 2,
                        "target_units": 2,
                        "unit": "pages"
                    }
                },
                "effective_at": "2026-03-01T09:00:00Z"
            }),
            serde_json::json!({
                "outcome": {
                    "type": "completed",
                    "quantity": {
                        "completed_units": 1,
                        "target_units": 2,
                        "unit": "pages"
                    }
                },
                "effective_at": "2026-03-01T09:00:00Z"
            }),
            serde_json::json!({
                "outcome": {"type": "partial"},
                "effective_at": "2026-03-01T09:00:00Z"
            }),
        ] {
            assert!(serde_json::from_value::<HabitOccurrenceValue>(invalid).is_err());
        }
    }

    #[test]
    fn pause_projection_is_bounded_and_matches_half_open_union_semantics() {
        let pauses = vec![
            HabitPauseInterval {
                start: datetime!(2026-03-02 8:30 UTC),
                end: Some(datetime!(2026-03-02 10:00 UTC)),
            },
            HabitPauseInterval {
                start: datetime!(2026-03-01 8:00 UTC),
                end: Some(datetime!(2026-03-01 9:00 UTC)),
            },
            HabitPauseInterval {
                start: datetime!(2026-03-02 8:00 UTC),
                end: Some(datetime!(2026-03-02 9:00 UTC)),
            },
        ];
        let occurrences = vec![
            analytics_occurrence(
                70,
                date!(2026 - 03 - 01),
                datetime!(2026-03-01 9:00 UTC),
                datetime!(2026-03-01 10:00 UTC),
                HabitOccurrenceOutcome::Pending,
            ),
            analytics_occurrence(
                71,
                date!(2026 - 03 - 02),
                datetime!(2026-03-02 9:30 UTC),
                datetime!(2026-03-02 10:30 UTC),
                HabitOccurrenceOutcome::Pending,
            ),
        ];
        let input = HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 02),
            as_of: datetime!(2026-03-03 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 03),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses,
            occurrences,
        };
        let analytics = calculate_habit_analytics(&input).unwrap();
        assert_eq!(analytics.counts.due, 2);
        assert_eq!(analytics.counts.protected_paused, 1);
        assert_eq!(analytics.counts.eligible, 1);

        let too_many = HabitAnalyticsInput {
            pauses: vec![
                HabitPauseInterval {
                    start: datetime!(2026-03-01 8:00 UTC),
                    end: Some(datetime!(2026-03-01 9:00 UTC)),
                };
                MAX_HABIT_ANALYTICS_PAUSES + 1
            ],
            occurrences: Vec::new(),
            ..input
        };
        assert_eq!(
            calculate_habit_analytics(&too_many),
            Err(HabitAnalyticsError::TooManyPauses),
        );
    }

    #[test]
    fn supportive_facts_do_not_invent_no_due_personal_best_or_gapped_trends() {
        let protected = analytics_occurrence(
            80,
            date!(2026 - 03 - 01),
            datetime!(2026-03-01 8:00 UTC),
            datetime!(2026-03-01 9:00 UTC),
            HabitOccurrenceOutcome::Pending,
        );
        let analytics = calculate_habit_analytics(&HabitAnalyticsInput {
            range_start: date!(2026 - 03 - 01),
            range_end: date!(2026 - 03 - 03),
            as_of: datetime!(2026-03-04 0:00 UTC),
            as_of_local_date: date!(2026 - 03 - 04),
            trend_granularity: HabitTrendGranularity::Day,
            week_starts_on: DayOfWeek::Monday,
            preserves_statistics_when_paused: true,
            pauses: vec![HabitPauseInterval {
                start: datetime!(2026-03-01 8:00 UTC),
                end: Some(datetime!(2026-03-01 9:00 UTC)),
            }],
            occurrences: vec![protected],
        })
        .unwrap();
        assert_eq!(analytics.counts.due, 1);
        assert_eq!(analytics.counts.eligible, 0);
        assert!(!analytics.supportive_facts.iter().any(|fact| matches!(
            fact.code,
            HabitSupportiveFactCode::NoDueOccurrences
                | HabitSupportiveFactCode::PersonalBest
                | HabitSupportiveFactCode::ImprovingTrend
        )));
        assert!(analytics.supportive_facts.contains(&HabitSupportiveFact {
            code: HabitSupportiveFactCode::PausedOccurrencesProtected,
            value: Some(1),
        }));
    }
}
