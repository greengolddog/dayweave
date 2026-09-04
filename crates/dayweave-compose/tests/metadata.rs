use chrono::{DateTime, Utc};
use dayweave_compose::{
    CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy, SchedulingMetadata,
    SchedulingMetadataInput, validate_scheduling_metadata,
};
use dayweave_core::Recurrence;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
struct FixtureFile {
    schema: String,
    cases: Vec<FixtureCase>,
}

#[derive(Deserialize)]
struct FixtureCase {
    name: String,
    #[serde(default)]
    expected_error_contains: Option<String>,
    fields: FixtureFields,
}

#[derive(Deserialize)]
struct FixtureFields {
    item_id: Uuid,
    kind: CanonicalItemKind,
    status: CanonicalItemStatus,
    timezone_name: String,
    duration_seconds: Option<u32>,
    deadline_at: Option<DateTime<Utc>>,
    earliest_start_at: Option<DateTime<Utc>>,
    recurrence: Option<Value>,
    flexible_constraints: Value,
    split_policy: CanonicalSplitPolicy,
    parent_id: Option<Uuid>,
}

impl FixtureFields {
    fn input(&self) -> SchedulingMetadataInput<'_> {
        SchedulingMetadataInput {
            item_id: self.item_id,
            kind: self.kind,
            status: self.status,
            timezone_name: &self.timezone_name,
            duration_seconds: self.duration_seconds,
            deadline_at: self.deadline_at,
            earliest_start_at: self.earliest_start_at,
            recurrence: self.recurrence.as_ref(),
            flexible_constraints: &self.flexible_constraints,
            split_policy: &self.split_policy,
            parent_id: self.parent_id,
        }
    }
}

fn fixture(name: &str) -> FixtureFile {
    let source = match name {
        "valid-rich-items.json" => {
            include_str!("../../../fixtures/scheduling-metadata/valid-rich-items.json")
        }
        "invalid-items.json" => {
            include_str!("../../../fixtures/scheduling-metadata/invalid-items.json")
        }
        _ => panic!("unknown fixture {name}"),
    };
    serde_json::from_str(source).expect("fixture must be strict JSON")
}

#[test]
fn legacy_recurrence_defaults_are_normalized_exactly() {
    let fixture = fixture("valid-rich-items.json");
    let recurrence = |name: &str| {
        let case = fixture
            .cases
            .iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("missing case {name}"));
        validate_scheduling_metadata(case.fields.input())
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .recurrence
            .expect("fixture must be recurring")
    };
    assert_eq!(
        recurrence("legacy_daily_default_count"),
        Recurrence::Daily { times_per_day: 1 }
    );
    assert!(matches!(
        recurrence("legacy_weekly_default_count"),
        Recurrence::Weekly {
            times_per_week: 2,
            ..
        }
    ));
    assert_eq!(
        recurrence("legacy_monthly_default_count"),
        Recurrence::Monthly { times_per_month: 1 }
    );
}

#[test]
fn every_shared_rich_fixture_is_accepted_and_serializable() {
    let fixture = fixture("valid-rich-items.json");
    assert_eq!(fixture.schema, "dayweave.scheduling-metadata-fixtures/1");
    assert!(!fixture.cases.is_empty());
    let mut saw_explicit_split_defaults = false;
    for case in fixture.cases {
        assert!(case.expected_error_contains.is_none(), "{}", case.name);
        let validated = validate_scheduling_metadata(case.fields.input())
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        if case.name == "indivisible_explicit_default_split_extensions" {
            saw_explicit_split_defaults = true;
            assert_eq!(validated.metadata.maximum_sessions, None);
            assert_eq!(validated.metadata.minimum_gap_minutes, 0);
            assert_eq!(validated.metadata.maximum_split_days, None);
        }
        let encoded = serde_json::to_value(&validated.metadata)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let round_trip: SchedulingMetadata = serde_json::from_value(encoded)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(round_trip, validated.metadata, "{}", case.name);
    }
    assert!(
        saw_explicit_split_defaults,
        "shared fixtures must cover semantic split defaults on an indivisible item"
    );
}

#[test]
fn every_shared_invalid_fixture_fails_for_the_documented_reason() {
    let fixture = fixture("invalid-items.json");
    assert_eq!(fixture.schema, "dayweave.scheduling-metadata-fixtures/1");
    assert!(!fixture.cases.is_empty());
    for case in fixture.cases {
        let expected = case
            .expected_error_contains
            .as_deref()
            .unwrap_or_else(|| panic!("{} must document an expected error", case.name));
        let Err(error) = validate_scheduling_metadata(case.fields.input()) else {
            panic!("{} unexpectedly passed", case.name);
        };
        assert!(
            error.to_string().contains(expected),
            "{}: expected {expected:?}, got {error}",
            case.name
        );
    }
}

#[test]
fn bounded_custom_rrules_are_authorable_but_unbounded_rules_are_not() {
    let mut fields = FixtureFields {
        item_id: Uuid::from_u128(900),
        kind: CanonicalItemKind::Habit,
        status: CanonicalItemStatus::Planned,
        timezone_name: "Europe/Paris".to_owned(),
        duration_seconds: Some(1_800),
        deadline_at: None,
        earliest_start_at: None,
        recurrence: Some(serde_json::json!({
            "type": "custom",
            "rrule": "BYDAY=MO,WE,FR;COUNT=24;FREQ=WEEKLY"
        })),
        flexible_constraints: serde_json::json!({}),
        split_policy: CanonicalSplitPolicy::Indivisible,
        parent_id: None,
    };
    let validated = validate_scheduling_metadata(fields.input()).unwrap();
    assert_eq!(
        validated.recurrence,
        Some(Recurrence::Custom {
            rrule: "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,WE,FR;COUNT=24".to_owned(),
        })
    );

    fields.recurrence = Some(serde_json::json!({
        "type": "custom",
        "rrule": "FREQ=DAILY;INTERVAL=2"
    }));
    let error = validate_scheduling_metadata(fields.input()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exactly one finite COUNT or UNTIL")
    );
}

#[test]
fn canonical_frequency_anchors_use_the_portable_identity_range() {
    let mut fields = FixtureFields {
        item_id: Uuid::from_u128(901),
        kind: CanonicalItemKind::Habit,
        status: CanonicalItemStatus::Planned,
        timezone_name: "Europe/Paris".to_owned(),
        duration_seconds: Some(1_800),
        deadline_at: None,
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: serde_json::json!({}),
        split_policy: CanonicalSplitPolicy::Indivisible,
        parent_id: None,
    };
    let recurrence = |anchor: &str| {
        serde_json::json!({
            "type": "frequency",
            "target": 1,
            "period": "day",
            "semantics": "rolling",
            "weekdays": [],
            "minimum_spacing": 0,
            "anchor": anchor
        })
    };

    for anchor in [
        "0001-01-01T00:00:00Z",
        "2026-09-04T08:00:00+18:00",
        "9999-12-31T23:59:59.999999Z",
    ] {
        fields.recurrence = Some(recurrence(anchor));
        validate_scheduling_metadata(fields.input())
            .unwrap_or_else(|error| panic!("portable anchor {anchor} failed: {error}"));
    }

    for (anchor, expected) in [
        ("0000-01-01T00:00:00Z", "must use canonical RFC 3339 syntax"),
        (
            "2026-09-04T08:00:00+18:01",
            "must use canonical RFC 3339 syntax",
        ),
    ] {
        fields.recurrence = Some(recurrence(anchor));
        let error = validate_scheduling_metadata(fields.input()).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} for {anchor}, got {error}"
        );
    }
}
