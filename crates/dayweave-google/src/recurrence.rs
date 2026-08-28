//! Strict parsing for the iCalendar recurrence properties returned by Google
//! Calendar. Date values remain lossless strings because `TZID`, floating
//! local time, date-only values, and UTC values require caller timezone policy.

use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecurrenceParseError {
    #[error("recurrence property is malformed: {0}")]
    Malformed(String),
    #[error("RRULE is missing FREQ")]
    MissingFrequency,
    #[error("RRULE contains duplicate {0}")]
    DuplicateRulePart(String),
    #[error("RRULE value for {name} is invalid: {value}")]
    InvalidRuleValue { name: String, value: String },
    #[error("RRULE cannot contain both COUNT and UNTIL")]
    CountAndUntil,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecurrenceSet {
    pub rules: Vec<RecurrenceRule>,
    pub exception_rules: Vec<RecurrenceRule>,
    pub inclusion_dates: Vec<DateValueList>,
    pub exclusion_dates: Vec<DateValueList>,
    /// Non-recurrence properties are preserved so imports remain forward
    /// compatible instead of silently discarding provider extensions.
    pub extensions: Vec<String>,
}

impl RecurrenceSet {
    /// Parses Google Calendar's `event.recurrence` string array.
    ///
    /// # Errors
    ///
    /// Returns [`RecurrenceParseError`] for malformed properties or invalid
    /// RFC 5545 rule values.
    pub fn parse(properties: &[String]) -> Result<Self, RecurrenceParseError> {
        let mut set = Self::default();
        for line in unfold(properties)? {
            let Some((header, value)) = line.split_once(':') else {
                return Err(RecurrenceParseError::Malformed(line));
            };
            let mut header_parts = header.split(';');
            let name = header_parts.next().unwrap_or_default().to_ascii_uppercase();
            let parameters = parse_parameters(header_parts)?;
            match name.as_str() {
                "RRULE" => set.rules.push(RecurrenceRule::parse(value)?),
                "EXRULE" => set.exception_rules.push(RecurrenceRule::parse(value)?),
                "RDATE" => set
                    .inclusion_dates
                    .push(DateValueList::parse(parameters, value)?),
                "EXDATE" => set
                    .exclusion_dates
                    .push(DateValueList::parse(parameters, value)?),
                _ => set.extensions.push(line),
            }
        }
        Ok(set)
    }
}

fn unfold(properties: &[String]) -> Result<Vec<String>, RecurrenceParseError> {
    let mut result: Vec<String> = Vec::new();
    for property in properties {
        for physical in property.replace("\r\n", "\n").split('\n') {
            if physical.starts_with([' ', '\t']) {
                let Some(previous) = result.last_mut() else {
                    return Err(RecurrenceParseError::Malformed(property.clone()));
                };
                previous.push_str(physical.trim_start_matches([' ', '\t']));
            } else if !physical.is_empty() {
                result.push(physical.to_owned());
            }
        }
    }
    Ok(result)
}

fn parse_parameters<'a>(
    values: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>, RecurrenceParseError> {
    let mut result = BTreeMap::new();
    for value in values {
        let Some((name, parameter_value)) = value.split_once('=') else {
            return Err(RecurrenceParseError::Malformed(value.to_owned()));
        };
        let name = name.to_ascii_uppercase();
        if name.is_empty() || parameter_value.is_empty() || result.contains_key(&name) {
            return Err(RecurrenceParseError::Malformed(value.to_owned()));
        }
        result.insert(name, parameter_value.to_owned());
    }
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateValueList {
    pub time_zone: Option<String>,
    pub value_type: Option<String>,
    pub values: Vec<String>,
    pub parameters: BTreeMap<String, String>,
}

impl DateValueList {
    fn parse(
        mut parameters: BTreeMap<String, String>,
        value: &str,
    ) -> Result<Self, RecurrenceParseError> {
        let values: Vec<_> = value.split(',').map(str::to_owned).collect();
        if values.is_empty() || values.iter().any(String::is_empty) {
            return Err(RecurrenceParseError::Malformed(value.to_owned()));
        }
        Ok(Self {
            time_zone: parameters.remove("TZID"),
            value_type: parameters.remove("VALUE"),
            values,
            parameters,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceRule {
    pub frequency: Frequency,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<String>,
    pub week_start: Option<Weekday>,
    pub by_day: Vec<ByDay>,
    pub by_month_day: Vec<i8>,
    pub by_year_day: Vec<i16>,
    pub by_week_number: Vec<i8>,
    pub by_month: Vec<u8>,
    pub by_hour: Vec<u8>,
    pub by_minute: Vec<u8>,
    pub by_second: Vec<u8>,
    pub by_set_position: Vec<i16>,
    pub extensions: BTreeMap<String, String>,
}

impl RecurrenceRule {
    fn parse(value: &str) -> Result<Self, RecurrenceParseError> {
        let mut parts = BTreeMap::new();
        for part in value.split(';') {
            let Some((name, part_value)) = part.split_once('=') else {
                return Err(RecurrenceParseError::Malformed(part.to_owned()));
            };
            let name = name.to_ascii_uppercase();
            if name.is_empty() || part_value.is_empty() {
                return Err(RecurrenceParseError::Malformed(part.to_owned()));
            }
            if parts.insert(name.clone(), part_value.to_owned()).is_some() {
                return Err(RecurrenceParseError::DuplicateRulePart(name));
            }
        }
        let frequency = parts
            .remove("FREQ")
            .ok_or(RecurrenceParseError::MissingFrequency)
            .and_then(|value| Frequency::parse(&value))?;
        let interval = optional_number::<u32>(&mut parts, "INTERVAL")?.unwrap_or(1);
        if interval == 0 {
            return Err(invalid("INTERVAL", "0"));
        }
        let count = optional_number::<u32>(&mut parts, "COUNT")?;
        if count == Some(0) {
            return Err(invalid("COUNT", "0"));
        }
        let until = parts.remove("UNTIL");
        if count.is_some() && until.is_some() {
            return Err(RecurrenceParseError::CountAndUntil);
        }
        let week_start = parts
            .remove("WKST")
            .map(|value| Weekday::parse(&value))
            .transpose()?;
        let by_day = optional_list(&mut parts, "BYDAY", ByDay::parse)?;
        let by_month_day = ranged_list(&mut parts, "BYMONTHDAY", -31_i8, 31, true)?;
        let by_year_day = ranged_list(&mut parts, "BYYEARDAY", -366_i16, 366, true)?;
        let by_week_number = ranged_list(&mut parts, "BYWEEKNO", -53_i8, 53, true)?;
        let by_month = ranged_list(&mut parts, "BYMONTH", 1_u8, 12, false)?;
        let by_hour = ranged_list(&mut parts, "BYHOUR", 0_u8, 23, false)?;
        let by_minute = ranged_list(&mut parts, "BYMINUTE", 0_u8, 59, false)?;
        let by_second = ranged_list(&mut parts, "BYSECOND", 0_u8, 60, false)?;
        let by_set_position = ranged_list(&mut parts, "BYSETPOS", -366_i16, 366, true)?;
        Ok(Self {
            frequency,
            interval,
            count,
            until,
            week_start,
            by_day,
            by_month_day,
            by_year_day,
            by_week_number,
            by_month,
            by_hour,
            by_minute,
            by_second,
            by_set_position,
            extensions: parts,
        })
    }
}

fn optional_number<T: std::str::FromStr>(
    parts: &mut BTreeMap<String, String>,
    name: &str,
) -> Result<Option<T>, RecurrenceParseError> {
    parts
        .remove(name)
        .map(|value| value.parse().map_err(|_| invalid(name, &value)))
        .transpose()
}

fn optional_list<T>(
    parts: &mut BTreeMap<String, String>,
    name: &str,
    parse: impl Fn(&str) -> Result<T, RecurrenceParseError>,
) -> Result<Vec<T>, RecurrenceParseError> {
    parts.remove(name).map_or_else(
        || Ok(Vec::new()),
        |value| {
            let values: Result<Vec<_>, _> = value.split(',').map(parse).collect();
            let values = values?;
            if values.is_empty() {
                Err(invalid(name, &value))
            } else {
                Ok(values)
            }
        },
    )
}

fn ranged_list<T>(
    parts: &mut BTreeMap<String, String>,
    name: &str,
    minimum: T,
    maximum: T,
    disallow_zero: bool,
) -> Result<Vec<T>, RecurrenceParseError>
where
    T: Copy + PartialOrd + PartialEq + Default + std::str::FromStr,
{
    optional_list(parts, name, |value| {
        let parsed: T = value.parse().map_err(|_| invalid(name, value))?;
        if parsed < minimum || parsed > maximum || (disallow_zero && parsed == T::default()) {
            return Err(invalid(name, value));
        }
        Ok(parsed)
    })
}

fn invalid(name: &str, value: &str) -> RecurrenceParseError {
    RecurrenceParseError::InvalidRuleValue {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frequency {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    fn parse(value: &str) -> Result<Self, RecurrenceParseError> {
        match value.to_ascii_uppercase().as_str() {
            "SECONDLY" => Ok(Self::Secondly),
            "MINUTELY" => Ok(Self::Minutely),
            "HOURLY" => Ok(Self::Hourly),
            "DAILY" => Ok(Self::Daily),
            "WEEKLY" => Ok(Self::Weekly),
            "MONTHLY" => Ok(Self::Monthly),
            "YEARLY" => Ok(Self::Yearly),
            _ => Err(invalid("FREQ", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    fn parse(value: &str) -> Result<Self, RecurrenceParseError> {
        match value.to_ascii_uppercase().as_str() {
            "MO" => Ok(Self::Monday),
            "TU" => Ok(Self::Tuesday),
            "WE" => Ok(Self::Wednesday),
            "TH" => Ok(Self::Thursday),
            "FR" => Ok(Self::Friday),
            "SA" => Ok(Self::Saturday),
            "SU" => Ok(Self::Sunday),
            _ => Err(invalid("weekday", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByDay {
    pub ordinal: Option<i8>,
    pub weekday: Weekday,
}

impl ByDay {
    fn parse(value: &str) -> Result<Self, RecurrenceParseError> {
        if value.len() < 2 {
            return Err(invalid("BYDAY", value));
        }
        let (ordinal, weekday) = value.split_at(value.len() - 2);
        let ordinal = if ordinal.is_empty() {
            None
        } else {
            let parsed: i8 = ordinal.parse().map_err(|_| invalid("BYDAY", value))?;
            if parsed == 0 || !(-53..=53).contains(&parsed) {
                return Err(invalid("BYDAY", value));
            }
            Some(parsed)
        };
        Ok(Self {
            ordinal,
            weekday: Weekday::parse(weekday).map_err(|_| invalid("BYDAY", value))?,
        })
    }
}
