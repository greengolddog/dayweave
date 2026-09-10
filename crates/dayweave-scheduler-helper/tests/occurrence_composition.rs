use dayweave_compose::{CanonicalItem, ComposeScheduleRequest, prepare_canonical_schedule};
use dayweave_core::{
    ItemId, OccurrenceLifecycleContext, OccurrenceLifecycleInstance, OccurrenceLifecycleMember,
    WorkStatus, expand_occurrences,
};
use dayweave_scheduler_helper::{ProcessOutput, process_bytes};
use serde_json::{Value, json};
use uuid::Uuid;

const V1: &[u8] = include_bytes!("fixtures/compose-request-v1.json");

fn id(value: u128) -> String {
    Uuid::from_u128(value).to_string()
}

fn context(value: &Value) -> OccurrenceLifecycleContext {
    let items: Vec<CanonicalItem> =
        serde_json::from_value(value["request"]["canonical_items"].clone()).unwrap();
    let schedule: ComposeScheduleRequest =
        serde_json::from_value(value["request"]["schedule"].clone()).unwrap();
    let prepared = prepare_canonical_schedule(items.clone(), schedule).unwrap();
    OccurrenceLifecycleContext {
        snapshot_revision: 7,
        instances: expand_occurrences(&prepared.plan_request)
            .unwrap()
            .into_iter()
            .map(|occurrence| OccurrenceLifecycleInstance {
                root_item_id: occurrence.series_item_id,
                occurrence_id: occurrence.id,
                identity: occurrence.identity,
                members: items
                    .iter()
                    .map(|item| OccurrenceLifecycleMember {
                        item_id: ItemId(item.id),
                        parent_id: item.parent_id.map(ItemId),
                        source_revision: item.revision,
                        status: WorkStatus::NotStarted,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn request() -> Value {
    let mut value: Value = serde_json::from_slice(V1).unwrap();
    value["version"] = json!(2);
    let mut root = value["request"]["canonical_items"][0].clone();
    let mut first = root.clone();
    let mut second = root.clone();
    root["kind"] = json!("routine");
    root["recurrence"] = json!({"type":"daily","times_per_day":1});
    root["duration_seconds"] = Value::Null;
    root["is_executable"] = json!(false);
    for (item, number) in [(&mut first, 2), (&mut second, 3)] {
        item["id"] = json!(id(number));
        item["parent_id"] = json!(id(1));
        item["duration_seconds"] = json!(300);
    }
    value["request"]["canonical_items"] = json!([root, first, second]);
    value["request"]["schedule"]["horizon_end"] = json!("2026-09-03T00:00:00Z");
    value["request"]["schedule"]["availability"][0]["end"] = json!("2026-09-03T00:00:00Z");
    value["request"]["occurrence_lifecycle"] = serde_json::to_value(context(&value)).unwrap();
    value
}

fn invoke(value: &Value) -> (ProcessOutput, Value) {
    let output = process_bytes(&serde_json::to_vec(value).unwrap());
    let decoded = serde_json::from_slice(&output.stdout).unwrap();
    (output, decoded)
}

fn composition(value: &Value) -> Value {
    let (output, decoded) = invoke(value);
    assert_eq!(output.exit_code, 0, "{decoded}");
    assert_eq!(decoded["version"], json!(2));
    decoded["result"]["composition"].clone()
}

fn reject(value: &Value, code: &str) {
    let (output, decoded) = invoke(value);
    assert_eq!(output.exit_code, 2, "{decoded}");
    assert_eq!(decoded["result"]["error"]["code"], json!(code));
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("Golden task")
    );
}

#[test]
fn v2_empty_context_preserves_plan_but_uses_its_own_fingerprint_domain() {
    let mut value: Value = serde_json::from_slice(V1).unwrap();
    let (_, legacy) = invoke(&value);
    value["version"] = json!(2);
    value["request"]["occurrence_lifecycle"] = json!({"snapshot_revision":0,"instances":[]});
    let modern = composition(&value);
    assert_eq!(modern["plan"], legacy["result"]["composition"]["plan"]);
    assert_eq!(modern["occurrence_snapshot_revision"], json!(0));
    assert_ne!(
        modern["local_input_fingerprint"],
        legacy["result"]["composition"]["local_input_fingerprint"]
    );
    value["request"]["occurrence_lifecycle"]["snapshot_revision"] = json!(1);
    assert_ne!(
        modern["local_input_fingerprint"],
        composition(&value)["local_input_fingerprint"]
    );
}

#[test]
fn done_members_are_exact_to_one_instance_and_parent_done_keeps_optional_demand() {
    let mut value = request();
    let before = composition(&value);
    assert_eq!(before["plan"]["blocks"].as_array().unwrap().len(), 4);
    let instance = &mut value["request"]["occurrence_lifecycle"]["instances"][0];
    let occurrence = instance["occurrence_id"].clone();
    instance["members"][0]["status"] = json!("completed");
    instance["members"][1]["status"] = json!("completed");
    let after = composition(&value);
    let blocks = after["plan"]["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert!(
        !blocks
            .iter()
            .any(|block| block["item_id"] == id(2) && block["occurrence_id"] == occurrence)
    );
    assert!(
        blocks
            .iter()
            .any(|block| block["item_id"] == id(3) && block["occurrence_id"] == occurrence)
    );
    assert_ne!(
        before["local_input_fingerprint"],
        after["local_input_fingerprint"]
    );
}

#[test]
fn managed_whole_completion_and_partial_claims_never_override_member_authority() {
    let mut value = request();
    let expected = composition(&value);
    let occurrence = value["request"]["occurrence_lifecycle"]["instances"][0]["occurrence_id"]
        .as_str()
        .unwrap()
        .to_owned();
    value["request"]["schedule"]["recurrence_context"]["completed_occurrence_ids"] =
        json!([occurrence]);
    value["request"]["schedule"]["recurrence_context"]["partial_progress"] = json!({occurrence:{"progress_basis_points":5000,"expected_duration_minutes":5,"remaining_duration_minutes":3}});
    assert_eq!(composition(&value), expected);
}

#[test]
fn input_order_does_not_change_normalized_v2_output_or_fingerprint() {
    let mut value = request();
    let expected = composition(&value);
    value["request"]["canonical_items"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let instances = value["request"]["occurrence_lifecycle"]["instances"]
        .as_array_mut()
        .unwrap();
    instances.reverse();
    for instance in instances {
        instance["members"].as_array_mut().unwrap().reverse();
    }
    assert_eq!(composition(&value), expected);
}

#[test]
fn omitted_inbox_members_still_require_exact_current_sources_and_complete_membership() {
    let mut value = request();
    value["request"]["canonical_items"][2]["status"] = json!("inbox");
    assert_eq!(
        composition(&value)["plan"]["blocks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let mut stale = value.clone();
    stale["request"]["canonical_items"][2]["revision"] = json!(2);
    reject(&stale, "invalid_request");
    let mut omitted = value.clone();
    for instance in omitted["request"]["occurrence_lifecycle"]["instances"]
        .as_array_mut()
        .unwrap()
    {
        instance["members"].as_array_mut().unwrap().pop();
    }
    reject(&omitted, "invalid_request");
    value["request"]["canonical_items"][2]["revision"] = json!(2);
    for instance in value["request"]["occurrence_lifecycle"]["instances"]
        .as_array_mut()
        .unwrap()
    {
        instance["members"][2]["source_revision"] = json!(2);
    }
    composition(&value);
}

#[test]
fn v2_is_explicit_and_closed_without_changing_the_v1_request_contract() {
    let value = request();
    let mut old = value.clone();
    old["version"] = json!(1);
    reject(&old, "invalid_request");
    let mut absent = value.clone();
    absent["request"]
        .as_object_mut()
        .unwrap()
        .remove("occurrence_lifecycle");
    reject(&absent, "invalid_request");
    let mut extra = value.clone();
    extra["request"]["execution"] = json!({});
    reject(&extra, "invalid_request");
    let mut unknown = value.clone();
    unknown["request"]["occurrence_lifecycle"]["private_future"] = json!(true);
    reject(&unknown, "invalid_request");
    let mut parent = value.clone();
    parent["request"]["occurrence_lifecycle"]["instances"][0]["members"][0]
        .as_object_mut()
        .unwrap()
        .remove("parent_id");
    reject(&parent, "invalid_request");
    let mut fractional = value.clone();
    fractional["request"]["occurrence_lifecycle"]["snapshot_revision"] = json!(7.0);
    reject(&fractional, "invalid_request");
    let mut plan = value;
    plan["operation"] = json!("plan");
    reject(&plan, "unsupported_operation");
}

#[test]
fn invalid_exact_identity_revision_lifecycle_and_bounds_fail_closed() {
    let value = request();
    for field in ["root_item_id", "occurrence_id"] {
        let mut malformed = value.clone();
        malformed["request"]["occurrence_lifecycle"]["instances"][0][field] = json!(id(88));
        reject(&malformed, "invalid_request");
    }
    for status in ["active", "paused"] {
        let mut malformed = value.clone();
        malformed["request"]["occurrence_lifecycle"]["instances"][0]["members"][1]["status"] =
            json!(status);
        reject(&malformed, "invalid_request");
    }
    for revision in [0, u64::MAX] {
        let mut malformed = value.clone();
        malformed["request"]["occurrence_lifecycle"]["snapshot_revision"] = json!(revision);
        reject(&malformed, "invalid_request");
    }
    let mut oversized = value;
    let member = oversized["request"]["occurrence_lifecycle"]["instances"][0]["members"][0].clone();
    oversized["request"]["occurrence_lifecycle"]["instances"][0]["members"] =
        Value::Array(vec![member; 10_001]);
    reject(&oversized, "resource_limit_exceeded");
}

#[test]
fn five_thousand_level_occurrence_uses_the_same_bounded_small_stack_bridge() {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let mut value = request();
            value["request"]["schedule"]["horizon_end"] = json!("2026-09-02T00:00:00Z");
            value["request"]["schedule"]["availability"][0]["end"] = json!("2026-09-02T00:00:00Z");
            let root = value["request"]["canonical_items"][0].clone();
            let leaf = value["request"]["canonical_items"][1].clone();
            let mut items = vec![root];
            for number in 2..=5_000 {
                let mut item = leaf.clone();
                item["id"] = json!(id(number));
                item["parent_id"] = json!(id(number - 1));
                if number != 5_000 {
                    item["kind"] = json!("routine");
                    item["duration_seconds"] = Value::Null;
                    item["is_executable"] = json!(false);
                }
                items.push(item);
            }
            value["request"]["canonical_items"] = Value::Array(items);
            value["request"]["occurrence_lifecycle"] =
                serde_json::to_value(context(&value)).unwrap();
            let before = composition(&value);
            assert_eq!(before["plan"]["blocks"].as_array().unwrap().len(), 1);
            assert_eq!(before["plan"]["blocks"][0]["item_id"], json!(id(5_000)));
            for member in value["request"]["occurrence_lifecycle"]["instances"][0]["members"]
                .as_array_mut()
                .unwrap()
            {
                member["status"] = json!("completed");
            }
            assert!(
                composition(&value)["plan"]["blocks"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn v2_body_rejections_keep_v2_framing_but_unreadable_envelopes_use_fixed_v1_errors() {
    let mut value = request();
    value["request"]["occurrence_lifecycle"]["snapshot_revision"] = json!(-1);
    assert_eq!(invoke(&value).1["version"], json!(2));
    let duplicate = br#"{"protocol":"dayweave.scheduler.helper","version":2,"operation":"compose","request":{"occurrence_lifecycle":{"snapshot_revision":1,"snapshot_revision":2}}}"#;
    let output = process_bytes(duplicate);
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.exit_code, 2);
    assert_eq!(error["version"], json!(1));
    assert_eq!(
        error["result"]["error"]["code"],
        json!("duplicate_json_key")
    );
}
