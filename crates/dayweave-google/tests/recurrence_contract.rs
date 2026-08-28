use dayweave_google::recurrence::{ByDay, Frequency, RecurrenceParseError, RecurrenceSet, Weekday};

fn lines(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn parses_common_google_weekly_rule_and_preserves_extensions() {
    let parsed = RecurrenceSet::parse(&lines(&[
        "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR;WKST=SU;COUNT=12;X-TEAM=A",
        "X-GOOGLE-CUSTOM:retained",
    ]))
    .expect("weekly rule parses");

    assert_eq!(parsed.rules.len(), 1);
    let rule = &parsed.rules[0];
    assert_eq!(rule.frequency, Frequency::Weekly);
    assert_eq!(rule.interval, 2);
    assert_eq!(rule.count, Some(12));
    assert_eq!(rule.week_start, Some(Weekday::Sunday));
    assert_eq!(
        rule.by_day,
        vec![
            ByDay {
                ordinal: None,
                weekday: Weekday::Monday,
            },
            ByDay {
                ordinal: None,
                weekday: Weekday::Wednesday,
            },
            ByDay {
                ordinal: None,
                weekday: Weekday::Friday,
            },
        ]
    );
    assert_eq!(rule.extensions["X-TEAM"], "A");
    assert_eq!(parsed.extensions, vec!["X-GOOGLE-CUSTOM:retained"]);
}

#[test]
fn retains_timezone_date_and_period_value_shapes() {
    let parsed = RecurrenceSet::parse(&lines(&[
        "RRULE:FREQ=DAILY",
        "EXDATE;TZID=Europe/Madrid:20261025T090000,20261026T090000",
        "RDATE;VALUE=DATE:20261101",
        "RDATE;VALUE=PERIOD:20261102T090000Z/PT30M",
    ]))
    .expect("date lists parse");

    assert_eq!(
        parsed.exclusion_dates[0].time_zone.as_deref(),
        Some("Europe/Madrid")
    );
    assert_eq!(parsed.exclusion_dates[0].values.len(), 2);
    assert_eq!(
        parsed.inclusion_dates[0].value_type.as_deref(),
        Some("DATE")
    );
    assert_eq!(
        parsed.inclusion_dates[1].values,
        vec!["20261102T090000Z/PT30M"]
    );
}

#[test]
fn unfolds_content_lines_and_parses_ordinal_days() {
    let parsed = RecurrenceSet::parse(&lines(&[
        "RRULE:FREQ=MONTHLY;BYDAY=1MO,\r\n -1FR;BYSETPOS=1,-1",
    ]))
    .expect("folded rule parses");
    let rule = &parsed.rules[0];
    assert_eq!(rule.by_day[0].ordinal, Some(1));
    assert_eq!(rule.by_day[1].ordinal, Some(-1));
    assert_eq!(rule.by_set_position, vec![1, -1]);
}

#[test]
fn rejects_ambiguous_or_out_of_range_rules() {
    assert_eq!(
        RecurrenceSet::parse(&lines(&["RRULE:FREQ=DAILY;COUNT=2;UNTIL=20260901T000000Z"]))
            .expect_err("count and until conflict"),
        RecurrenceParseError::CountAndUntil
    );
    assert!(matches!(
        RecurrenceSet::parse(&lines(&["RRULE:FREQ=MONTHLY;BYMONTHDAY=0"])),
        Err(RecurrenceParseError::InvalidRuleValue { .. })
    ));
    assert!(matches!(
        RecurrenceSet::parse(&lines(&["RRULE:INTERVAL=2"])),
        Err(RecurrenceParseError::MissingFrequency)
    ));
    assert!(matches!(
        RecurrenceSet::parse(&lines(&["RRULE:FREQ=DAILY;FREQ=WEEKLY"])),
        Err(RecurrenceParseError::DuplicateRulePart(_))
    ));
}

#[test]
fn parses_full_yearly_selector_surface() {
    let parsed = RecurrenceSet::parse(&lines(&[
        "RRULE:FREQ=YEARLY;BYMONTH=1,12;BYYEARDAY=1,-1;BYWEEKNO=1,-1;BYHOUR=0,23;BYMINUTE=0,59;BYSECOND=0,60",
    ]))
    .expect("yearly selectors parse");
    let rule = &parsed.rules[0];
    assert_eq!(rule.by_month, vec![1, 12]);
    assert_eq!(rule.by_year_day, vec![1, -1]);
    assert_eq!(rule.by_week_number, vec![1, -1]);
    assert_eq!(rule.by_hour, vec![0, 23]);
    assert_eq!(rule.by_minute, vec![0, 59]);
    assert_eq!(rule.by_second, vec![0, 60]);
}
