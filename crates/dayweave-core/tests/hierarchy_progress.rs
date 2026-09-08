use std::collections::BTreeMap;

use dayweave_core::{
    HierarchyProgressError, HierarchyProgressEstimate, HierarchyProgressItem,
    HierarchyProgressKind, HierarchyProgressStatus, HierarchyProgressSummary, ItemId,
    MAX_HIERARCHY_PROGRESS_VALUE, roll_up_hierarchy_progress,
};
use serde::Deserialize;
use uuid::Uuid;

fn id(value: u128) -> ItemId {
    ItemId(Uuid::from_u128(value))
}

fn item(value: u128) -> HierarchyProgressItem {
    HierarchyProgressItem {
        id: id(value),
        parent_id: None,
        kind: HierarchyProgressKind::Task,
        status: HierarchyProgressStatus::Planned,
        has_own_effort: false,
        recurs: false,
        has_children_outside_plan: false,
        duration: None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    schema: String,
    cases: Vec<ValidCase>,
    invalid_cases: Vec<InvalidCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidCase {
    name: String,
    items: Vec<HierarchyProgressItem>,
    expected: BTreeMap<ItemId, HierarchyProgressSummary>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvalidCase {
    name: String,
    items: Vec<HierarchyProgressItem>,
    error: String,
}

fn fixtures() -> Fixtures {
    let fixture: Fixtures = serde_json::from_str(include_str!(
        "../../../fixtures/hierarchy-progress/projection-v1.json"
    ))
    .unwrap();
    assert_eq!(fixture.schema, "dayweave.hierarchy-progress-fixtures/1");
    fixture
}

#[test]
fn shared_normalized_fixtures_match_every_subtree_in_both_input_orders() {
    for mut case in fixtures().cases {
        let result = roll_up_hierarchy_progress(&case.items).unwrap();
        assert_eq!(result, case.expected, "{}", case.name);
        assert_eq!(result.len(), case.items.len(), "{}", case.name);
        case.items.reverse();
        assert_eq!(
            roll_up_hierarchy_progress(&case.items).unwrap(),
            case.expected,
            "{} reversed",
            case.name
        );
    }
}

#[test]
fn shared_invalid_fixtures_publish_no_totals_in_either_input_order() {
    for mut case in fixtures().invalid_cases {
        let error = roll_up_hierarchy_progress(&case.items).unwrap_err();
        assert_eq!(error.code(), case.error, "{}", case.name);
        case.items.reverse();
        let error = roll_up_hierarchy_progress(&case.items).unwrap_err();
        assert_eq!(error.code(), case.error, "{} reversed", case.name);
    }
}

#[test]
fn five_thousand_levels_preserve_rich_seconds_and_inherited_recurrence() {
    let mut items: Vec<_> = (1..=5_000)
        .map(|value| {
            let mut item = item(value);
            item.parent_id = (value > 1).then(|| id(value - 1));
            item.kind = HierarchyProgressKind::Project;
            item.status = HierarchyProgressStatus::Completed;
            item.has_own_effort = true;
            item.duration = Some(HierarchyProgressEstimate {
                minimum_seconds: 2,
                expected_seconds: 50,
                maximum_seconds: 90,
            });
            item
        })
        .collect();
    let leaf = items.last_mut().unwrap();
    leaf.kind = HierarchyProgressKind::Task;
    leaf.status = HierarchyProgressStatus::Blocked;
    leaf.duration = Some(HierarchyProgressEstimate {
        minimum_seconds: 1,
        expected_seconds: 30,
        maximum_seconds: 59,
    });
    items.reverse();
    let expected = HierarchyProgressSummary {
        open_leaf_items: 1,
        minimum_estimate_seconds: 1,
        expected_estimate_seconds: 30,
        maximum_estimate_seconds: 59,
        ..HierarchyProgressSummary::default()
    };
    let summaries = roll_up_hierarchy_progress(&items).unwrap();
    assert_eq!(summaries.len(), 5_000);
    assert!(summaries.values().all(|value| *value == expected));
    // Template completion and parent estimates never enter leaf achievement.
    let root = items.last_mut().unwrap();
    root.recurs = true;
    root.kind = HierarchyProgressKind::Routine;
    let recurring = HierarchyProgressSummary {
        recurring_leaf_items: 1,
        ..HierarchyProgressSummary::default()
    };
    assert!(
        roll_up_hierarchy_progress(&items)
            .unwrap()
            .values()
            .all(|value| *value == recurring)
    );
}

#[test]
fn malformed_unrelated_component_withholds_the_entire_forest() {
    let mut good = item(1);
    good.status = HierarchyProgressStatus::Completed;
    let mut bad = item(2);
    bad.has_children_outside_plan = true;
    assert_eq!(
        roll_up_hierarchy_progress(&[good, bad]),
        Err(HierarchyProgressError::IncompleteTopology(id(2)))
    );
}

#[test]
fn cycle_diagnostic_identifies_a_cycle_member_not_its_lower_id_descendant() {
    let mut first = item(20);
    first.parent_id = Some(id(30));
    let mut second = item(30);
    second.parent_id = Some(id(20));
    let mut descendant = item(1);
    descendant.parent_id = Some(id(20));
    assert_eq!(
        roll_up_hierarchy_progress(&[descendant, second, first]),
        Err(HierarchyProgressError::Cycle(id(20)))
    );
}

#[test]
fn known_range_overflow_is_checked_independently_for_expected_and_maximum() {
    for duration in [
        HierarchyProgressEstimate {
            minimum_seconds: 1,
            expected_seconds: MAX_HIERARCHY_PROGRESS_VALUE,
            maximum_seconds: MAX_HIERARCHY_PROGRESS_VALUE,
        },
        HierarchyProgressEstimate {
            minimum_seconds: 1,
            expected_seconds: 1,
            maximum_seconds: MAX_HIERARCHY_PROGRESS_VALUE,
        },
    ] {
        let root = item(1);
        let mut left = item(2);
        left.parent_id = Some(root.id);
        left.duration = Some(duration);
        let mut right = item(3);
        right.parent_id = Some(root.id);
        right.duration = Some(HierarchyProgressEstimate {
            minimum_seconds: 1,
            expected_seconds: 1,
            maximum_seconds: 1,
        });
        assert_eq!(
            roll_up_hierarchy_progress(&[root, left, right]),
            Err(HierarchyProgressError::Overflow(id(1)))
        );
    }
}

#[test]
fn normalized_wire_rejects_unsupported_and_negative_values() {
    let node = serde_json::to_value(item(1)).unwrap();
    for (field, value) in [
        ("kind", serde_json::json!("calendar_event")),
        ("status", serde_json::json!("finished")),
        ("has_own_effort", serde_json::json!(1)),
        ("recurs", serde_json::json!(null)),
        ("unknown_future_field", serde_json::json!(true)),
        (
            "duration",
            serde_json::json!({"minimum_seconds": -1, "expected_seconds": 1, "maximum_seconds": 1}),
        ),
        (
            "duration",
            serde_json::json!({"minimum_seconds": 1, "expected_seconds": 1.5, "maximum_seconds": 2}),
        ),
        (
            "duration",
            serde_json::json!({"minimum_seconds": 1, "expected_seconds": 1, "maximum_seconds": 1, "remaining_seconds": 0}),
        ),
    ] {
        let mut invalid = node.clone();
        invalid[field] = value;
        assert!(
            serde_json::from_value::<HierarchyProgressItem>(invalid).is_err(),
            "{field}"
        );
    }
    let mut event = node;
    event["kind"] = serde_json::json!("event");
    assert_eq!(
        serde_json::from_value::<HierarchyProgressItem>(event)
            .unwrap()
            .kind,
        HierarchyProgressKind::Event
    );
}

#[test]
fn summaries_are_read_only_and_do_not_promote_parent_lifecycle() {
    let mut root = item(1);
    root.kind = HierarchyProgressKind::Goal;
    root.status = HierarchyProgressStatus::Blocked;
    let mut child = item(2);
    child.parent_id = Some(root.id);
    child.status = HierarchyProgressStatus::Completed;
    let items = vec![root, child];
    let before = items.clone();
    let summaries = roll_up_hierarchy_progress(&items).unwrap();
    assert_eq!(items, before);
    assert_eq!(summaries[&id(1)].completed_leaf_items, 1);
    assert_eq!(items[0].status, HierarchyProgressStatus::Blocked);
    assert_eq!(summaries[&id(1)].unknown_estimates, 1);
}
