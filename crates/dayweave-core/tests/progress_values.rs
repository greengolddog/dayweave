use dayweave_core::{format_progress_decimal, is_valid_progress_label, parse_progress_decimal};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    schema: String,
    decimals: Vec<DecimalCase>,
    labels: Vec<LabelCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecimalCase {
    value: String,
    valid: bool,
    scaled_millionths: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelCase {
    value: String,
    max_scalars: usize,
    valid: bool,
}

#[test]
fn native_scalar_fixture_matches_exact_shared_core_values() {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../fixtures/item-progress/values-v1.json"
    ))
    .expect("valid synthetic fixture");
    assert_eq!(fixture.schema, "dayweave.item-progress-values/1");
    assert_eq!(fixture.decimals.len(), 37);
    assert_eq!(fixture.labels.len(), 21);
    for case in fixture.decimals {
        let actual = parse_progress_decimal(&case.value);
        assert_eq!(actual.is_ok(), case.valid, "{:?}", case.value);
        if case.valid {
            let scaled = case
                .scaled_millionths
                .as_deref()
                .expect("valid cases have exact string-encoded expectations")
                .parse::<i64>()
                .expect("signed portable integer");
            assert_eq!(actual, Ok(scaled));
            assert_eq!(
                format_progress_decimal(scaled).as_deref(),
                Some(case.value.as_str())
            );
        } else {
            assert!(case.scaled_millionths.is_none());
        }
    }
    for case in fixture.labels {
        assert_eq!(
            is_valid_progress_label(&case.value, case.max_scalars),
            case.valid
        );
    }
}
