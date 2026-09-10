//! Synthetic cross-native contract emitted by the real normalization/helper producers.
//! Authentication and database capture remain covered by the independent HTTP/PG gates.
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use dayweave_core::{
    DayOfWeek, ItemId, Minutes, OccurrenceLifecycleInstance, OccurrenceLifecycleMember,
    RecurrenceCalendar, RecurrencePartialProgress, WorkStatus, ZonedDayBoundary,
    expand_occurrences,
};
use uuid::Uuid;

use crate::items::{Item, NewItem};

const FIXTURE: &str = include_str!("../../../../fixtures/routine-planning-witness/wire-v1.json");
const BASE_NAME: &str = "two_instances_current_sources_habit_calendar";

fn instant(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn scope() -> DatabaseScope {
    DatabaseScope {
        workspace_id: Uuid::from_u128(500),
        user_id: Uuid::from_u128(501),
    }
}

fn base_schedule() -> ComposeScheduleRequest {
    let mut request: ComposeScheduleRequest = serde_json::from_value(json!({
        "as_of":"2026-09-10T09:00:00.123456Z", "horizon_start":"2026-09-10T00:00:00Z",
        "horizon_end":"2026-09-12T00:00:00Z", "timezone_name":"UTC",
        "availability":[
            {"start":"2026-09-10T09:00:00Z","end":"2026-09-10T18:00:00Z","contexts":[],"location":null,"energy":"deep"},
            {"start":"2026-09-11T09:00:00Z","end":"2026-09-11T18:00:00Z","contexts":[],"location":null,"energy":"deep"}],
        "config":{"slot_granularity_minutes":5,"stability_weight":4,"default_soft_weight":100}
    })).unwrap();
    request.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("UTC".into()),
        week_starts_on: DayOfWeek::Monday,
        days: (0..2)
            .map(|offset| {
                let start = request.horizon_start + Duration::days(offset);
                ZonedDayBoundary {
                    local_date: time::Date::from_calendar_date(
                        2026,
                        time::Month::September,
                        u8::try_from(10 + offset).unwrap(),
                    )
                    .unwrap(),
                    start: time::OffsetDateTime::from_unix_timestamp(start.timestamp()).unwrap(),
                    end: time::OffsetDateTime::from_unix_timestamp(
                        (start + Duration::days(1)).timestamp(),
                    )
                    .unwrap(),
                }
            })
            .collect(),
    };
    request
}

fn item(number: u128, kind: &str, parent: Option<u128>, status: &str) -> Item {
    let duration =
        (!matches!(kind, "routine" | "project" | "event") && number != 20).then_some(600);
    let mut value = json!({"id":Uuid::from_u128(number),"is_sensitive":true,
        "kind":kind,"status":status,"title":format!("Synthetic witness source {number}"),
        "notes":null,"timezone_name":"UTC","duration_seconds":duration,
        "deadline_at":null,"earliest_start_at":null,"recurrence":null,
        "flexible_constraints":{},"split_policy":{"type":"indivisible"},
        "importance":50,"urgency":50,"parent_id":parent.map(Uuid::from_u128),"sibling_order":number});
    if matches!(kind, "routine" | "habit") {
        value["recurrence"] = json!({"type":"daily","times_per_day":1});
    }
    if status == "blocked" {
        value["blocked_reason_kind"] = json!("manual");
        value["blocked_reason"] = json!("Synthetic waiting for input");
    }
    if kind == "event" {
        value["flexible_constraints"] = json!({"calendar_event":{
            "start":"2026-09-10T10:00:00Z","end":"2026-09-10T10:45:00Z",
            "immutable":true,"all_day":false,"source_calendar_id":null}});
    }
    let input: NewItem = serde_json::from_value(value).unwrap();
    let mut result = Item::new(input, instant("2026-09-01T00:00:00.123456Z")).unwrap();
    result.revision = 2 + u64::try_from(number / 10).unwrap();
    result.updated_at = instant("2026-09-09T14:30:00.654321Z");
    if matches!(number, 1 | 10 | 20) {
        result.is_executable = false;
    }
    result
}

fn sources(recurring: bool) -> Vec<Item> {
    if !recurring {
        return vec![
            item(30, "task", None, "planned"),
            item(50, "task", None, "inbox"),
        ];
    }
    vec![
        item(1, "project", None, "planned"),
        item(10, "routine", Some(1), "planned"),
        item(20, "task", Some(10), "planned"),
        item(30, "task", Some(20), "planned"),
        item(40, "task", Some(10), "planned"),
        item(50, "task", Some(20), "inbox"),
        item(60, "task", Some(10), "blocked"),
        item(70, "task", Some(10), "planned"),
        item(80, "habit", None, "planned"),
        item(90, "event", None, "planned"),
    ]
}

#[allow(clippy::too_many_lines)] // Keep the deterministic producer chain next to its captured authorities.
fn qualified_case(recurring: bool) -> Value {
    let items = sources(recurring);
    let canonical: Vec<_> = items.iter().cloned().map(into_canonical_item).collect();
    let mut original = base_schedule();
    let prepared = prepare_canonical_schedule(canonical.clone(), original.clone()).unwrap();
    let generated = expand_occurrences(&prepared.plan_request).unwrap();
    let mut lifecycle = OccurrenceLifecycleContext {
        snapshot_revision: 17,
        instances: Vec::new(),
    };
    for (index, occurrence) in generated
        .iter()
        .filter(|value| value.series_item_id.0 == Uuid::from_u128(10))
        .enumerate()
    {
        let members = canonical
            .iter()
            .filter(|item| (10..=70).contains(&item.id.as_u128()))
            .map(|item| {
                let number = item.id.as_u128();
                let status = match (index, number) {
                    (0, 10 | 20 | 30) => WorkStatus::Completed,
                    (0, 70) => WorkStatus::Skipped,
                    (_, 60) => WorkStatus::Blocked,
                    _ => WorkStatus::NotStarted,
                };
                OccurrenceLifecycleMember {
                    item_id: ItemId(item.id),
                    parent_id: if number == 10 {
                        None
                    } else {
                        item.parent_id.map(ItemId)
                    },
                    source_revision: item.revision,
                    status,
                }
            })
            .collect();
        lifecycle.instances.push(OccurrenceLifecycleInstance {
            root_item_id: occurrence.series_item_id,
            occurrence_id: occurrence.id,
            identity: occurrence.identity,
            members,
        });
    }
    lifecycle.validate().unwrap();
    let mut habit = AuthoritativeHabitRecurrence::default();
    if recurring {
        habit.change_head = 23;
        let habit_ids = generated
            .iter()
            .filter(|value| value.series_item_id.0 == Uuid::from_u128(80))
            .map(|value| value.id)
            .collect::<Vec<_>>();
        assert_eq!(habit_ids.len(), 2);
        habit.context.completed_occurrence_ids.insert(habit_ids[0]);
        original
            .recurrence_context
            .completed_occurrence_ids
            .extend(habit_ids);
        let first = lifecycle.instances[0].occurrence_id;
        original
            .recurrence_context
            .completed_occurrence_ids
            .insert(first);
        original.recurrence_context.partial_progress.insert(
            first,
            RecurrencePartialProgress {
                progress_basis_points: 9_999,
                expected_duration_minutes: Minutes(10),
                remaining_duration_minutes: Some(Minutes(1)),
            },
        );
    }
    let mut planning = AuthoritativePlanningEvidence {
        published_revision_id: Some(Uuid::from_u128(600)),
        ..AuthoritativePlanningEvidence::default()
    };
    if recurring {
        planning.previous_assignments = serde_json::from_value(json!([{
            "item_id":Uuid::from_u128(40),"item_revision":canonical.iter().find(|item|item.id==Uuid::from_u128(40)).unwrap().revision,
            "occurrence_id":lifecycle.instances[0].occurrence_id,"pinned":false,
            "blocks":[{"start":"2026-09-10T10:00:00Z","end":"2026-09-10T10:10:00Z","session_index":0}]}])).unwrap();
    }
    let calendar = if recurring {
        vec![CalendarProjectionStamp {
            collection_id: Uuid::from_u128(700),
            collection_revision: 4,
            generation: 9,
            window_start: original.horizon_start,
            window_end: original.horizon_end,
            refreshed_at: original.as_of - Duration::minutes(1),
        }]
    } else {
        Vec::new()
    };
    // This synthetic terminal is byte-for-byte in the repository encoder's
    // closed workspace/list_head/after form. It is not an authentication claim.
    let cursor = format!(
        "DWR1.{}",
        URL_SAFE_NO_PAD.encode(format!(
            "{{\"workspace\":\"{}\",\"list_head\":null,\"after\":17}}",
            scope().workspace_id
        ))
    );
    let request = RoutinePlanningWitnessRequest {
        schema_version: 1,
        schedule: original,
        expected_source_item_revisions: canonical
            .iter()
            .map(|item| (item.id, item.revision))
            .collect(),
        terminal_cursor: cursor,
    };
    request.validate().unwrap();
    let mut normalized = request.schedule.clone();
    let normalization =
        normalize_authoritative_schedule_request(&mut normalized, &items, Some(&habit), &planning)
            .unwrap();
    discard_managed_occurrence_claims(&mut normalized, &lifecycle);
    let composed = compose_items_with_lifecycle_for_schema(
        items,
        normalized.clone(),
        publication_schema_for_lifecycle(&lifecycle),
        calendar.clone(),
        planning.clone(),
        habit.change_head,
        normalization.untrusted_assignments,
        lifecycle.clone(),
    )
    .unwrap();
    let local = require_helper_parity(&canonical, &normalized, &lifecycle, &composed).unwrap();
    let witness = make_witness(
        scope(),
        &request,
        normalized.clone(),
        lifecycle.clone(),
        local,
        &canonical,
        &planning,
        &habit,
        &calendar,
        &composed,
    )
    .unwrap();
    let helper_request = json!({"protocol":"dayweave.scheduler.helper","version":2,"operation":"compose",
        "request":{"canonical_items":canonical,"schedule":normalized,"occurrence_lifecycle":lifecycle}});
    let output =
        dayweave_scheduler_helper::process_bytes(&serde_json::to_vec(&helper_request).unwrap());
    assert_eq!(output.exit_code, 0);
    let helper_response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let response = RoutinePlanningWitnessResponse {
        schema_version: 1,
        result: RoutinePlanningWitnessResult::Qualified {
            witness: Box::new(witness),
        },
    };
    response.validate_size().unwrap();
    json!({"name":if recurring {BASE_NAME} else {"positive_head_without_current_instances"},
        "canonical_items":canonical,"request":request,"response":response,
        "helper_request":helper_request,"helper_response":helper_response})
}

fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    })
}

fn fingerprint_shape(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[allow(clippy::too_many_lines)] // Independent portable-response admission complements the actual producer comparison.
fn admits(case: &Value, response: &Value) -> bool {
    if !exact_keys(response, &["schema_version", "result"])
        || !exact_keys(&response["result"], &["status", "witness"])
    {
        return false;
    }
    let raw = &response["result"]["witness"];
    if !exact_keys(
        raw,
        &[
            "workspace_id",
            "user_id",
            "request_fingerprint",
            "witness_fingerprint",
            "local_input_fingerprint",
            "calendar_projection_fingerprint",
            "source_item_revisions",
            "terminal_cursor",
            "schedule",
            "occurrence_lifecycle",
            "execution_snapshot_revision",
            "habit_change_head",
            "published_schedule_revision_id",
        ],
    ) {
        return false;
    }
    if !exact_keys(
        &raw["occurrence_lifecycle"],
        &["snapshot_revision", "instances"],
    ) {
        return false;
    }
    let Some(instances) = raw["occurrence_lifecycle"]["instances"].as_array() else {
        return false;
    };
    for instance in instances {
        if !exact_keys(
            instance,
            &["root_item_id", "occurrence_id", "identity", "members"],
        ) {
            return false;
        }
        let Some(members) = instance["members"].as_array() else {
            return false;
        };
        if members.iter().any(|member| {
            !exact_keys(
                member,
                &["item_id", "parent_id", "source_revision", "status"],
            )
        }) {
            return false;
        }
    }
    let Some(revisions) = raw["source_item_revisions"].as_object() else {
        return false;
    };
    let identities = revisions
        .keys()
        .map(|key| Uuid::parse_str(key))
        .collect::<Result<BTreeSet<_>, _>>();
    if identities.is_err() || identities.unwrap().len() != revisions.len() {
        return false;
    }
    let Ok(response) = serde_json::from_value::<RoutinePlanningWitnessResponse>(response.clone())
    else {
        return false;
    };
    if response.schema_version != 1 || response.validate_size().is_err() {
        return false;
    }
    let RoutinePlanningWitnessResult::Qualified { witness } = response.result else {
        return false;
    };
    let request: RoutinePlanningWitnessRequest =
        serde_json::from_value(case["request"].clone()).unwrap();
    if witness.workspace_id != scope().workspace_id
        || witness.user_id != scope().user_id
        || witness.terminal_cursor != request.terminal_cursor
        || witness.source_item_revisions != request.expected_source_item_revisions
        || witness.execution_snapshot_revision > i64::MAX as u64
        || witness.habit_change_head > i64::MAX as u64
        || witness
            .published_schedule_revision_id
            .is_some_and(|id| id.is_nil())
        || witness.occurrence_lifecycle.validate().is_err()
    {
        return false;
    }
    for (value, prefix) in [
        (
            &witness.request_fingerprint,
            "routine-witness-request-sha256:",
        ),
        (
            &witness.witness_fingerprint,
            "routine-witness-capture-sha256:",
        ),
        (
            &witness.calendar_projection_fingerprint,
            "routine-witness-calendar-sha256:",
        ),
        (&witness.local_input_fingerprint, "local-sha256:"),
    ] {
        if !fingerprint_shape(value, prefix) {
            return false;
        }
    }
    let mut paired = serde_json::to_value(&witness.schedule).unwrap();
    let mut original = serde_json::to_value(&request.schedule).unwrap();
    for value in [&mut paired, &mut original] {
        value
            .as_object_mut()
            .unwrap()
            .remove("previous_assignments");
        let recurrence = value["recurrence_context"].as_object_mut().unwrap();
        for key in [
            "completion_anchors",
            "pauses",
            "exceptions",
            "completed_occurrence_ids",
            "partial_progress",
        ] {
            recurrence.remove(key);
        }
    }
    if paired != original
        || !witness.schedule.manual_placements.is_empty()
        || !witness.schedule.manual_placement_releases.is_empty()
    {
        return false;
    }
    let input = json!({"protocol":"dayweave.scheduler.helper","version":2,"operation":"compose",
        "request":{"canonical_items":case["canonical_items"],"schedule":witness.schedule,"occurrence_lifecycle":witness.occurrence_lifecycle}});
    let output = dayweave_scheduler_helper::process_bytes(&serde_json::to_vec(&input).unwrap());
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        return false;
    };
    output.exit_code == 0
        && value["result"]["composition"]["local_input_fingerprint"]
            == witness.local_input_fingerprint
}

#[allow(clippy::too_many_lines)] // Keep named negative mutations together for corpus review.
fn corpus() -> Value {
    let base = qualified_case(true);
    let mut invalid = Vec::new();
    for (name, pointer, value) in [
        ("wrong_version", "/schema_version", json!(2)),
        (
            "foreign_workspace",
            "/result/witness/workspace_id",
            json!(Uuid::from_u128(900)),
        ),
        (
            "foreign_user",
            "/result/witness/user_id",
            json!(Uuid::from_u128(901)),
        ),
        (
            "foreign_cursor",
            "/result/witness/terminal_cursor",
            json!("DWR1.synthetic-other"),
        ),
        (
            "invalid_local_fingerprint",
            "/result/witness/local_input_fingerprint",
            json!("local-sha256:not-a-digest"),
        ),
        (
            "publication_digest_is_not_witness",
            "/result/witness/witness_fingerprint",
            json!(format!("sha256:{}", "01".repeat(32))),
        ),
        (
            "negative_habit_head",
            "/result/witness/habit_change_head",
            json!(-1),
        ),
        (
            "fractional_execution_revision",
            "/result/witness/execution_snapshot_revision",
            json!(1.0),
        ),
        (
            "zero_head_with_instances",
            "/result/witness/occurrence_lifecycle/snapshot_revision",
            json!(0),
        ),
        (
            "active_member_without_execution",
            "/result/witness/occurrence_lifecycle/instances/0/members/0/status",
            json!("active"),
        ),
        (
            "stale_member_revision",
            "/result/witness/occurrence_lifecycle/instances/0/members/0/source_revision",
            json!(1),
        ),
        (
            "changed_config",
            "/result/witness/schedule/config/stability_weight",
            json!(9),
        ),
    ] {
        let mut response = base["response"].clone();
        *response.pointer_mut(pointer).unwrap() = value;
        invalid.push(json!({"name":name,"base":BASE_NAME,"response":response}));
    }
    for name in [
        "missing_nullable_parent",
        "omitted_inbox_member",
        "duplicate_instance",
        "unknown_witness_field",
        "source_uuid_alias",
    ] {
        let mut response = base["response"].clone();
        let witness = &mut response["result"]["witness"];
        match name {
            "missing_nullable_parent" => {
                witness["occurrence_lifecycle"]["instances"][0]["members"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("parent_id");
            }
            "omitted_inbox_member" => {
                witness["occurrence_lifecycle"]["instances"][0]["members"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|member| member["item_id"] != json!(Uuid::from_u128(50)));
            }
            "duplicate_instance" => {
                let copy = witness["occurrence_lifecycle"]["instances"][0].clone();
                witness["occurrence_lifecycle"]["instances"]
                    .as_array_mut()
                    .unwrap()
                    .push(copy);
            }
            "unknown_witness_field" => {
                witness["future_private_field"] = json!("Synthetic private future field");
            }
            _ => {
                witness["source_item_revisions"]
                    .as_object_mut()
                    .unwrap()
                    .insert(Uuid::from_u128(10).simple().to_string(), json!(3));
            }
        }
        invalid.push(json!({"name":name,"base":BASE_NAME,"response":response}));
    }
    let original = serde_json::to_string(&base["response"]).unwrap();
    let raw_invalid = [
        (
            "duplicate_schema_key",
            original.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            ),
        ),
        (
            "escaped_duplicate_schema_key",
            original.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_vers\\u0069on\":1",
                1,
            ),
        ),
        (
            "exponent_habit_head",
            original.replacen("\"habit_change_head\":23", "\"habit_change_head\":23e0", 1),
        ),
        ("trailing_json", format!("{original}{{}}")),
    ]
    .into_iter()
    .map(|(name, raw)| json!({"name":name,"base":BASE_NAME,"raw":raw}))
    .collect::<Vec<_>>();
    let remote = [
        RemoteReason::FirstPublicationRequired,
        RemoteReason::ExecutionEvidenceRequired,
        RemoteReason::RetainedManualPlacementRequired,
        RemoteReason::SourceIneligible,
        RemoteReason::CalendarProjectionIncomplete,
        RemoteReason::CompositionUnsupported,
    ]
    .into_iter()
    .map(|reason| {
        let name = serde_json::to_value(reason).unwrap();
        json!({"name":name,"response":RoutinePlanningWitnessResponse {schema_version:1,
                result:RoutinePlanningWitnessResult::RemoteRequired {reason}}})
    })
    .collect::<Vec<_>>();
    json!({"schema_version":1,"qualified":[base,qualified_case(false)],"remote_required":remote,
        "invalid":invalid,"raw_invalid":raw_invalid})
}

#[test]
fn routine_planning_witness_shared_fixture_matches_real_producers() {
    let actual = corpus();
    let retained: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(
        actual, retained,
        "regenerate only through the explicit synthetic producer command"
    );
    for case in actual["qualified"].as_array().unwrap() {
        assert!(admits(case, &case["response"]), "{}", case["name"]);
    }
}

#[test]
fn routine_planning_witness_shared_fixture_rejects_semantic_and_raw_mismatches() {
    let value = corpus();
    let base = &value["qualified"][0];
    for case in value["invalid"].as_array().unwrap() {
        assert!(!admits(base, &case["response"]), "{}", case["name"]);
    }
    for case in value["raw_invalid"].as_array().unwrap() {
        let decoded = dayweave_scheduler_helper::decode_bounded_json(
            case["raw"].as_str().unwrap().as_bytes(),
        );
        assert!(
            !decoded.is_ok_and(|response| admits(base, &response)),
            "{}",
            case["name"]
        );
    }
}

#[test]
#[ignore = "maintenance-only deterministic synthetic fixture regeneration"]
fn emit_shared_routine_planning_witness_fixture() {
    let value = corpus();
    for case in value["qualified"].as_array().unwrap() {
        assert!(admits(case, &case["response"]));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/routine-planning-witness/wire-v1.json");
    let mut bytes = serde_json::to_vec_pretty(&value).unwrap();
    bytes.push(b'\n');
    std::fs::write(path, bytes).unwrap();
}
