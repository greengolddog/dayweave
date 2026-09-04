//! Strict, bounded parsing for authorable custom recurrence rules.

use std::{collections::BTreeSet, fmt::Write as _};

use thiserror::Error;
use time::{Date, Duration, Month};
use uuid::Uuid;

use crate::DayOfWeek;

/// Maximum accepted UTF-8 size of a custom RRULE.
pub const MAX_CUSTOM_RRULE_BYTES: usize = 1_024;

/// Maximum explicit interval for the supported daily, weekly, or monthly unit.
pub const MAX_CUSTOM_RRULE_INTERVAL: u32 = 1_200;

/// Maximum number of occurrences in one finite custom rule.
pub const MAX_CUSTOM_RRULE_OCCURRENCES: u32 = 10_000;

/// Maximum inclusive distance from `DTSTART` to a custom rule's final search day.
pub const MAX_CUSTOM_RRULE_SPAN_DAYS: i64 = 36_600;

const CUSTOM_RULE_NAMESPACE: Uuid = Uuid::from_u128(0x4441_5957_4541_5645_4355_5354_4f4d_0001);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomFrequency {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomTermination {
    Count(u32),
    Until(Date),
}

/// Parsed semantic form shared by validation, expansion, and moved-source proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomRecurrenceRule {
    frequency: CustomFrequency,
    interval: u32,
    by_day: BTreeSet<DayOfWeek>,
    by_month_day: BTreeSet<i8>,
    termination: CustomTermination,
}

/// Parses and returns the canonical semantic spelling of the supported subset.
///
/// Equivalent key order, ASCII case, an optional `RRULE:` prefix, and an omitted
/// `INTERVAL=1` produce the same canonical value. The accepted subset is:
/// `FREQ=DAILY|WEEKLY|MONTHLY`, optional `INTERVAL`, `BYDAY`, `BYMONTHDAY`, and
/// exactly one finite `COUNT` or date-only `UNTIL=YYYYMMDD` terminator.
///
/// # Errors
///
/// Returns a specific error for malformed, duplicate, unsupported, contradictory,
/// or unbounded rule parts.
pub fn canonicalize_custom_rrule(rrule: &str) -> Result<String, CustomRecurrenceRuleError> {
    parse_custom_rrule(rrule).map(|rule| rule.canonical())
}

/// Validates an authorable custom recurrence rule without expanding it.
///
/// # Errors
///
/// Returns a specific parse or boundedness error.
pub fn validate_custom_rrule(rrule: &str) -> Result<(), CustomRecurrenceRuleError> {
    parse_custom_rrule(rrule).map(|_| ())
}

/// Validates both the custom rule grammar and all anchor-dependent bounds.
///
/// Unlike [`validate_custom_rrule`], this rejects an `UNTIL` before the local
/// creation date, unreachable `COUNT` rules, and rules with no occurrence.
/// Call authoring boundaries once the item's local creation date is known.
///
/// # Errors
///
/// Returns a parse, calendar, reachability, or bounded-expansion error.
pub fn validate_custom_rrule_for_anchor(
    rrule: &str,
    anchor: Date,
    week_starts_on: DayOfWeek,
) -> Result<(), CustomRecurrenceRuleError> {
    parse_custom_rrule(rrule)?
        .all_occurrence_dates(anchor, week_starts_on)
        .map(|_| ())
}

/// Conservative number of local dates a core expansion may inspect.
///
/// This is deliberately separate from the in-horizon occurrence bound: one
/// supported custom rule emits at most one occurrence per local date, but a
/// sparse finite rule can inspect dates outside the current planning horizon.
///
/// # Errors
///
/// Returns a parse or anchor-dependent date-bound error.
pub fn custom_rrule_search_day_bound(
    rrule: &str,
    anchor: Date,
) -> Result<usize, CustomRecurrenceRuleError> {
    parse_custom_rrule(rrule)?.search_day_bound(anchor)
}

pub(crate) fn parse_custom_rrule(
    rrule: &str,
) -> Result<CustomRecurrenceRule, CustomRecurrenceRuleError> {
    if rrule.is_empty() {
        return Err(CustomRecurrenceRuleError::Empty);
    }
    if rrule.len() > MAX_CUSTOM_RRULE_BYTES {
        return Err(CustomRecurrenceRuleError::TooLong);
    }
    if !rrule.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(CustomRecurrenceRuleError::InvalidCharacters);
    }
    let body = if rrule
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("RRULE:"))
    {
        &rrule[6..]
    } else {
        rrule
    };
    if body.is_empty() {
        return Err(CustomRecurrenceRuleError::Empty);
    }

    let mut seen = BTreeSet::new();
    let mut frequency = None;
    let mut interval = None;
    let mut by_day = None;
    let mut by_month_day = None;
    let mut count = None;
    let mut until = None;

    for part in body.split(';') {
        let Some((raw_name, raw_value)) = part.split_once('=') else {
            return Err(CustomRecurrenceRuleError::MalformedPart);
        };
        if raw_name.is_empty() || raw_value.is_empty() || raw_value.contains('=') {
            return Err(CustomRecurrenceRuleError::MalformedPart);
        }
        let name = raw_name.to_ascii_uppercase();
        let value = raw_value.to_ascii_uppercase();
        if !seen.insert(name.clone()) {
            return Err(CustomRecurrenceRuleError::DuplicatePart(name));
        }
        match name.as_str() {
            "FREQ" => {
                frequency = Some(match value.as_str() {
                    "DAILY" => CustomFrequency::Daily,
                    "WEEKLY" => CustomFrequency::Weekly,
                    "MONTHLY" => CustomFrequency::Monthly,
                    _ => return Err(CustomRecurrenceRuleError::UnsupportedFrequency(value)),
                });
            }
            "INTERVAL" => interval = Some(parse_bounded_interval(&value)?),
            "BYDAY" => by_day = Some(parse_by_day(&value)?),
            "BYMONTHDAY" => by_month_day = Some(parse_by_month_day(&value)?),
            "COUNT" => count = Some(parse_bounded_count(&value)?),
            "UNTIL" => until = Some(parse_until(&value)?),
            "BYSETPOS" => return Err(CustomRecurrenceRuleError::UnsupportedBySetPosition),
            "BYHOUR" | "BYMINUTE" | "BYSECOND" => {
                return Err(CustomRecurrenceRuleError::UnsupportedTimeComponent(name));
            }
            _ => return Err(CustomRecurrenceRuleError::UnknownPart(name)),
        }
    }

    let frequency = frequency.ok_or(CustomRecurrenceRuleError::MissingFrequency)?;
    let termination = match (count, until) {
        (None, None) => return Err(CustomRecurrenceRuleError::MissingTermination),
        (Some(_), Some(_)) => return Err(CustomRecurrenceRuleError::ConflictingTermination),
        (Some(value), None) => CustomTermination::Count(value),
        (None, Some(value)) => CustomTermination::Until(value),
    };
    let by_day = by_day.unwrap_or_default();
    let by_month_day = by_month_day.unwrap_or_default();
    if frequency == CustomFrequency::Weekly && !by_month_day.is_empty() {
        return Err(CustomRecurrenceRuleError::WeeklyByMonthDay);
    }
    Ok(CustomRecurrenceRule {
        frequency,
        interval: interval.unwrap_or(1),
        by_day,
        by_month_day,
        termination,
    })
}

fn parse_bounded_interval(value: &str) -> Result<u32, CustomRecurrenceRuleError> {
    let interval = parse_plain_u32(value).ok_or(CustomRecurrenceRuleError::InvalidInterval)?;
    if !(1..=MAX_CUSTOM_RRULE_INTERVAL).contains(&interval) {
        return Err(CustomRecurrenceRuleError::InvalidInterval);
    }
    Ok(interval)
}

fn parse_bounded_count(value: &str) -> Result<u32, CustomRecurrenceRuleError> {
    let count = parse_plain_u32(value).ok_or(CustomRecurrenceRuleError::InvalidCount)?;
    if !(1..=MAX_CUSTOM_RRULE_OCCURRENCES).contains(&count) {
        return Err(CustomRecurrenceRuleError::InvalidCount);
    }
    Ok(count)
}

fn parse_plain_u32(value: &str) -> Option<u32> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn parse_by_day(value: &str) -> Result<BTreeSet<DayOfWeek>, CustomRecurrenceRuleError> {
    let mut result = BTreeSet::new();
    for token in value.split(',') {
        let day = match token {
            "MO" => DayOfWeek::Monday,
            "TU" => DayOfWeek::Tuesday,
            "WE" => DayOfWeek::Wednesday,
            "TH" => DayOfWeek::Thursday,
            "FR" => DayOfWeek::Friday,
            "SA" => DayOfWeek::Saturday,
            "SU" => DayOfWeek::Sunday,
            _ if token
                .bytes()
                .any(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-')) =>
            {
                return Err(CustomRecurrenceRuleError::UnsupportedOrdinalByDay);
            }
            _ => return Err(CustomRecurrenceRuleError::InvalidByDay),
        };
        if !result.insert(day) {
            return Err(CustomRecurrenceRuleError::DuplicateByDay(token.to_owned()));
        }
    }
    if result.is_empty() {
        return Err(CustomRecurrenceRuleError::InvalidByDay);
    }
    Ok(result)
}

fn parse_by_month_day(value: &str) -> Result<BTreeSet<i8>, CustomRecurrenceRuleError> {
    let mut result = BTreeSet::new();
    for token in value.split(',') {
        if token.is_empty()
            || !token
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte.is_ascii_digit() || (index == 0 && byte == b'-'))
        {
            return Err(CustomRecurrenceRuleError::InvalidByMonthDay);
        }
        let day = token
            .parse::<i8>()
            .ok()
            .filter(|day| *day != 0 && (-31..=31).contains(day))
            .ok_or(CustomRecurrenceRuleError::InvalidByMonthDay)?;
        if !result.insert(day) {
            return Err(CustomRecurrenceRuleError::DuplicateByMonthDay(day));
        }
    }
    if result.is_empty() {
        return Err(CustomRecurrenceRuleError::InvalidByMonthDay);
    }
    Ok(result)
}

fn parse_until(value: &str) -> Result<Date, CustomRecurrenceRuleError> {
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CustomRecurrenceRuleError::InvalidUntil);
    }
    let year = value[0..4]
        .parse::<i32>()
        .map_err(|_| CustomRecurrenceRuleError::InvalidUntil)?;
    let month = value[4..6]
        .parse::<u8>()
        .ok()
        .and_then(|month| Month::try_from(month).ok())
        .ok_or(CustomRecurrenceRuleError::InvalidUntil)?;
    let day = value[6..8]
        .parse::<u8>()
        .map_err(|_| CustomRecurrenceRuleError::InvalidUntil)?;
    if year == 0 {
        return Err(CustomRecurrenceRuleError::InvalidUntil);
    }
    Date::from_calendar_date(year, month, day).map_err(|_| CustomRecurrenceRuleError::InvalidUntil)
}

impl CustomRecurrenceRule {
    pub(crate) fn canonical(&self) -> String {
        let frequency = match self.frequency {
            CustomFrequency::Daily => "DAILY",
            CustomFrequency::Weekly => "WEEKLY",
            CustomFrequency::Monthly => "MONTHLY",
        };
        let mut result = format!("FREQ={frequency};INTERVAL={}", self.interval);
        if !self.by_day.is_empty() {
            result.push_str(";BYDAY=");
            for (index, day) in self.by_day.iter().enumerate() {
                if index > 0 {
                    result.push(',');
                }
                result.push_str(day_name(*day));
            }
        }
        if !self.by_month_day.is_empty() {
            result.push_str(";BYMONTHDAY=");
            for (index, day) in self.by_month_day.iter().enumerate() {
                if index > 0 {
                    result.push(',');
                }
                write!(&mut result, "{day}").expect("writing to String cannot fail");
            }
        }
        match self.termination {
            CustomTermination::Count(count) => {
                write!(&mut result, ";COUNT={count}").expect("writing to String cannot fail");
            }
            CustomTermination::Until(until) => {
                write!(
                    &mut result,
                    ";UNTIL={:04}{:02}{:02}",
                    until.year(),
                    u8::from(until.month()),
                    until.day()
                )
                .expect("writing to String cannot fail");
            }
        }
        result
    }

    pub(crate) fn rule_id(&self) -> Uuid {
        Uuid::new_v5(&CUSTOM_RULE_NAMESPACE, self.canonical().as_bytes())
    }

    pub(crate) fn occurrence_dates_through(
        &self,
        anchor: Date,
        through: Date,
        week_starts_on: DayOfWeek,
    ) -> Result<Vec<(u32, Date)>, CustomRecurrenceRuleError> {
        let (final_date, target_count) = self.search_bounds(anchor)?;
        let mut result = Vec::new();
        let mut occurrence_count = 0_u32;
        let mut date = anchor;
        loop {
            if self.matches_date(anchor, date, week_starts_on) {
                if occurrence_count == MAX_CUSTOM_RRULE_OCCURRENCES {
                    return Err(CustomRecurrenceRuleError::OccurrenceBudgetExceeded);
                }
                if date <= through {
                    result.push((occurrence_count, date));
                }
                occurrence_count = occurrence_count
                    .checked_add(1)
                    .ok_or(CustomRecurrenceRuleError::OccurrenceBudgetExceeded)?;
                if target_count == Some(occurrence_count) {
                    return Ok(result);
                }
            }
            if date == final_date {
                break;
            }
            date = date
                .next_day()
                .ok_or(CustomRecurrenceRuleError::DateOutOfRange)?;
        }
        Self::validate_scan_result(occurrence_count, target_count)?;
        Ok(result)
    }

    pub(crate) fn all_occurrence_dates(
        &self,
        anchor: Date,
        week_starts_on: DayOfWeek,
    ) -> Result<Vec<Date>, CustomRecurrenceRuleError> {
        self.occurrence_dates_through(anchor, Date::MAX, week_starts_on)
            .map(|dates| dates.into_iter().map(|(_, date)| date).collect())
    }

    fn search_bounds(
        &self,
        anchor: Date,
    ) -> Result<(Date, Option<u32>), CustomRecurrenceRuleError> {
        let final_date = match self.termination {
            CustomTermination::Count(_) => anchor
                .checked_add(Duration::days(MAX_CUSTOM_RRULE_SPAN_DAYS - 1))
                .ok_or(CustomRecurrenceRuleError::DateOutOfRange)?,
            CustomTermination::Until(until) => {
                if until < anchor {
                    return Err(CustomRecurrenceRuleError::UntilBeforeAnchor);
                }
                let span = i64::from(until.to_julian_day())
                    .checked_sub(i64::from(anchor.to_julian_day()))
                    .and_then(|value| value.checked_add(1))
                    .ok_or(CustomRecurrenceRuleError::DateOutOfRange)?;
                if span > MAX_CUSTOM_RRULE_SPAN_DAYS {
                    return Err(CustomRecurrenceRuleError::SpanTooLarge);
                }
                until
            }
        };
        let target_count = match self.termination {
            CustomTermination::Count(count) => Some(count),
            CustomTermination::Until(_) => None,
        };
        Ok((final_date, target_count))
    }

    fn validate_scan_result(
        occurrence_count: u32,
        target_count: Option<u32>,
    ) -> Result<(), CustomRecurrenceRuleError> {
        if occurrence_count == 0 {
            return Err(CustomRecurrenceRuleError::NoOccurrences);
        }
        if target_count.is_some() {
            return Err(CustomRecurrenceRuleError::CountExceedsSpan);
        }
        Ok(())
    }

    fn search_day_bound(&self, anchor: Date) -> Result<usize, CustomRecurrenceRuleError> {
        let (final_date, _) = self.search_bounds(anchor)?;
        let span = i64::from(final_date.to_julian_day())
            .checked_sub(i64::from(anchor.to_julian_day()))
            .and_then(|value| value.checked_add(1))
            .ok_or(CustomRecurrenceRuleError::DateOutOfRange)?;
        usize::try_from(span).map_err(|_| CustomRecurrenceRuleError::DateOutOfRange)
    }

    fn matches_date(&self, anchor: Date, date: Date, week_starts_on: DayOfWeek) -> bool {
        if date < anchor {
            return false;
        }
        let interval_matches = match self.frequency {
            CustomFrequency::Daily => {
                let elapsed = i64::from(date.to_julian_day()) - i64::from(anchor.to_julian_day());
                elapsed.rem_euclid(i64::from(self.interval)) == 0
            }
            CustomFrequency::Weekly => {
                let elapsed_weeks =
                    i64::from(week_key(date, week_starts_on) - week_key(anchor, week_starts_on))
                        / 7;
                elapsed_weeks.rem_euclid(i64::from(self.interval)) == 0
            }
            CustomFrequency::Monthly => {
                let elapsed_months = i64::from(month_index(date) - month_index(anchor));
                elapsed_months.rem_euclid(i64::from(self.interval)) == 0
            }
        };
        if !interval_matches {
            return false;
        }

        let weekday_matches = if self.by_day.is_empty() {
            self.frequency != CustomFrequency::Weekly || date.weekday() == anchor.weekday()
        } else {
            self.by_day.contains(&DayOfWeek::from_time(date.weekday()))
        };
        if !weekday_matches {
            return false;
        }

        if !self.by_month_day.is_empty() {
            let positive = i8::try_from(date.day()).expect("calendar day fits i8");
            let negative = positive
                - i8::try_from(date.month().length(date.year())).expect("month length fits i8")
                - 1;
            return self.by_month_day.contains(&positive) || self.by_month_day.contains(&negative);
        }
        self.frequency != CustomFrequency::Monthly
            || !self.by_day.is_empty()
            || date.day() == anchor.day()
    }
}

const fn day_name(day: DayOfWeek) -> &'static str {
    match day {
        DayOfWeek::Monday => "MO",
        DayOfWeek::Tuesday => "TU",
        DayOfWeek::Wednesday => "WE",
        DayOfWeek::Thursday => "TH",
        DayOfWeek::Friday => "FR",
        DayOfWeek::Saturday => "SA",
        DayOfWeek::Sunday => "SU",
    }
}

fn week_key(date: Date, starts_on: DayOfWeek) -> i32 {
    let day = weekday_index(DayOfWeek::from_time(date.weekday()));
    let start = weekday_index(starts_on);
    date.to_julian_day() - i32::from((7 + day - start) % 7)
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

fn month_index(date: Date) -> i32 {
    date.year() * 12 + i32::from(u8::from(date.month())) - 1
}

/// Strict custom RRULE parsing and bounded-expansion errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CustomRecurrenceRuleError {
    #[error("custom RRULE cannot be empty")]
    Empty,
    #[error("custom RRULE exceeds {MAX_CUSTOM_RRULE_BYTES} bytes")]
    TooLong,
    #[error("custom RRULE must contain printable ASCII without whitespace")]
    InvalidCharacters,
    #[error("custom RRULE contains a malformed part")]
    MalformedPart,
    #[error("custom RRULE contains duplicate part {0}")]
    DuplicatePart(String),
    #[error("custom RRULE requires FREQ")]
    MissingFrequency,
    #[error("custom RRULE frequency {0} is unsupported")]
    UnsupportedFrequency(String),
    #[error("custom RRULE INTERVAL must be in 1..={MAX_CUSTOM_RRULE_INTERVAL}")]
    InvalidInterval,
    #[error("custom RRULE COUNT must be in 1..={MAX_CUSTOM_RRULE_OCCURRENCES}")]
    InvalidCount,
    #[error("custom RRULE BYDAY is invalid")]
    InvalidByDay,
    #[error("custom RRULE does not support ordinal BYDAY entries")]
    UnsupportedOrdinalByDay,
    #[error("custom RRULE BYDAY contains duplicate {0}")]
    DuplicateByDay(String),
    #[error("custom RRULE BYMONTHDAY accepts unique nonzero values in -31..=31")]
    InvalidByMonthDay,
    #[error("custom RRULE BYMONTHDAY contains duplicate {0}")]
    DuplicateByMonthDay(i8),
    #[error("custom RRULE UNTIL must be a valid date-only YYYYMMDD value")]
    InvalidUntil,
    #[error("custom RRULE must define exactly one finite COUNT or UNTIL")]
    MissingTermination,
    #[error("custom RRULE cannot combine COUNT and UNTIL")]
    ConflictingTermination,
    #[error("custom weekly RRULE cannot combine FREQ=WEEKLY with BYMONTHDAY")]
    WeeklyByMonthDay,
    #[error("custom RRULE does not support BYSETPOS")]
    UnsupportedBySetPosition,
    #[error("custom RRULE does not support time component {0}")]
    UnsupportedTimeComponent(String),
    #[error("custom RRULE part {0} is unsupported")]
    UnknownPart(String),
    #[error("custom RRULE UNTIL precedes its item creation anchor")]
    UntilBeforeAnchor,
    #[error("custom RRULE exceeds the {MAX_CUSTOM_RRULE_SPAN_DAYS}-day expansion span")]
    SpanTooLarge,
    #[error("custom RRULE exceeds the {MAX_CUSTOM_RRULE_OCCURRENCES}-occurrence budget")]
    OccurrenceBudgetExceeded,
    #[error("custom RRULE has no occurrence within the supported span")]
    NoOccurrences,
    #[error("custom RRULE COUNT cannot be reached within the supported span")]
    CountExceedsSpan,
    #[error("custom RRULE calendar arithmetic exceeded the supported date range")]
    DateOutOfRange,
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    #[test]
    fn equivalent_rule_spellings_have_one_canonical_form_and_rule_id() {
        let first = parse_custom_rrule("rrule:count=5;byday=fr,mo;freq=weekly").unwrap();
        let second = parse_custom_rrule("FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,FR;COUNT=5").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.rule_id(), second.rule_id());
        assert_eq!(
            first.canonical(),
            "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,FR;COUNT=5"
        );
    }

    #[test]
    fn parser_rejects_duplicate_unknown_unbounded_and_unsupported_parts() {
        let cases = [
            (
                "FREQ=DAILY;FREQ=WEEKLY;COUNT=2",
                CustomRecurrenceRuleError::DuplicatePart("FREQ".to_owned()),
            ),
            (
                "FREQ=DAILY;WKST=MO;COUNT=2",
                CustomRecurrenceRuleError::UnknownPart("WKST".to_owned()),
            ),
            ("FREQ=DAILY", CustomRecurrenceRuleError::MissingTermination),
            (
                "FREQ=MONTHLY;BYDAY=1MO;COUNT=2",
                CustomRecurrenceRuleError::UnsupportedOrdinalByDay,
            ),
            (
                "FREQ=MONTHLY;BYSETPOS=1;COUNT=2",
                CustomRecurrenceRuleError::UnsupportedBySetPosition,
            ),
            (
                "FREQ=DAILY;BYHOUR=9;COUNT=2",
                CustomRecurrenceRuleError::UnsupportedTimeComponent("BYHOUR".to_owned()),
            ),
            (
                "FREQ=WEEKLY;BYMONTHDAY=1;COUNT=2",
                CustomRecurrenceRuleError::WeeklyByMonthDay,
            ),
            (
                "FREQ=DAILY;COUNT=2;UNTIL=20260930",
                CustomRecurrenceRuleError::ConflictingTermination,
            ),
            (
                "FREQ=DAILY;COUNT=2\u{1b}",
                CustomRecurrenceRuleError::InvalidCharacters,
            ),
            (
                "FREQ=DAILY;COUNT=2\u{7f}",
                CustomRecurrenceRuleError::InvalidCharacters,
            ),
            (
                "FREQ=DAILY;COUNT=2\0",
                CustomRecurrenceRuleError::InvalidCharacters,
            ),
        ];
        for (rule, expected) in cases {
            assert_eq!(parse_custom_rrule(rule), Err(expected), "{rule}");
        }
    }

    #[test]
    fn date_matching_obeys_interval_filters_and_negative_month_days() {
        let weekly = parse_custom_rrule("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR;COUNT=4").unwrap();
        assert_eq!(
            weekly
                .all_occurrence_dates(date!(2026 - 09 - 01), DayOfWeek::Monday)
                .unwrap(),
            vec![
                date!(2026 - 09 - 04),
                date!(2026 - 09 - 14),
                date!(2026 - 09 - 18),
                date!(2026 - 09 - 28),
            ]
        );

        let monthly = parse_custom_rrule("FREQ=MONTHLY;BYMONTHDAY=1,-1;UNTIL=20261130").unwrap();
        assert_eq!(
            monthly
                .all_occurrence_dates(date!(2026 - 09 - 15), DayOfWeek::Monday)
                .unwrap(),
            vec![
                date!(2026 - 09 - 30),
                date!(2026 - 10 - 01),
                date!(2026 - 10 - 31),
                date!(2026 - 11 - 01),
                date!(2026 - 11 - 30),
            ]
        );
    }

    #[test]
    fn anchor_dependent_contradictions_and_unsafe_spans_fail_closed() {
        let impossible = parse_custom_rrule("FREQ=DAILY;INTERVAL=7;BYDAY=TU;COUNT=1").unwrap();
        assert_eq!(
            impossible.all_occurrence_dates(date!(2026 - 09 - 07), DayOfWeek::Monday),
            Err(CustomRecurrenceRuleError::NoOccurrences)
        );

        let backwards = parse_custom_rrule("FREQ=DAILY;UNTIL=20260101").unwrap();
        assert_eq!(
            backwards.all_occurrence_dates(date!(2026 - 09 - 01), DayOfWeek::Monday),
            Err(CustomRecurrenceRuleError::UntilBeforeAnchor)
        );

        let too_many = parse_custom_rrule("FREQ=DAILY;UNTIL=20560101").unwrap();
        assert_eq!(
            too_many.all_occurrence_dates(date!(2026 - 09 - 01), DayOfWeek::Monday),
            Err(CustomRecurrenceRuleError::OccurrenceBudgetExceeded)
        );
    }
}
