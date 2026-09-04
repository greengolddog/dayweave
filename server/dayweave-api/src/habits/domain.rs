use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Utc};
use dayweave_core::is_valid_habit_quantity_unit;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

pub const MAX_HABIT_NOTE_CHARS: usize = 10_000;
pub const MAX_HABIT_UNIT_CHARS: usize = dayweave_core::MAX_HABIT_QUANTITY_UNIT_CHARS;
pub const MAX_HABIT_QUANTITY: i64 = dayweave_core::MAX_HABIT_QUANTITY;
pub const MAX_HABIT_ACTUAL_SECONDS: u64 = 366 * 24 * 60 * 60;

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HabitOccurrence {
    pub evidence: HabitOccurrenceEvidence,
    pub outcome: Option<HabitOutcome>,
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

#[must_use]
pub fn calculate_analytics(
    habit_id: Uuid,
    occurrences: &[HabitOccurrence],
    pauses: &[HabitPause],
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
    }) {
        let state = classify(occurrence, pauses, now);
        accumulate(
            &mut totals,
            &mut raw_quantities,
            &mut adherence_sum,
            occurrence,
            state,
        );
        if state != ClassifiedState::Excused && occurrence.evidence.window_end <= now {
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
    if pauses.iter().any(|pause| {
        pause.habit_id == occurrence.evidence.habit_id
            && pause.preserves_streak
            && pause.started_at < occurrence.evidence.window_end
            && pause
                .ended_at
                .is_none_or(|ended| ended > occurrence.evidence.window_start)
    }) {
        return ClassifiedState::Excused;
    }
    match occurrence.outcome.as_ref().map(|outcome| outcome.status) {
        Some(HabitOutcomeStatus::Completed) => ClassifiedState::Completed,
        Some(HabitOutcomeStatus::Partial) => ClassifiedState::Partial,
        Some(HabitOutcomeStatus::Skipped) => ClassifiedState::Skipped,
        Some(HabitOutcomeStatus::Unresolved) | None if occurrence.evidence.window_end <= now => {
            ClassifiedState::Missed
        }
        Some(HabitOutcomeStatus::Unresolved) | None => ClassifiedState::Unresolved,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(date: NaiveDate, status: Option<HabitOutcomeStatus>) -> HabitOccurrence {
        let start = date.and_hms_opt(8, 0, 0).unwrap().and_utc();
        HabitOccurrence {
            evidence: HabitOccurrenceEvidence {
                id: Uuid::new_v4(),
                habit_id: Uuid::from_u128(1),
                planner_occurrence_id: Uuid::new_v4(),
                source_schedule_revision_id: Uuid::new_v4(),
                source_item_revision: 1,
                policy_fingerprint: format!("sha256:{}", "0".repeat(64)),
                identity: serde_json::json!({"type":"daily","date":date}),
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
        }
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
            &[],
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
            &[pause],
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
