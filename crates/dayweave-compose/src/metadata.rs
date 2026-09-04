use std::collections::BTreeSet;

use chrono::{DateTime, Timelike as _, Utc};
use chrono_tz::Tz;
use dayweave_core::{
    AllocationRange, BreakCategory, CalendarEventSpec, ConstraintStrength, EnergyLevel,
    GoalMeasure, HabitMissedPolicy, Minutes, Qualified, QuantityTarget, Recurrence,
    RecurrencePeriod, RecurrenceSemantics, SchedulingConstraints, canonicalize_custom_rrule,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy};

pub const MAX_RECURRENCE_BYTES: usize = 16 * 1_024;
pub const MAX_SCHEDULING_METADATA_BYTES: usize = 32 * 1_024;
/// Largest caller-authored minute offset accepted by the scheduling boundary.
///
/// The public planning horizon is at most 90 days. A leap-year allowance keeps long notice,
/// dependency, spacing, and buffer policies useful while preventing date arithmetic from
/// approaching the representable timestamp boundary.
pub const MAX_SCHEDULING_OFFSET_MINUTES: u32 = 366 * 24 * 60;
const MAX_SOFT_WEIGHT: u32 = 1_000_000;

/// Strict portable contents of a canonical item's `flexible_constraints` object.
///
/// Canonical duration, deadline, earliest start, recurrence, and split bounds remain separate
/// item fields. This object carries advanced scheduler policy without allowing clients to invent
/// unrecognized extension keys.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct SchedulingMetadata {
    #[serde(skip_serializing_if = "is_default")]
    pub constraints: SchedulingConstraints,
    #[serde(skip_serializing_if = "is_false")]
    pub has_own_effort: bool,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub goal_ids: BTreeSet<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy: Option<EnergyMetadata>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar_event: Option<CalendarEventSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar_context: Option<CalendarContextSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dayweave_firm_block: Option<DayWeaveFirmBlockSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub habit_target: Option<QuantityTarget>,
    #[serde(skip_serializing_if = "is_true")]
    pub preserves_streak_when_paused: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub habit_missed_policy: HabitMissedPolicy,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub habit_minimum_spacing_minutes: u32,
    #[serde(skip_serializing_if = "is_false")]
    pub routine_ordered: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub goal_measures: Vec<GoalMeasure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_weekly_allocation: Option<AllocationRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub break_category: Option<BreakCategory>,
    #[serde(skip_serializing_if = "is_false")]
    pub break_mandatory: bool,
    #[serde(skip_serializing_if = "is_true")]
    pub break_prompt_to_resume: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_sessions: Option<u16>,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub minimum_gap_minutes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_split_days: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_start_minute: Option<u16>,
}

impl Default for SchedulingMetadata {
    fn default() -> Self {
        Self {
            constraints: SchedulingConstraints::default(),
            has_own_effort: false,
            goal_ids: BTreeSet::new(),
            energy: None,
            tags: BTreeSet::new(),
            calendar_event: None,
            calendar_context: None,
            dayweave_firm_block: None,
            habit_target: None,
            preserves_streak_when_paused: true,
            habit_missed_policy: HabitMissedPolicy::Ask,
            habit_minimum_spacing_minutes: 0,
            routine_ordered: false,
            goal_measures: Vec::new(),
            goal_weekly_allocation: None,
            break_category: None,
            break_mandatory: false,
            break_prompt_to_resume: true,
            maximum_sessions: None,
            minimum_gap_minutes: 0,
            maximum_split_days: None,
            preferred_start_minute: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarContextSpec {
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
    pub all_day: bool,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct DayWeaveFirmBlockSpec {
    pub owned: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ends_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "is_false")]
    pub all_day: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tentative: bool,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub busy: bool,
}

impl DayWeaveFirmBlockSpec {
    /// Converts a proven DayWeave-owned legacy block into the core event contract.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership is absent or the time range is invalid.
    pub fn as_calendar_event(&self) -> Result<CalendarEventSpec, SchedulingMetadataError> {
        if !self.owned {
            return Err(flexible("dayweave_firm_block must be explicitly owned"));
        }
        validate_calendar_bounds(self.starts_at, self.ends_at, "dayweave_firm_block")?;
        Ok(CalendarEventSpec {
            start: self.starts_at,
            end: self.ends_at,
            immutable: true,
            all_day: self.all_day,
            source_calendar_id: None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EnergyMetadata {
    Simple(EnergyLevel),
    Qualified(Qualified<EnergyLevel>),
}

impl EnergyMetadata {
    #[must_use]
    pub fn into_qualified(self) -> Qualified<EnergyLevel> {
        match self {
            Self::Simple(value) => Qualified::soft(value, 100),
            Self::Qualified(value) => value,
        }
    }
}

/// Canonical scheduling fields needed to validate a write without constructing a stored item.
#[derive(Debug, Clone, Copy)]
pub struct SchedulingMetadataInput<'a> {
    pub item_id: Uuid,
    pub kind: CanonicalItemKind,
    pub status: CanonicalItemStatus,
    pub timezone_name: &'a str,
    pub duration_seconds: Option<u32>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    pub recurrence: Option<&'a Value>,
    pub flexible_constraints: &'a Value,
    pub split_policy: &'a CanonicalSplitPolicy,
    pub parent_id: Option<Uuid>,
}

/// Strictly decoded scheduling policy returned by [`validate_scheduling_metadata`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ValidatedSchedulingMetadata {
    pub metadata: SchedulingMetadata,
    /// Legacy count defaults are materialized in this normalized core recurrence.
    pub recurrence: Option<Recurrence>,
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum SchedulingMetadataError {
    #[error("invalid recurrence: {0}")]
    Recurrence(String),
    #[error("invalid flexible_constraints: {0}")]
    FlexibleConstraints(String),
}

/// Decodes and semantically validates recurrence and advanced scheduling metadata.
///
/// Inbox items remain valid incomplete captures, but any fields that are present must be strict,
/// internally coherent, and valid for the selected item kind.
///
/// # Errors
///
/// Returns a recurrence or flexible-constraint error for unknown keys, malformed values,
/// contradictory canonical fields, unsupported kind combinations, or invalid scheduler policy.
pub fn validate_scheduling_metadata(
    input: SchedulingMetadataInput<'_>,
) -> Result<ValidatedSchedulingMetadata, SchedulingMetadataError> {
    validate_encoded_object(
        input.flexible_constraints,
        MAX_SCHEDULING_METADATA_BYTES,
        "flexible_constraints",
    )?;
    if let Some(recurrence) = input.recurrence {
        if !recurrence.is_object()
            || serde_json::to_vec(recurrence)
                .map_or(true, |encoded| encoded.len() > MAX_RECURRENCE_BYTES)
        {
            return Err(SchedulingMetadataError::Recurrence(
                "recurrence must be a bounded JSON object".to_owned(),
            ));
        }
        validate_recurrence_set_arrays(recurrence)?;
        validate_recurrence_timestamp_strings(recurrence)?;
    }
    validate_metadata_set_arrays(input.flexible_constraints)?;
    validate_metadata_timestamp_strings(input.flexible_constraints)?;

    let metadata: SchedulingMetadata =
        serde_json::from_value(input.flexible_constraints.clone())
            .map_err(|error| flexible(format!("unsupported shape: {error}")))?;
    let mut recurrence = parse_recurrence(input.recurrence)?;
    if let Some(Recurrence::Custom { rrule }) = &mut recurrence {
        *rrule = canonicalize_custom_rrule(rrule)
            .map_err(|error| SchedulingMetadataError::Recurrence(error.to_string()))?;
    }
    validate_recurrence_rule(recurrence.as_ref())?;
    validate_constraints(&metadata.constraints)?;
    validate_metadata_values(&metadata)?;
    validate_kind_keys(input, &metadata, recurrence.as_ref())?;
    validate_split_extensions(input, &metadata)?;
    validate_canonical_interactions(input, &metadata)?;
    Ok(ValidatedSchedulingMetadata {
        metadata,
        recurrence,
    })
}

fn validate_recurrence_set_arrays(value: &Value) -> Result<(), SchedulingMetadataError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if let Some(weekdays) = object.get("weekdays") {
        validate_unique_string_array(weekdays, "recurrence.weekdays")
            .map_err(SchedulingMetadataError::Recurrence)?;
    }
    Ok(())
}

fn validate_recurrence_timestamp_strings(value: &Value) -> Result<(), SchedulingMetadataError> {
    let Some(anchor) = value.as_object().and_then(|object| object.get("anchor")) else {
        return Ok(());
    };
    validate_timestamp_string(anchor, "recurrence.anchor").map_err(recurrence)
}

fn validate_metadata_timestamp_strings(value: &Value) -> Result<(), SchedulingMetadataError> {
    let Some(metadata) = value.as_object() else {
        return Ok(());
    };
    for (key, start, end) in [
        ("calendar_event", "start", "end"),
        ("calendar_context", "start", "end"),
        ("dayweave_firm_block", "starts_at", "ends_at"),
    ] {
        if let Some(object) = metadata.get(key).and_then(Value::as_object) {
            for field in [start, end] {
                if let Some(value) = object.get(field) {
                    validate_timestamp_string(value, &format!("{key}.{field}"))
                        .map_err(flexible)?;
                }
            }
        }
    }
    let Some(constraints) = metadata.get("constraints").and_then(Value::as_object) else {
        return Ok(());
    };
    for field in ["earliest_start", "latest_finish"] {
        if let Some(value) = constraints
            .get(field)
            .and_then(Value::as_object)
            .and_then(|qualified| qualified.get("value"))
        {
            validate_timestamp_string(value, &format!("constraints.{field}.value"))
                .map_err(flexible)?;
        }
    }
    for field in ["preferred_absolute_windows", "forbidden_windows"] {
        if let Some(windows) = constraints.get(field).and_then(Value::as_array) {
            for (index, window) in windows.iter().enumerate() {
                let Some(window) = window
                    .as_object()
                    .and_then(|qualified| qualified.get("value"))
                    .and_then(Value::as_object)
                else {
                    continue;
                };
                for bound in ["start", "end"] {
                    if let Some(value) = window.get(bound) {
                        validate_timestamp_string(
                            value,
                            &format!("constraints.{field}[{index}].value.{bound}"),
                        )
                        .map_err(flexible)?;
                    }
                }
            }
        }
    }
    if let Some(window) = constraints
        .get("occurrence_window")
        .and_then(Value::as_object)
    {
        for bound in ["start", "end"] {
            if let Some(value) = window.get(bound) {
                validate_timestamp_string(value, &format!("constraints.occurrence_window.{bound}"))
                    .map_err(flexible)?;
            }
        }
    }
    Ok(())
}

fn validate_timestamp_string(value: &Value, owner: &str) -> Result<(), String> {
    let Some(value) = value.as_str() else {
        return Ok(());
    };
    if !is_canonical_rfc3339(value) {
        return Err(format!(
            "{owner} must use canonical RFC 3339 syntax (YYYY-MM-DDTHH:MM:SS, optional 1-9 fractional digits, and Z or an offset no larger than +/-18:00)"
        ));
    }
    Ok(())
}

/// Returns whether a timestamp uses the portable RFC 3339 lexical subset shared by native clients.
///
/// This checks syntax only. Callers must still parse the value to validate calendar dates and
/// enforce the microsecond precision required by PostgreSQL-backed canonical state.
#[must_use]
pub fn is_canonical_rfc3339(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || ![0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .into_iter()
            .all(|index| bytes.get(index).is_some_and(u8::is_ascii_digit))
    {
        return false;
    }
    let decimal = |start: usize| -> u8 { (bytes[start] - b'0') * 10 + bytes[start + 1] - b'0' };
    if bytes[..4] == *b"0000"
        || decimal(5) == 0
        || decimal(5) > 12
        || decimal(8) == 0
        || decimal(8) > 31
        || decimal(11) > 23
        || decimal(14) > 59
        || decimal(17) > 59
    {
        return false;
    }
    let mut zone_index = 19;
    if bytes.get(zone_index) == Some(&b'.') {
        zone_index += 1;
        let fraction_start = zone_index;
        while bytes.get(zone_index).is_some_and(u8::is_ascii_digit) {
            zone_index += 1;
        }
        if !(1..=9).contains(&zone_index.saturating_sub(fraction_start)) {
            return false;
        }
    }
    match bytes.get(zone_index) {
        Some(b'Z') => zone_index + 1 == bytes.len(),
        Some(b'+' | b'-') => {
            if zone_index + 6 != bytes.len()
                || bytes.get(zone_index + 3) != Some(&b':')
                || ![
                    zone_index + 1,
                    zone_index + 2,
                    zone_index + 4,
                    zone_index + 5,
                ]
                .into_iter()
                .all(|index| bytes.get(index).is_some_and(u8::is_ascii_digit))
            {
                return false;
            }
            let hours = decimal(zone_index + 1);
            let minutes = decimal(zone_index + 4);
            minutes <= 59 && (hours < 18 || (hours == 18 && minutes == 0))
        }
        _ => false,
    }
}

fn validate_metadata_set_arrays(value: &Value) -> Result<(), SchedulingMetadataError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if let Some(values) = object.get("goal_ids") {
        validate_unique_uuid_array(values, "goal_ids").map_err(flexible)?;
    }
    if let Some(values) = object.get("tags") {
        validate_unique_string_array(values, "tags").map_err(flexible)?;
    }
    let Some(constraints) = object.get("constraints").and_then(Value::as_object) else {
        return Ok(());
    };
    if let Some(values) = constraints
        .get("allowed_weekdays")
        .and_then(Value::as_object)
        .and_then(|qualified| qualified.get("value"))
    {
        validate_unique_string_array(values, "constraints.allowed_weekdays.value")
            .map_err(flexible)?;
    }
    if let Some(windows) = constraints
        .get("preferred_daily_windows")
        .and_then(Value::as_array)
    {
        for (index, values) in windows.iter().enumerate().filter_map(|(index, qualified)| {
            qualified
                .as_object()
                .and_then(|qualified| qualified.get("value"))
                .and_then(Value::as_object)
                .and_then(|window| window.get("weekdays"))
                .map(|weekdays| (index, weekdays))
        }) {
            validate_unique_string_array(
                values,
                &format!("constraints.preferred_daily_windows[{index}].value.weekdays"),
            )
            .map_err(flexible)?;
        }
    }
    Ok(())
}

fn validate_unique_uuid_array(value: &Value, owner: &str) -> Result<(), String> {
    let Some(values) = value.as_array() else {
        return Ok(());
    };
    let mut unique = BTreeSet::new();
    for value in values {
        let Some(value) = value.as_str() else {
            continue;
        };
        let Ok(value) = Uuid::parse_str(value) else {
            continue;
        };
        if !unique.insert(value) {
            return Err(format!("{owner} cannot contain duplicate UUID identifiers"));
        }
    }
    Ok(())
}

fn validate_unique_string_array(value: &Value, owner: &str) -> Result<(), String> {
    let Some(values) = value.as_array() else {
        return Ok(());
    };
    let mut unique = BTreeSet::new();
    for value in values {
        let Some(value) = value.as_str() else {
            continue;
        };
        if !unique.insert(value) {
            return Err(format!("{owner} cannot contain duplicate values"));
        }
    }
    Ok(())
}

/// Parses the canonical recurrence object and materializes legacy count defaults.
///
/// # Errors
///
/// Returns an error for a missing/unknown discriminator, unknown field, or malformed value.
pub fn parse_recurrence(
    value: Option<&Value>,
) -> Result<Option<Recurrence>, SchedulingMetadataError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut value = value.clone();
    let object = value
        .as_object_mut()
        .ok_or_else(|| recurrence("recurrence must be an object"))?;
    let recurrence_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| recurrence("recurrence.type is required"))?;
    match recurrence_type {
        "daily" => {
            object
                .entry("times_per_day")
                .or_insert_with(|| Value::from(1));
        }
        "weekly" => {
            let default = object
                .get("weekdays")
                .and_then(Value::as_array)
                .map_or(1, |days| days.len().max(1));
            object
                .entry("times_per_week")
                .or_insert_with(|| Value::from(default));
            object
                .entry("weekdays")
                .or_insert_with(|| Value::Array(Vec::new()));
        }
        "monthly" => {
            object
                .entry("times_per_month")
                .or_insert_with(|| Value::from(1));
        }
        _ => {}
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|error| recurrence(format!("unsupported shape: {error}")))
}

fn validate_recurrence_rule(value: Option<&Recurrence>) -> Result<(), SchedulingMetadataError> {
    match value {
        Some(Recurrence::Daily { times_per_day: 0 }) => {
            Err(recurrence("times_per_day must be greater than zero"))
        }
        Some(Recurrence::Weekly {
            times_per_week: 0, ..
        }) => Err(recurrence("times_per_week must be greater than zero")),
        Some(Recurrence::Monthly { times_per_month: 0 }) => {
            Err(recurrence("times_per_month must be greater than zero"))
        }
        Some(Recurrence::EveryInterval { interval } | Recurrence::AfterCompletion { interval }) => {
            if interval.is_zero() {
                Err(recurrence("interval must be greater than zero"))
            } else {
                validate_bounded_recurrence_minutes(*interval, "interval")
            }
        }
        Some(Recurrence::Frequency { target: 0, .. }) => {
            Err(recurrence("frequency target must be greater than zero"))
        }
        Some(Recurrence::Frequency {
            target,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Rolling,
            ..
        }) if u32::from(*target) > 24 * 60 => {
            Err(recurrence("rolling daily target exceeds minute precision"))
        }
        Some(Recurrence::Frequency {
            target,
            period: RecurrencePeriod::Week,
            semantics: RecurrenceSemantics::Rolling,
            ..
        }) if u32::from(*target) > 7 * 24 * 60 => {
            Err(recurrence("rolling weekly target exceeds minute precision"))
        }
        Some(Recurrence::Frequency {
            semantics: RecurrenceSemantics::Rolling,
            weekdays,
            ..
        }) if !weekdays.is_empty() => Err(recurrence(
            "rolling frequency cannot select calendar weekdays",
        )),
        Some(Recurrence::Frequency {
            semantics: RecurrenceSemantics::Calendar,
            anchor: Some(_),
            ..
        }) => Err(recurrence(
            "calendar frequency cannot define a rolling anchor",
        )),
        Some(Recurrence::Frequency {
            anchor: Some(anchor),
            ..
        }) if !instant_has_database_precision(*anchor) => Err(recurrence(
            "frequency anchor must use PostgreSQL microsecond precision",
        )),
        Some(Recurrence::Frequency {
            minimum_spacing, ..
        }) => validate_bounded_recurrence_minutes(*minimum_spacing, "frequency minimum_spacing"),
        _ => Ok(()),
    }
}

fn validate_bounded_recurrence_minutes(
    value: Minutes,
    owner: &str,
) -> Result<(), SchedulingMetadataError> {
    if value.get() > MAX_SCHEDULING_OFFSET_MINUTES {
        return Err(recurrence(format!(
            "{owner} must be at most {MAX_SCHEDULING_OFFSET_MINUTES} minutes"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_constraints(value: &SchedulingConstraints) -> Result<(), SchedulingMetadataError> {
    if value
        .earliest_start
        .as_ref()
        .zip(value.latest_finish.as_ref())
        .is_some_and(|(start, end)| start.value >= end.value)
    {
        return Err(flexible(
            "constraints.earliest_start must precede constraints.latest_finish",
        ));
    }
    for (owner, instant) in value
        .earliest_start
        .as_ref()
        .map(|qualified| ("constraints.earliest_start", qualified.value))
        .into_iter()
        .chain(
            value
                .latest_finish
                .as_ref()
                .map(|qualified| ("constraints.latest_finish", qualified.value)),
        )
    {
        validate_instant_precision(instant, owner)?;
    }
    for (owner, windows) in [
        (
            "constraints.preferred_absolute_windows",
            &value.preferred_absolute_windows,
        ),
        ("constraints.forbidden_windows", &value.forbidden_windows),
    ] {
        for window in windows {
            if window.value.start >= window.value.end {
                return Err(flexible(format!("{owner} contains an empty window")));
            }
            validate_instant_precision(window.value.start, owner)?;
            validate_instant_precision(window.value.end, owner)?;
            validate_strength(window.strength, owner)?;
        }
    }
    for window in &value.preferred_daily_windows {
        if window.value.start_minute >= 1_440
            || window.value.end_minute > 1_440
            || window.value.start_minute == window.value.end_minute
        {
            return Err(flexible(
                "constraints.preferred_daily_windows contains an invalid day interval",
            ));
        }
        validate_strength(window.strength, "constraints.preferred_daily_windows")?;
    }
    validate_optional_strength(value.earliest_start.as_ref(), "constraints.earliest_start")?;
    validate_optional_strength(value.latest_finish.as_ref(), "constraints.latest_finish")?;
    validate_optional_strength(value.minimum_notice.as_ref(), "constraints.minimum_notice")?;
    if let Some(notice) = &value.minimum_notice {
        validate_policy_minutes(notice.value, "constraints.minimum_notice")?;
    }
    validate_optional_strength(
        value.allowed_weekdays.as_ref(),
        "constraints.allowed_weekdays",
    )?;
    if value
        .allowed_weekdays
        .as_ref()
        .is_some_and(|weekdays| weekdays.value.is_empty())
    {
        return Err(flexible(
            "constraints.allowed_weekdays cannot be an empty set",
        ));
    }
    validate_optional_strength(
        value.required_location.as_ref(),
        "constraints.required_location",
    )?;
    validate_optional_strength(
        value.maximum_daily_work.as_ref(),
        "constraints.maximum_daily_work",
    )?;
    validate_optional_strength(
        value.maximum_weekly_work.as_ref(),
        "constraints.maximum_weekly_work",
    )?;
    for context in &value.required_contexts {
        validate_strength(context.strength, "constraints.required_contexts")?;
        if context.value.trim().is_empty() {
            return Err(flexible(
                "constraints.required_contexts cannot contain an empty value",
            ));
        }
    }
    if value
        .required_location
        .as_ref()
        .is_some_and(|location| location.value.trim().is_empty())
    {
        return Err(flexible("constraints.required_location cannot be empty"));
    }
    let mut dependency_ids = BTreeSet::new();
    for dependency in &value.dependencies {
        if dependency.item_id.0.is_nil() {
            return Err(flexible(
                "constraints.dependencies cannot reference a nil item",
            ));
        }
        if !dependency_ids.insert(dependency.item_id) {
            return Err(flexible(
                "constraints.dependencies cannot contain duplicate item_id values",
            ));
        }
        validate_policy_minutes(
            dependency.minimum_lag,
            "constraints.dependencies.minimum_lag",
        )?;
        validate_strength(dependency.strength, "constraints.dependencies")?;
    }
    if value.buffers.strength.is_some()
        && value.buffers.before.is_zero()
        && value.buffers.after.is_zero()
    {
        return Err(flexible(
            "constraints.buffers strength requires a non-zero before or after buffer",
        ));
    }
    if let Some(strength) = value.buffers.strength {
        validate_strength(strength, "constraints.buffers")?;
    }
    validate_policy_minutes(value.buffers.before, "constraints.buffers.before")?;
    validate_policy_minutes(value.buffers.after, "constraints.buffers.after")?;
    if value.occurrence_window.is_some() {
        return Err(flexible(
            "constraints.occurrence_window is reserved for generated occurrences",
        ));
    }
    Ok(())
}

fn validate_metadata_values(value: &SchedulingMetadata) -> Result<(), SchedulingMetadataError> {
    if value.goal_ids.iter().any(Uuid::is_nil) {
        return Err(flexible("goal_ids cannot contain a nil identifier"));
    }
    if value.tags.iter().any(|tag| tag.trim().is_empty()) {
        return Err(flexible("tags cannot contain an empty value"));
    }
    if let Some(energy) = &value.energy {
        match energy {
            EnergyMetadata::Simple(_) => {}
            EnergyMetadata::Qualified(qualified) => {
                validate_strength(qualified.strength, "energy")?;
            }
        }
    }
    if let Some(target) = &value.habit_target
        && (target.amount == 0 || target.unit.trim().is_empty())
    {
        return Err(flexible(
            "habit_target requires a positive amount and non-empty unit",
        ));
    }
    validate_policy_minutes(
        Minutes(value.habit_minimum_spacing_minutes),
        "habit_minimum_spacing_minutes",
    )?;
    for measure in &value.goal_measures {
        if measure.name.trim().is_empty() || measure.unit.trim().is_empty() {
            return Err(flexible("goal_measures require non-empty names and units"));
        }
    }
    if let Some(allocation) = value.goal_weekly_allocation
        && allocation
            .maximum
            .is_some_and(|maximum| maximum < allocation.minimum)
    {
        return Err(flexible(
            "goal_weekly_allocation maximum cannot be below minimum",
        ));
    }
    Ok(())
}

fn validate_kind_keys(
    input: SchedulingMetadataInput<'_>,
    metadata: &SchedulingMetadata,
    recurrence_value: Option<&Recurrence>,
) -> Result<(), SchedulingMetadataError> {
    let keys = input
        .flexible_constraints
        .as_object()
        .expect("object shape was validated before key classification");
    let has_any = |names: &[&str]| names.iter().any(|name| keys.contains_key(*name));
    if metadata.goal_ids.contains(&input.item_id) {
        return Err(flexible("goal_ids cannot contain the item itself"));
    }
    if metadata
        .constraints
        .dependencies
        .iter()
        .any(|dependency| dependency.item_id.0 == input.item_id)
    {
        return Err(flexible(
            "constraints.dependencies cannot reference the item itself",
        ));
    }
    if input.kind != CanonicalItemKind::Event
        && has_any(&["calendar_event", "calendar_context", "dayweave_firm_block"])
    {
        return Err(flexible("calendar metadata is only valid for event items"));
    }
    if input.kind != CanonicalItemKind::Habit
        && has_any(&[
            "habit_target",
            "preserves_streak_when_paused",
            "habit_missed_policy",
            "habit_minimum_spacing_minutes",
        ])
    {
        return Err(flexible("habit metadata is only valid for habit items"));
    }
    if input.kind != CanonicalItemKind::Routine && keys.contains_key("routine_ordered") {
        return Err(flexible("routine_ordered is only valid for routine items"));
    }
    if input.kind != CanonicalItemKind::Goal
        && has_any(&["goal_measures", "goal_weekly_allocation"])
    {
        return Err(flexible("goal metadata is only valid for goal items"));
    }
    if input.kind != CanonicalItemKind::Break
        && has_any(&[
            "break_category",
            "break_mandatory",
            "break_prompt_to_resume",
        ])
    {
        return Err(flexible("break metadata is only valid for break items"));
    }

    match input.kind {
        CanonicalItemKind::Event => {
            if recurrence_value.is_some() {
                return Err(recurrence(
                    "event recurrence must be expanded by its calendar source",
                ));
            }
            validate_event_metadata(input, metadata)?;
        }
        CanonicalItemKind::Task | CanonicalItemKind::Routine => {}
        CanonicalItemKind::Habit => {
            if input.status != CanonicalItemStatus::Inbox && recurrence_value.is_none() {
                return Err(recurrence(
                    "habit requires recurrence after it leaves the Inbox",
                ));
            }
        }
        CanonicalItemKind::Goal => {
            if recurrence_value.is_some() {
                return Err(recurrence(
                    "goal does not support recurrence; use a routine or habit",
                ));
            }
        }
        CanonicalItemKind::Project => {
            if recurrence_value.is_some() {
                return Err(recurrence(
                    "project does not support recurrence; use a routine or habit",
                ));
            }
        }
        CanonicalItemKind::Break => {
            if recurrence_value.is_some() {
                return Err(recurrence(
                    "break does not support recurrence; use a routine or habit",
                ));
            }
        }
    }
    Ok(())
}

fn validate_event_metadata(
    input: SchedulingMetadataInput<'_>,
    metadata: &SchedulingMetadata,
) -> Result<(), SchedulingMetadataError> {
    let keys = input
        .flexible_constraints
        .as_object()
        .expect("metadata is an object");
    let variants = usize::from(metadata.calendar_event.is_some())
        + usize::from(metadata.calendar_context.is_some())
        + usize::from(metadata.dayweave_firm_block.is_some());
    if variants > 1 {
        return Err(flexible(
            "event metadata must select exactly one event representation",
        ));
    }
    if input.status != CanonicalItemStatus::Inbox && variants == 0 {
        return Err(flexible(
            "event requires timing metadata after it leaves the Inbox",
        ));
    }
    if let Some(event) = &metadata.calendar_event {
        validate_calendar_bounds(event.start, event.end, "calendar_event")?;
        validate_event_canonical_fields(input, event.start, event.end, event.all_day)?;
        if event
            .source_calendar_id
            .as_ref()
            .is_some_and(|identifier| identifier.trim().is_empty())
        {
            return Err(flexible(
                "calendar_event.source_calendar_id cannot be empty",
            ));
        }
    }
    if let Some(context) = &metadata.calendar_context {
        if input.parent_id.is_some() {
            return Err(flexible("calendar_context event must be a root item"));
        }
        if keys.len() != 1 || !keys.contains_key("calendar_context") {
            return Err(flexible(
                "calendar_context must be the sole scheduling metadata key",
            ));
        }
        validate_calendar_bounds(context.start, context.end, "calendar_context")?;
        validate_event_canonical_fields(input, context.start, context.end, context.all_day)?;
    }
    if let Some(firm) = &metadata.dayweave_firm_block {
        if keys.len() != 1 || !keys.contains_key("dayweave_firm_block") {
            return Err(flexible(
                "dayweave_firm_block must be the sole scheduling metadata key",
            ));
        }
        firm.as_calendar_event()?;
        validate_event_canonical_fields(input, firm.starts_at, firm.ends_at, firm.all_day)?;
    }
    if !matches!(input.split_policy, CanonicalSplitPolicy::Indivisible) {
        return Err(flexible("event items must be indivisible"));
    }
    Ok(())
}

fn validate_event_canonical_fields(
    input: SchedulingMetadataInput<'_>,
    start: OffsetDateTime,
    end: OffsetDateTime,
    all_day: bool,
) -> Result<(), SchedulingMetadataError> {
    if input
        .earliest_start_at
        .is_some_and(|canonical| !same_instant(canonical, start))
    {
        return Err(flexible(
            "event earliest_start_at must equal its metadata start",
        ));
    }
    if input
        .deadline_at
        .is_some_and(|canonical| !same_instant(canonical, end))
    {
        return Err(flexible("event deadline_at must equal its metadata end"));
    }
    if let Some(duration_seconds) = input.duration_seconds {
        let duration = end - start;
        let seconds = duration.whole_seconds();
        if duration != Duration::seconds(seconds)
            || seconds <= 0
            || u32::try_from(seconds) != Ok(duration_seconds)
        {
            return Err(flexible(
                "event duration_seconds must equal its metadata interval",
            ));
        }
    }
    if all_day {
        validate_all_day_bounds(start, end, input.timezone_name)?;
    }
    Ok(())
}

fn validate_all_day_bounds(
    start: OffsetDateTime,
    end: OffsetDateTime,
    timezone_name: &str,
) -> Result<(), SchedulingMetadataError> {
    let timezone: Tz = timezone_name
        .parse()
        .map_err(|_| flexible("all-day event timezone_name must be a valid IANA timezone"))?;
    let localized = |value: OffsetDateTime| {
        DateTime::<Utc>::from_timestamp(value.unix_timestamp(), value.nanosecond())
            .map(|instant| instant.with_timezone(&timezone))
    };
    let start = localized(start)
        .ok_or_else(|| flexible("all-day event start is outside the supported range"))?;
    let end = localized(end)
        .ok_or_else(|| flexible("all-day event end is outside the supported range"))?;
    let is_midnight = |value: &DateTime<Tz>| {
        value.hour() == 0 && value.minute() == 0 && value.second() == 0 && value.nanosecond() == 0
    };
    if !is_midnight(&start) || !is_midnight(&end) || start.date_naive() >= end.date_naive() {
        return Err(flexible(
            "all-day event bounds must be distinct local-midnight dates",
        ));
    }
    Ok(())
}

fn same_instant(value: DateTime<Utc>, other: OffsetDateTime) -> bool {
    value.timestamp() == other.unix_timestamp()
        && value.timestamp_subsec_nanos() == other.nanosecond()
}

fn validate_split_extensions(
    input: SchedulingMetadataInput<'_>,
    metadata: &SchedulingMetadata,
) -> Result<(), SchedulingMetadataError> {
    let has_extensions = metadata.maximum_sessions.is_some()
        || metadata.minimum_gap_minutes != 0
        || metadata.maximum_split_days.is_some();
    if has_extensions && !matches!(input.split_policy, CanonicalSplitPolicy::Splittable { .. }) {
        return Err(flexible(
            "split extension keys require a splittable split_policy",
        ));
    }
    if let CanonicalSplitPolicy::Splittable {
        minimum_chunk_seconds,
        maximum_chunk_seconds,
    } = input.split_policy
    {
        let Some(duration_seconds) = input.duration_seconds else {
            return Err(flexible(
                "splittable split_policy requires a canonical duration",
            ));
        };
        if *minimum_chunk_seconds == 0
            || *maximum_chunk_seconds == 0
            || maximum_chunk_seconds < minimum_chunk_seconds
            || *minimum_chunk_seconds > duration_seconds
            || *maximum_chunk_seconds > duration_seconds
        {
            return Err(flexible(
                "splittable split_policy requires positive ordered chunk bounds within the canonical duration",
            ));
        }
    }
    if metadata.maximum_sessions == Some(0) {
        return Err(flexible("maximum_sessions must be greater than zero"));
    }
    if metadata.maximum_split_days == Some(0) {
        return Err(flexible("maximum_split_days must be greater than zero"));
    }
    validate_policy_minutes(Minutes(metadata.minimum_gap_minutes), "minimum_gap_minutes")?;
    if let (
        Some(duration_seconds),
        CanonicalSplitPolicy::Splittable {
            maximum_chunk_seconds,
            ..
        },
        Some(maximum_sessions),
    ) = (
        input.duration_seconds,
        input.split_policy,
        metadata.maximum_sessions,
    ) {
        let required_sessions = duration_seconds.div_ceil(*maximum_chunk_seconds);
        if required_sessions > u32::from(maximum_sessions) {
            return Err(flexible(
                "maximum_sessions cannot contain the canonical duration within maximum chunks",
            ));
        }
    }
    Ok(())
}

fn validate_canonical_interactions(
    input: SchedulingMetadataInput<'_>,
    metadata: &SchedulingMetadata,
) -> Result<(), SchedulingMetadataError> {
    for (owner, instant) in [
        ("earliest_start_at", input.earliest_start_at),
        ("deadline_at", input.deadline_at),
    ] {
        if instant.is_some_and(|instant| !chrono_instant_has_database_precision(instant)) {
            return Err(flexible(format!(
                "canonical {owner} must use PostgreSQL microsecond precision"
            )));
        }
    }
    if input.earliest_start_at.is_some() && metadata.constraints.earliest_start.is_some() {
        return Err(flexible(
            "earliest start is defined in both the canonical field and metadata",
        ));
    }
    if input.deadline_at.is_some() && metadata.constraints.latest_finish.is_some() {
        return Err(flexible(
            "deadline is defined in both the canonical field and metadata",
        ));
    }
    if input
        .earliest_start_at
        .zip(metadata.constraints.latest_finish.as_ref())
        .is_some_and(|(start, finish)| !chrono_precedes_offset(start, finish.value))
    {
        return Err(flexible(
            "canonical earliest_start_at must precede constraints.latest_finish",
        ));
    }
    if metadata
        .constraints
        .earliest_start
        .as_ref()
        .zip(input.deadline_at)
        .is_some_and(|(start, finish)| !offset_precedes_chrono(start.value, finish))
    {
        return Err(flexible(
            "constraints.earliest_start must precede canonical deadline_at",
        ));
    }
    if let Some(preferred_start) = metadata.preferred_start_minute {
        if input.kind == CanonicalItemKind::Event {
            return Err(flexible(
                "preferred_start_minute is not valid for a fixed event",
            ));
        }
        if preferred_start > 1_439 {
            return Err(flexible("preferred_start_minute must be in 0..=1439"));
        }
        let Some(duration_seconds) = input.duration_seconds else {
            return Err(flexible(
                "preferred_start_minute requires a canonical duration",
            ));
        };
        let duration_minutes = duration_seconds.saturating_add(59) / 60;
        if u32::from(preferred_start).saturating_add(duration_minutes) > 1_440 {
            return Err(flexible(
                "preferred_start_minute duration must finish the same day",
            ));
        }
    }
    Ok(())
}

fn validate_calendar_bounds(
    start: OffsetDateTime,
    end: OffsetDateTime,
    owner: &str,
) -> Result<(), SchedulingMetadataError> {
    if start >= end {
        return Err(flexible(format!("{owner} end must follow start")));
    }
    validate_instant_precision(start, owner)?;
    validate_instant_precision(end, owner)
}

fn validate_instant_precision(
    value: OffsetDateTime,
    owner: &str,
) -> Result<(), SchedulingMetadataError> {
    if !instant_has_database_precision(value) {
        return Err(flexible(format!(
            "{owner} instants must use PostgreSQL microsecond precision"
        )));
    }
    Ok(())
}

fn chrono_instant_has_database_precision(value: DateTime<Utc>) -> bool {
    value.timestamp_subsec_nanos().is_multiple_of(1_000)
}

fn chrono_precedes_offset(left: DateTime<Utc>, right: OffsetDateTime) -> bool {
    (left.timestamp(), left.timestamp_subsec_nanos()) < (right.unix_timestamp(), right.nanosecond())
}

fn offset_precedes_chrono(left: OffsetDateTime, right: DateTime<Utc>) -> bool {
    (left.unix_timestamp(), left.nanosecond()) < (right.timestamp(), right.timestamp_subsec_nanos())
}

fn validate_policy_minutes(value: Minutes, owner: &str) -> Result<(), SchedulingMetadataError> {
    if value.get() > MAX_SCHEDULING_OFFSET_MINUTES {
        return Err(flexible(format!(
            "{owner} must be at most {MAX_SCHEDULING_OFFSET_MINUTES} minutes"
        )));
    }
    Ok(())
}

fn instant_has_database_precision(value: OffsetDateTime) -> bool {
    value.nanosecond().is_multiple_of(1_000)
}

fn validate_optional_strength<T>(
    value: Option<&Qualified<T>>,
    owner: &str,
) -> Result<(), SchedulingMetadataError> {
    if let Some(value) = value {
        validate_strength(value.strength, owner)?;
    }
    Ok(())
}

fn validate_strength(
    value: ConstraintStrength,
    owner: &str,
) -> Result<(), SchedulingMetadataError> {
    if let ConstraintStrength::Soft { weight } = value
        && weight > MAX_SOFT_WEIGHT
    {
        return Err(flexible(format!(
            "{owner} soft weight must be at most {MAX_SOFT_WEIGHT}"
        )));
    }
    Ok(())
}

fn validate_encoded_object(
    value: &Value,
    maximum_bytes: usize,
    owner: &str,
) -> Result<(), SchedulingMetadataError> {
    if !value.is_object()
        || serde_json::to_vec(value).map_or(true, |encoded| encoded.len() > maximum_bytes)
    {
        return Err(flexible(format!("{owner} must be a bounded JSON object")));
    }
    Ok(())
}

fn recurrence(message: impl Into<String>) -> SchedulingMetadataError {
    SchedulingMetadataError::Recurrence(message.into())
}

fn flexible(message: impl Into<String>) -> SchedulingMetadataError {
    SchedulingMetadataError::FlexibleConstraints(message.into())
}

fn default_true() -> bool {
    true
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_true(value: &bool) -> bool {
    *value
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}
