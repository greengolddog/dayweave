use dayweave_scheduler_helper::{REJECTED_EXIT_CODE, SUCCESS_EXIT_CODE, process_bytes};
use serde_json::{Value, json};
use uuid::Uuid;

const COMPOSE_REQUEST: &[u8] = include_bytes!("fixtures/compose-request-v1.json");
const ROOT_ID: u128 = 1;
const LEAF_ID: u128 = 2;

fn routine_chain(item_count: usize, occurrences_per_day: u16) -> Value {
    assert!(item_count >= 2);
    let mut request: Value = serde_json::from_slice(COMPOSE_REQUEST).unwrap();
    let template = request["request"]["canonical_items"][0].clone();
    let ids: Vec<_> = std::iter::once(Uuid::from_u128(ROOT_ID))
        .chain((3..=u128::try_from(item_count).unwrap()).map(Uuid::from_u128))
        .chain(std::iter::once(Uuid::from_u128(LEAF_ID)))
        .collect();
    assert_eq!(ids.len(), item_count);
    let mut items: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let mut item = template.clone();
            let is_leaf = index + 1 == item_count;
            item["id"] = json!(id);
            item["parent_id"] = index
                .checked_sub(1)
                .map_or(Value::Null, |parent| json!(ids[parent]));
            item["kind"] = json!(if is_leaf { "task" } else { "routine" });
            item["title"] = json!(if is_leaf {
                "Only actionable step"
            } else {
                "Structural routine"
            });
            item["is_executable"] = json!(is_leaf);
            item["duration_seconds"] = if is_leaf { json!(1_800) } else { Value::Null };
            item["flexible_constraints"] = if is_leaf {
                json!({})
            } else {
                json!({"has_own_effort": false, "routine_ordered": true})
            };
            item["recurrence"] = if index == 0 {
                json!({"type": "daily", "times_per_day": occurrences_per_day})
            } else {
                Value::Null
            };
            item
        })
        .collect();
    // Input order must not provide a convenient parent-first traversal.
    items.reverse();
    request["request"]["canonical_items"] = json!(items);
    request
}

fn composition(request: &Value) -> Value {
    let output = process_bytes(&serde_json::to_vec(request).unwrap());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.exit_code, SUCCESS_EXIT_CODE, "{response}");
    assert_eq!(response["result"]["type"], "composition");
    response["result"]["composition"].clone()
}

#[test]
fn five_thousand_level_recurring_routine_preserves_one_leaf_schedule_on_a_bounded_stack() {
    let shallow = composition(&routine_chain(8, 1));
    let deep = std::thread::Builder::new()
        .name("deep-routine-compose".into())
        .stack_size(512 * 1024)
        .spawn(|| composition(&routine_chain(5_000, 1)))
        .unwrap()
        .join()
        .unwrap();

    assert_eq!(deep["source_item_count"], 5_000);
    assert_eq!(deep["accepted_item_count"], 5_000);
    assert_eq!(
        deep["source_item_revisions"].as_object().unwrap().len(),
        5_000
    );
    assert_eq!(deep["rejected_items"], json!([]));
    assert_eq!(deep["ignored_previous_assignments"], json!([]));
    for field in [
        "as_of",
        "horizon_start",
        "horizon_end",
        "blocks",
        "unscheduled",
        "violations",
        "score",
        "occurrences",
    ] {
        assert_eq!(deep["plan"][field], shallow["plan"][field], "{field}");
    }
    let blocks = deep["plan"]["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["item_id"], json!(Uuid::from_u128(LEAF_ID)));
    assert_eq!(blocks[0]["start"], "2026-09-01T08:00:00Z");
    assert_eq!(blocks[0]["end"], "2026-09-01T08:30:00Z");
    let occurrences = deep["plan"]["occurrences"].as_array().unwrap();
    assert_eq!(occurrences.len(), 1);
    assert_eq!(
        occurrences[0]["series_item_id"],
        json!(Uuid::from_u128(ROOT_ID))
    );
    assert_eq!(blocks[0]["occurrence_id"], occurrences[0]["id"]);
    assert_eq!(deep["plan"]["decisions"].as_array().unwrap().len(), 5_000);
}

#[test]
fn deep_structural_chain_still_rejects_aggregate_recurrence_materialization_over_budget() {
    let request = routine_chain(5_000, 3);
    let output = process_bytes(&serde_json::to_vec(&request).unwrap());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.exit_code, REJECTED_EXIT_CODE);
    assert_eq!(response["result"]["type"], "error");
    assert_eq!(
        response["result"]["error"]["code"],
        "resource_limit_exceeded"
    );
    assert!(response["result"].get("composition").is_none());
}

#[test]
fn wide_executable_forest_still_rejects_quadratic_ordering_over_budget() {
    let mut request: Value = serde_json::from_slice(COMPOSE_REQUEST).unwrap();
    let template = request["request"]["canonical_items"][0].clone();
    let items: Vec<_> = (1..=4_000_u128)
        .map(|id| {
            let mut item = template.clone();
            item["id"] = json!(Uuid::from_u128(id));
            item
        })
        .collect();
    request["request"]["canonical_items"] = json!(items);
    // No candidate slots or busy-window scans: the retained pairwise executable
    // ordering budget must independently reject this otherwise bounded forest.
    request["request"]["schedule"]["availability"] = json!([]);
    let output = process_bytes(&serde_json::to_vec(&request).unwrap());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.exit_code, REJECTED_EXIT_CODE);
    assert_eq!(response["result"]["type"], "error");
    assert_eq!(
        response["result"]["error"]["code"],
        "resource_limit_exceeded"
    );
    assert!(response["result"].get("composition").is_none());
}
