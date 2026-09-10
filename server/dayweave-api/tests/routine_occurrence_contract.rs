//! Shared native wire fixtures are emitted from real recurrence expansion and
//! the pure occurrence planner. Response serde alone is not semantic admission.
use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use dayweave_api::{
    item_completion::{ItemCompletionMode, ItemCompletionReopenState},
    items::{BlockedReasonKind, ItemKind, ItemStatus},
    persistence::{RoutineOccurrenceChange, RoutineOccurrenceMutation, RoutineOccurrencePage},
    routine_occurrences::{
        MAX_ROUTINE_OCCURRENCE_BYTES, MAX_ROUTINE_OCCURRENCE_MEMBERS, RoutineOccurrenceAction,
        RoutineOccurrenceAggregate, RoutineOccurrenceCommand, RoutineOccurrenceError,
        RoutineOccurrenceEvidence, RoutineOccurrenceManifest, RoutineOccurrenceMemberDefinition,
        RoutineOccurrenceSnapshot, RoutineOccurrenceSourceEvidence, initialize_routine_occurrence,
        plan_routine_occurrence, routine_occurrence_snapshot,
    },
};
use dayweave_core::{
    DayOfWeek, ItemId, ItemKind as CoreItemKind, Minutes, PlanRequest, Recurrence,
    RecurrenceException, RecurrenceExceptionAction, RecurrenceExceptionSelector,
    RecurrenceMoveSource, RecurrencePeriod, RecurrenceSemantics, RoutineSpec, expand_occurrences,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const FIXTURES: &str = include_str!("../../../fixtures/routine-occurrences/wire-v1.json");
const PLAN: &[u8] =
    include_bytes!("../../../crates/dayweave-scheduler-helper/tests/fixtures/plan-request-v1.json");

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    schema_version: u16,
    valid: Vec<Case>,
    invalid: Vec<Case>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    kind: String,
    value: Value,
}

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn now() -> DateTime<Utc> {
    "2026-09-01T07:00:00.123456Z".parse().unwrap()
}

fn open(status: ItemStatus) -> ItemCompletionReopenState {
    ItemCompletionReopenState {
        status,
        blocked_reason_kind: (status == ItemStatus::Blocked).then_some(BlockedReasonKind::Manual),
        blocked_by_item_id: None,
        blocked_reason: (status == ItemStatus::Blocked)
            .then(|| "Synthetic waiting for input".into()),
    }
}

fn definition(item: u128, parent: Option<u128>) -> RoutineOccurrenceMemberDefinition {
    RoutineOccurrenceMemberDefinition {
        item_id: id(item),
        parent_id: parent.map(id),
        source_revision: 7,
        title: format!("Synthetic member {item}"),
        kind: if parent.is_none() {
            ItemKind::Routine
        } else {
            ItemKind::Task
        },
        recurs: parent.is_none(),
        sibling_order: 0,
        required_for_parent: item != 4,
        initial_open: open(if parent.is_none() {
            ItemStatus::Blocked
        } else {
            ItemStatus::Planned
        }),
    }
}

fn planner_request(recurrence: Recurrence) -> PlanRequest {
    let raw: Value = serde_json::from_slice(PLAN).unwrap();
    let mut request: PlanRequest = serde_json::from_value(raw["request"].clone()).unwrap();
    let leaf = request.items[0].clone();
    let mut root = leaf.clone();
    root.kind = CoreItemKind::Routine(RoutineSpec {
        ordered: false,
        recurrence: Some(recurrence),
    });
    root.duration = None;
    root.revision = 7;
    let mut branch = leaf.clone();
    branch.id = ItemId(id(2));
    branch.parent_id = Some(root.id);
    branch.duration = None;
    let mut required = leaf.clone();
    required.id = ItemId(id(3));
    required.parent_id = Some(branch.id);
    let mut optional = leaf;
    optional.id = ItemId(id(4));
    optional.parent_id = Some(root.id);
    request.items = vec![root, branch, required, optional];
    for item in &mut request.items {
        item.revision = 7;
    }
    request
}

fn manifest_for(request: &PlanRequest) -> RoutineOccurrenceManifest {
    let occurrence = expand_occurrences(request)
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!(
                "synthetic horizon must contain an occurrence: {:?}",
                request.items[0].kind
            )
        });
    let instant = |value: time::OffsetDateTime| {
        DateTime::from_timestamp_micros(
            i64::try_from(value.unix_timestamp_nanos() / 1_000).unwrap(),
        )
        .unwrap()
    };
    RoutineOccurrenceManifest {
        schema_version: 1,
        id: id(100),
        series_item_id: occurrence.series_item_id.0,
        occurrence_id: occurrence.id.0,
        identity: occurrence.identity,
        nominal_start: instant(occurrence.nominal_start),
        nominal_end: instant(occurrence.nominal_end),
        window_start: instant(occurrence.window_start),
        window_end: instant(occurrence.window_end),
        timezone_name: "UTC".into(),
        definition_hash: format!("sha256:{}", "a".repeat(64)),
        members: vec![
            definition(1, None),
            definition(2, Some(1)),
            definition(3, Some(2)),
            definition(4, Some(1)),
        ],
    }
}

fn initial() -> RoutineOccurrenceAggregate {
    initialize_routine_occurrence(
        manifest_for(&planner_request(Recurrence::Daily { times_per_day: 1 })),
        now(),
    )
    .unwrap()
}

fn evidence(aggregate: &RoutineOccurrenceAggregate) -> RoutineOccurrenceEvidence {
    RoutineOccurrenceEvidence {
        current_definition_hash: aggregate.manifest.definition_hash.clone(),
        sources: aggregate
            .manifest
            .members
            .iter()
            .map(|member| RoutineOccurrenceSourceEvidence {
                item_id: member.item_id,
                current_revision: Some(member.source_revision),
                eligible: true,
            })
            .collect(),
        execution_revision: 0,
        live_work_units: BTreeSet::new(),
    }
}

fn reviewed(
    aggregate: &RoutineOccurrenceAggregate,
    target: u128,
    action: RoutineOccurrenceAction,
) -> RoutineOccurrenceCommand {
    RoutineOccurrenceCommand {
        schema_version: 1,
        operation_id: id(1_000 + u128::from(aggregate.revision)),
        expected_instance_revision: aggregate.revision,
        expected_member_revision: aggregate
            .members
            .iter()
            .find(|member| member.item_id == id(target))
            .unwrap()
            .revision,
        expected_evidence_hash: routine_occurrence_snapshot(aggregate, &evidence(aggregate))
            .unwrap()
            .evidence_hash,
        action,
    }
}

fn add(cases: &mut Vec<Case>, name: &str, kind: &str, value: &impl Serialize) {
    cases.push(Case {
        name: name.into(),
        kind: kind.into(),
        value: serde_json::to_value(value).unwrap(),
    });
}

fn transition(
    cases: &mut Vec<Case>,
    aggregate: &RoutineOccurrenceAggregate,
    name: &str,
    target: u128,
    action: RoutineOccurrenceAction,
) -> RoutineOccurrenceSnapshot {
    let command = reviewed(aggregate, target, action);
    let plan = plan_routine_occurrence(
        aggregate,
        &evidence(aggregate),
        id(target),
        &command,
        now() + Duration::seconds(i64::try_from(aggregate.revision).unwrap()),
    )
    .unwrap();
    add(cases, &format!("{name}_command"), "command", &command);
    add(
        cases,
        &format!("{name}_mutation"),
        "mutation",
        &RoutineOccurrenceMutation {
            operation_id: command.operation_id,
            replayed: false,
            occurrence: plan.snapshot.clone(),
        },
    );
    plan.snapshot
}

fn policy(required_for_parent: bool, mode: ItemCompletionMode) -> RoutineOccurrenceAction {
    RoutineOccurrenceAction::SetPolicy {
        required_for_parent,
        mode,
    }
}

fn cursor(after: u64, list_head: Option<u64>) -> String {
    // Same serialization order as the private server cursor, for a synthetic
    // scope only. Consumers treat the result as opaque, not a local proof.
    #[derive(Serialize)]
    struct Cursor {
        workspace: Uuid,
        list_head: Option<u64>,
        after: u64,
    }
    format!(
        "DWR1.{}",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Cursor {
                workspace: id(900),
                list_head,
                after
            })
            .unwrap()
        )
    )
}

#[allow(clippy::too_many_lines)] // One deterministic producer sequence documents the portable corpus.
fn valid_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let first = initial();
    let baseline = routine_occurrence_snapshot(&first, &evidence(&first)).unwrap();
    add(
        &mut cases,
        "initial_required_and_optional_snapshot",
        "snapshot",
        &baseline,
    );
    let done = transition(
        &mut cases,
        &first,
        "required_done",
        3,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Completed,
        },
    );
    add(
        &mut cases,
        "automatic_parent_keeps_optional_demand_snapshot",
        "snapshot",
        &done,
    );
    let unchanged_done = transition(
        &mut cases,
        &done.aggregate,
        "done_again_preserves_completed_at",
        3,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Completed,
        },
    );
    assert_eq!(
        done.aggregate.members[2].completed_at,
        unchanged_done.aggregate.members[2].completed_at
    );
    let corrected = transition(
        &mut cases,
        &unchanged_done.aggregate,
        "reopen_planned",
        3,
        RoutineOccurrenceAction::Reopen {
            open: open(ItemStatus::Planned),
        },
    );
    let skipped = transition(
        &mut cases,
        &corrected.aggregate,
        "required_skipped",
        3,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Skipped,
        },
    );
    add(
        &mut cases,
        "skipped_is_not_done_snapshot",
        "snapshot",
        &skipped,
    );
    let inbox = transition(
        &mut cases,
        &skipped.aggregate,
        "reopen_inbox",
        3,
        RoutineOccurrenceAction::Reopen {
            open: open(ItemStatus::Inbox),
        },
    );
    let blocked = transition(
        &mut cases,
        &inbox.aggregate,
        "reopen_blocked",
        3,
        RoutineOccurrenceAction::Reopen {
            open: open(ItemStatus::Blocked),
        },
    );
    let manual = transition(
        &mut cases,
        &blocked.aggregate,
        "parent_manual_complete",
        2,
        policy(true, ItemCompletionMode::Complete),
    );
    let kept = transition(
        &mut cases,
        &manual.aggregate,
        "parent_keep_open",
        2,
        policy(true, ItemCompletionMode::KeepOpen),
    );
    let automatic = transition(
        &mut cases,
        &kept.aggregate,
        "parent_automatic",
        2,
        policy(true, ItemCompletionMode::Automatic),
    );
    let optional = transition(
        &mut cases,
        &automatic.aggregate,
        "leaf_required_edge_changed",
        4,
        policy(true, ItemCompletionMode::Automatic),
    );
    add(
        &mut cases,
        "runtime_policy_differs_from_initial_definition_snapshot",
        "snapshot",
        &optional,
    );
    let historical = RoutineOccurrenceMutation {
        operation_id: id(1_001),
        replayed: true,
        occurrence: done.clone(),
    };
    add(
        &mut cases,
        "required_done_historical_replay",
        "mutation",
        &historical,
    );
    let mut current = evidence(&first);
    current.sources[0].current_revision = Some(8);
    add(
        &mut cases,
        "harmless_current_source_revision_drift_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&first, &current).unwrap(),
    );
    current.sources[1].eligible = false;
    current.sources[1].current_revision = None;
    add(
        &mut cases,
        "missing_source_historical_read_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&first, &current).unwrap(),
    );
    let mut changed = evidence(&first);
    changed.current_definition_hash = format!("sha256:{}", "b".repeat(64));
    add(
        &mut cases,
        "changed_definition_historical_read_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&first, &changed).unwrap(),
    );
    let mut nested = first.manifest.clone();
    nested.members[1].recurs = true;
    nested.members[1].kind = ItemKind::Routine;
    let nested = initialize_routine_occurrence(nested, now()).unwrap();
    add(
        &mut cases,
        "nested_recurrence_is_unqualified_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&nested, &evidence(&nested)).unwrap(),
    );
    let mut single = first.manifest.clone();
    single.members.truncate(1);
    single.members[0].kind = ItemKind::Task;
    let single = initialize_routine_occurrence(single, now()).unwrap();
    add(
        &mut cases,
        "recurring_task_single_member_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&single, &evidence(&single)).unwrap(),
    );
    let mut clipped_request = planner_request(Recurrence::Daily { times_per_day: 1 });
    clipped_request.horizon_start += time::Duration::hours(6);
    let clipped = initialize_routine_occurrence(manifest_for(&clipped_request), now()).unwrap();
    assert!(clipped.manifest.window_start > clipped.manifest.nominal_start);
    add(
        &mut cases,
        "clipped_effective_window_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&clipped, &evidence(&clipped)).unwrap(),
    );
    let mut moved_request = planner_request(Recurrence::Daily { times_per_day: 1 });
    let source = expand_occurrences(&moved_request).unwrap()[0];
    moved_request.horizon_start += time::Duration::days(2);
    moved_request.horizon_end += time::Duration::days(2);
    moved_request.as_of += time::Duration::days(2);
    moved_request
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: source.series_item_id,
            selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
            action: RecurrenceExceptionAction::Move {
                start: moved_request.horizon_start + time::Duration::hours(8),
                end: moved_request.horizon_start + time::Duration::hours(9),
                source: RecurrenceMoveSource {
                    item_revision: moved_request.items[0].revision,
                    identity: source.identity,
                    nominal_start: source.nominal_start,
                    nominal_end: source.nominal_end,
                    local_date: source.local_date,
                    ordinal: source.ordinal,
                },
            },
        });
    let mut moved_manifest = manifest_for(&moved_request);
    let moved_occurrence = expand_occurrences(&moved_request)
        .unwrap()
        .into_iter()
        .find(|occurrence| occurrence.id == source.id)
        .unwrap();
    moved_manifest.occurrence_id = moved_occurrence.id.0;
    moved_manifest.identity = moved_occurrence.identity;
    moved_manifest.nominal_start = first.manifest.nominal_start;
    moved_manifest.nominal_end = first.manifest.nominal_end;
    moved_manifest.window_start = DateTime::from_timestamp_micros(
        i64::try_from(moved_occurrence.window_start.unix_timestamp_nanos() / 1_000).unwrap(),
    )
    .unwrap();
    moved_manifest.window_end = DateTime::from_timestamp_micros(
        i64::try_from(moved_occurrence.window_end.unix_timestamp_nanos() / 1_000).unwrap(),
    )
    .unwrap();
    let moved = initialize_routine_occurrence(moved_manifest, now()).unwrap();
    assert!(moved.manifest.window_start > moved.manifest.nominal_end);
    add(
        &mut cases,
        "disjoint_moved_effective_window_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&moved, &evidence(&moved)).unwrap(),
    );
    for (name, recurrence) in [
        (
            "calendar_week",
            Recurrence::Weekly {
                times_per_week: 1,
                weekdays: BTreeSet::from([DayOfWeek::Tuesday]),
            },
        ),
        ("calendar_month", Recurrence::Monthly { times_per_month: 1 }),
        (
            "rolling_minutes",
            Recurrence::EveryInterval {
                interval: Minutes(60),
            },
        ),
        (
            "after_completion",
            Recurrence::AfterCompletion {
                interval: Minutes(60),
            },
        ),
        (
            "rolling_month",
            Recurrence::Frequency {
                target: 1,
                period: RecurrencePeriod::Month,
                semantics: RecurrenceSemantics::Rolling,
                weekdays: BTreeSet::new(),
                minimum_spacing: Minutes(0),
                anchor: None,
            },
        ),
        (
            "custom_rule",
            Recurrence::Custom {
                rrule: "FREQ=DAILY;INTERVAL=1;COUNT=3".into(),
            },
        ),
    ] {
        let aggregate =
            initialize_routine_occurrence(manifest_for(&planner_request(recurrence)), now())
                .unwrap();
        add(
            &mut cases,
            &format!("{name}_identity_snapshot"),
            "snapshot",
            &routine_occurrence_snapshot(&aggregate, &evidence(&aggregate)).unwrap(),
        );
    }
    for (name, hours, minutes) in [("positive", 23, 59), ("negative", -23, -59)] {
        let mut request = planner_request(Recurrence::EveryInterval {
            interval: Minutes(60),
        });
        request.items[0].created_at = request.items[0]
            .created_at
            .to_offset(time::UtcOffset::from_hms(hours, minutes, 0).unwrap());
        let aggregate = initialize_routine_occurrence(manifest_for(&request), now()).unwrap();
        add(
            &mut cases,
            &format!("identity_anchor_{name}_2359_snapshot"),
            "snapshot",
            &routine_occurrence_snapshot(&aggregate, &evidence(&aggregate)).unwrap(),
        );
    }
    let mut limits = first.clone();
    limits.revision = u64::try_from(i64::MAX).unwrap();
    for member in &mut limits.members {
        member.revision = limits.revision;
    }
    add(
        &mut cases,
        "int64_revision_limit_snapshot",
        "snapshot",
        &routine_occurrence_snapshot(&limits, &evidence(&limits)).unwrap(),
    );
    add(
        &mut cases,
        "int64_revision_limit_command",
        "command",
        &reviewed(
            &limits,
            3,
            RoutineOccurrenceAction::SetOutcome {
                status: ItemStatus::Completed,
            },
        ),
    );
    let mut utc = serde_json::to_value(&baseline).unwrap();
    utc["aggregate"]["members"][0]["updated_at"] = json!("2026-09-01T07:00:00.123456+00:00");
    add(
        &mut cases,
        "utc_zero_offset_spelling_snapshot",
        "snapshot",
        &utc,
    );
    for (name, kind, blocker, reason) in [
        (
            "reopen_external",
            BlockedReasonKind::External,
            None,
            Some("Synthetic external wait".into()),
        ),
        (
            "reopen_dependency",
            BlockedReasonKind::Dependency,
            Some(id(2)),
            None,
        ),
    ] {
        let command = reviewed(
            &first,
            3,
            RoutineOccurrenceAction::Reopen {
                open: ItemCompletionReopenState {
                    status: ItemStatus::Blocked,
                    blocked_reason_kind: Some(kind),
                    blocked_by_item_id: blocker,
                    blocked_reason: reason,
                },
            },
        );
        add(&mut cases, &format!("{name}_command"), "command", &command);
    }
    add(
        &mut cases,
        "empty_terminal_page",
        "page",
        &RoutineOccurrencePage {
            schema_version: 1,
            changes: vec![],
            cursor: cursor(0, None),
            has_more: false,
        },
    );
    add(
        &mut cases,
        "nonterminal_current_page",
        "page",
        &RoutineOccurrencePage {
            schema_version: 1,
            changes: vec![RoutineOccurrenceChange {
                sequence: 1,
                occurrence: baseline,
            }],
            cursor: cursor(1, Some(2)),
            has_more: true,
        },
    );
    // The repository marks an older delta revision ineligible for fresh review;
    // the already-emitted historical mutation retains its original true flag.
    let mut historical_delta = done;
    historical_delta.fresh_edit_eligible = false;
    add(
        &mut cases,
        "terminal_delta_same_instance_history_page",
        "page",
        &RoutineOccurrencePage {
            schema_version: 1,
            changes: vec![
                RoutineOccurrenceChange {
                    sequence: 2,
                    occurrence: historical_delta,
                },
                RoutineOccurrenceChange {
                    sequence: 3,
                    occurrence: unchanged_done,
                },
            ],
            cursor: cursor(3, None),
            has_more: false,
        },
    );
    cases
}

fn case_value(cases: &[Case], name: &str) -> Value {
    cases
        .iter()
        .find(|case| case.name == name)
        .unwrap()
        .value
        .clone()
}

fn changed(
    cases: &mut Vec<Case>,
    base: &Value,
    name: &str,
    kind: &str,
    pointer: &str,
    replacement: Value,
) {
    let mut value = base.clone();
    *value.pointer_mut(pointer).unwrap() = replacement;
    add(cases, name, kind, &value);
}

#[allow(clippy::too_many_lines)] // Explicit corruptions remain reviewable against their valid seed.
fn invalid_cases(valid: &[Case]) -> Vec<Case> {
    let mut cases = Vec::new();
    let initial = case_value(valid, "initial_required_and_optional_snapshot");
    for (name, pointer, replacement) in [
        ("snapshot_schema", "/schema_version", json!(2)),
        ("snapshot_hash", "/evidence_hash", json!("sha256:ABC")),
        ("snapshot_eligible_type", "/fresh_edit_eligible", json!(1)),
        ("aggregate_zero_revision", "/aggregate/revision", json!(0)),
        (
            "aggregate_overflow_revision",
            "/aggregate/revision",
            json!(9_223_372_036_854_775_808_u64),
        ),
        (
            "manifest_schema",
            "/aggregate/manifest/schema_version",
            json!(2),
        ),
        (
            "manifest_nil_id",
            "/aggregate/manifest/id",
            json!(Uuid::nil()),
        ),
        (
            "manifest_id_is_occurrence",
            "/aggregate/manifest/id",
            initial["aggregate"]["manifest"]["occurrence_id"].clone(),
        ),
        (
            "manifest_wrong_uuid_version",
            "/aggregate/manifest/occurrence_id",
            json!("00000000-0000-4000-8000-000000000001"),
        ),
        (
            "manifest_wrong_uuid_variant",
            "/aggregate/manifest/occurrence_id",
            json!("00000000-0000-5000-0000-000000000001"),
        ),
        (
            "manifest_missing_root",
            "/aggregate/manifest/series_item_id",
            json!(id(99)),
        ),
        (
            "manifest_bad_hash",
            "/aggregate/manifest/definition_hash",
            json!("sha256:"),
        ),
        (
            "manifest_bad_timezone",
            "/aggregate/manifest/timezone_name",
            json!("Synthetic/Unknown"),
        ),
        (
            "manifest_bad_date",
            "/aggregate/manifest/nominal_start",
            json!("2026-02-30T00:00:00Z"),
        ),
        (
            "manifest_nanos",
            "/aggregate/manifest/window_start",
            json!("2026-09-01T00:00:00.123456789Z"),
        ),
        (
            "manifest_non_utc",
            "/aggregate/manifest/window_start",
            json!("2026-09-01T01:00:00+01:00"),
        ),
        (
            "manifest_empty_window",
            "/aggregate/manifest/window_end",
            initial["aggregate"]["manifest"]["window_start"].clone(),
        ),
        (
            "identity_wrong_date",
            "/aggregate/manifest/identity/date",
            json!("2026-09-02"),
        ),
        (
            "identity_bucket_limit",
            "/aggregate/manifest/identity/bucket_ordinal",
            json!(65_535),
        ),
        (
            "identity_legacy_custom",
            "/aggregate/manifest/identity",
            json!({"type":"custom"}),
        ),
        (
            "identity_unknown",
            "/aggregate/manifest/identity",
            json!({"type":"invented"}),
        ),
        (
            "definition_missing_parent",
            "/aggregate/manifest/members/2/parent_id",
            json!(id(99)),
        ),
        (
            "definition_self_parent",
            "/aggregate/manifest/members/2/parent_id",
            json!(id(3)),
        ),
        (
            "definition_cycle",
            "/aggregate/manifest/members/1/parent_id",
            json!(id(3)),
        ),
        (
            "definition_second_root",
            "/aggregate/manifest/members/2/parent_id",
            Value::Null,
        ),
        (
            "definition_duplicate_id",
            "/aggregate/manifest/members/2/item_id",
            json!(id(2)),
        ),
        (
            "definition_zero_revision",
            "/aggregate/manifest/members/2/source_revision",
            json!(0),
        ),
        (
            "definition_blank_title",
            "/aggregate/manifest/members/2/title",
            json!(" "),
        ),
        (
            "definition_control_title",
            "/aggregate/manifest/members/2/title",
            json!("Synthetic\nmember"),
        ),
        (
            "definition_long_title",
            "/aggregate/manifest/members/2/title",
            json!("a".repeat(501)),
        ),
        (
            "definition_unknown_kind",
            "/aggregate/manifest/members/2/kind",
            json!("future_kind"),
        ),
        (
            "definition_habit_not_recurring",
            "/aggregate/manifest/members/2/kind",
            json!("habit"),
        ),
        (
            "definition_root_habit",
            "/aggregate/manifest/members/0/kind",
            json!("habit"),
        ),
        (
            "definition_root_not_recurring",
            "/aggregate/manifest/members/0/recurs",
            json!(false),
        ),
        (
            "definition_sibling_bound",
            "/aggregate/manifest/members/2/sibling_order",
            json!(1_000_001),
        ),
        (
            "definition_terminal_initial",
            "/aggregate/manifest/members/2/initial_open/status",
            json!("completed"),
        ),
        (
            "member_unknown_id",
            "/aggregate/members/2/item_id",
            json!(id(99)),
        ),
        (
            "member_duplicate_id",
            "/aggregate/members/2/item_id",
            json!(id(2)),
        ),
        (
            "member_zero_revision",
            "/aggregate/members/2/revision",
            json!(0),
        ),
        (
            "member_ahead_revision",
            "/aggregate/members/2/revision",
            json!(2),
        ),
        (
            "member_execution_status",
            "/aggregate/members/2/status",
            json!("in_progress"),
        ),
        (
            "member_open_mismatch",
            "/aggregate/members/2/status",
            json!("inbox"),
        ),
        (
            "member_unknown_mode",
            "/aggregate/members/2/mode",
            json!("future_mode"),
        ),
        (
            "member_leaf_manual_mode",
            "/aggregate/members/2/mode",
            json!("keep_open"),
        ),
        (
            "member_open_completion_time",
            "/aggregate/members/2/completed_at",
            json!("2026-09-01T08:00:00Z"),
        ),
        (
            "member_nanos",
            "/aggregate/members/2/updated_at",
            json!("2026-09-01T08:00:00.000000001Z"),
        ),
        (
            "member_non_utc",
            "/aggregate/members/2/updated_at",
            json!("2026-09-01T08:00:00+01:00"),
        ),
        (
            "member_year_zero",
            "/aggregate/members/2/updated_at",
            json!("0000-09-01T08:00:00Z"),
        ),
        ("evaluation_unknown_id", "/members/2/item_id", json!(id(99))),
        (
            "evaluation_duplicate_id",
            "/members/2/item_id",
            json!(id(2)),
        ),
        (
            "evaluation_bad_partition",
            "/members/0/counts/completed",
            json!(1),
        ),
        (
            "evaluation_count_overflow",
            "/members/0/counts/completed",
            json!(18_446_744_073_709_551_615_u64),
        ),
        (
            "evaluation_negative_count",
            "/members/0/counts/incomplete",
            json!(-1),
        ),
        (
            "evaluation_wrong_qualification",
            "/members/2/occurrence_evidence_required",
            json!(true),
        ),
        (
            "evaluation_unknown_reason",
            "/members/2/reason",
            json!("future_reason"),
        ),
    ] {
        changed(&mut cases, &initial, name, "snapshot", pointer, replacement);
    }
    for (name, pointer, key) in [
        ("snapshot_unknown_field", "", "unknown"),
        ("manifest_unknown_field", "/aggregate/manifest", "unknown"),
        (
            "definition_unknown_field",
            "/aggregate/manifest/members/0",
            "unknown",
        ),
        (
            "identity_unknown_field",
            "/aggregate/manifest/identity",
            "unknown",
        ),
        ("member_unknown_field", "/aggregate/members/0", "unknown"),
        ("open_unknown_field", "/aggregate/members/0/open", "unknown"),
        ("evaluation_unknown_field", "/members/0", "unknown"),
        ("counts_unknown_field", "/members/0/counts", "unknown"),
    ] {
        let mut value = initial.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), Value::Null);
        add(&mut cases, name, "snapshot", &value);
    }
    for (name, pointer, key) in [
        (
            "missing_parent_nullable",
            "/aggregate/manifest/members/0",
            "parent_id",
        ),
        (
            "missing_provenance_nullable",
            "/aggregate/members/0",
            "provenance",
        ),
        (
            "missing_completed_at_nullable",
            "/aggregate/members/0",
            "completed_at",
        ),
        (
            "missing_blocked_reason_kind_nullable",
            "/aggregate/members/2/open",
            "blocked_reason_kind",
        ),
        (
            "missing_blocked_by_nullable",
            "/aggregate/members/2/open",
            "blocked_by_item_id",
        ),
        (
            "missing_blocked_reason_nullable",
            "/aggregate/members/2/open",
            "blocked_reason",
        ),
        (
            "missing_required_bool",
            "/aggregate/members/2",
            "required_for_parent",
        ),
    ] {
        let mut value = initial.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        add(&mut cases, name, "snapshot", &value);
    }
    for (name, pointer) in [
        ("missing_definition", "/aggregate/manifest/members"),
        ("missing_member", "/aggregate/members"),
        ("missing_evaluation", "/members"),
    ] {
        let mut value = initial.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .pop();
        add(&mut cases, name, "snapshot", &value);
    }
    let done = case_value(valid, "automatic_parent_keeps_optional_demand_snapshot");
    for (name, pointer, replacement) in [
        (
            "completed_missing_anchor",
            "/aggregate/members/2/completed_at",
            Value::Null,
        ),
        (
            "completed_parent_missing_provenance",
            "/aggregate/members/0/provenance",
            Value::Null,
        ),
        (
            "completed_parent_wrong_provenance",
            "/aggregate/members/0/provenance/kind",
            json!("manual"),
        ),
        (
            "completed_parent_wrong_reopen",
            "/aggregate/members/0/provenance/reopen/blocked_reason",
            json!("Different reason"),
        ),
        (
            "completed_leaf_with_provenance",
            "/aggregate/members/2/provenance",
            done["aggregate"]["members"][1]["provenance"].clone(),
        ),
        (
            "completed_parent_keep_open",
            "/aggregate/members/0/mode",
            json!("keep_open"),
        ),
    ] {
        changed(&mut cases, &done, name, "snapshot", pointer, replacement);
    }
    let command = case_value(valid, "required_done_command");
    for (name, pointer, replacement) in [
        ("command_schema", "/schema_version", json!(2)),
        ("command_nil_operation", "/operation_id", json!(Uuid::nil())),
        (
            "command_zero_instance_revision",
            "/expected_instance_revision",
            json!(0),
        ),
        (
            "command_zero_member_revision",
            "/expected_member_revision",
            json!(0),
        ),
        (
            "command_overflow_revision",
            "/expected_member_revision",
            json!(9_223_372_036_854_775_808_u64),
        ),
        (
            "command_float_revision",
            "/expected_member_revision",
            json!(1.0),
        ),
        (
            "command_hash",
            "/expected_evidence_hash",
            json!("a".repeat(64)),
        ),
        (
            "command_outcome_planned",
            "/action/status",
            json!("planned"),
        ),
        (
            "command_outcome_cancelled",
            "/action/status",
            json!("cancelled"),
        ),
        (
            "command_unknown_action",
            "/action",
            json!({"type":"will_do_later"}),
        ),
        (
            "command_unknown_policy",
            "/action",
            json!({"type":"set_policy","required_for_parent":true,"mode":"future"}),
        ),
        (
            "command_missing_policy_required",
            "/action",
            json!({"type":"set_policy","mode":"automatic"}),
        ),
        (
            "command_action_unknown_field",
            "/action",
            json!({"type":"set_outcome","status":"completed","extra":null}),
        ),
    ] {
        changed(&mut cases, &command, name, "command", pointer, replacement);
    }
    let reopen = case_value(valid, "reopen_blocked_command");
    for (name, pointer, replacement) in [
        ("reopen_terminal", "/action/open/status", json!("completed")),
        (
            "reopen_missing_manual_reason",
            "/action/open/blocked_reason",
            Value::Null,
        ),
        (
            "reopen_blank_manual_reason",
            "/action/open/blocked_reason",
            json!(" "),
        ),
        (
            "reopen_control_reason",
            "/action/open/blocked_reason",
            json!("Synthetic\rreason"),
        ),
        (
            "reopen_long_reason",
            "/action/open/blocked_reason",
            json!("a".repeat(1001)),
        ),
        (
            "reopen_manual_with_dependency_id",
            "/action/open/blocked_by_item_id",
            json!(id(2)),
        ),
    ] {
        changed(&mut cases, &reopen, name, "command", pointer, replacement);
    }
    let dependency = case_value(valid, "reopen_dependency_command");
    changed(
        &mut cases,
        &dependency,
        "reopen_dependency_self",
        "command",
        "/action/open/blocked_by_item_id",
        json!(id(3)),
    );
    changed(
        &mut cases,
        &dependency,
        "reopen_dependency_missing_id",
        "command",
        "/action/open/blocked_by_item_id",
        Value::Null,
    );
    for key in [
        "blocked_reason_kind",
        "blocked_by_item_id",
        "blocked_reason",
    ] {
        let mut value = reopen.clone();
        value["action"]["open"].as_object_mut().unwrap().remove(key);
        add(
            &mut cases,
            &format!("command_missing_{key}_nullable"),
            "command",
            &value,
        );
    }
    let mutation = case_value(valid, "required_done_mutation");
    changed(
        &mut cases,
        &mutation,
        "mutation_nil_operation",
        "mutation",
        "/operation_id",
        json!(Uuid::nil()),
    );
    changed(
        &mut cases,
        &mutation,
        "mutation_replayed_type",
        "mutation",
        "/replayed",
        json!("true"),
    );
    changed(
        &mut cases,
        &mutation,
        "mutation_ineligible_success",
        "mutation",
        "/occurrence/fresh_edit_eligible",
        json!(false),
    );
    let page = case_value(valid, "terminal_delta_same_instance_history_page");
    for (name, pointer, replacement) in [
        ("page_schema", "/schema_version", json!(2)),
        ("page_zero_sequence", "/changes/0/sequence", json!(0)),
        ("page_duplicate_sequence", "/changes/1/sequence", json!(2)),
        (
            "page_overflow_sequence",
            "/changes/0/sequence",
            json!(9_223_372_036_854_775_808_u64),
        ),
        ("page_empty_cursor", "/cursor", json!("")),
        ("page_control_cursor", "/cursor", json!("DWR1.a\nb")),
        ("page_oversized_cursor", "/cursor", json!("a".repeat(513))),
        ("page_more_type", "/has_more", json!(1)),
    ] {
        changed(&mut cases, &page, name, "page", pointer, replacement);
    }
    let mut empty_more = case_value(valid, "empty_terminal_page");
    empty_more["has_more"] = json!(true);
    add(&mut cases, "page_empty_nonterminal", "page", &empty_more);
    cases
}

fn produced_fixtures() -> Fixtures {
    let valid = valid_cases();
    let invalid = invalid_cases(&valid);
    Fixtures {
        schema_version: 1,
        valid,
        invalid,
    }
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn utc_wire_time(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let Some(body) = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))
    else {
        return false;
    };
    (body.len() == 19 || (21..=26).contains(&body.len()))
        && !body.starts_with("0000-")
        && body.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'.',
            _ => byte.is_ascii_digit(),
        })
        && DateTime::parse_from_rfc3339(text).is_ok()
}

fn snapshot_admitted(snapshot: &RoutineOccurrenceSnapshot, raw: &Value) -> bool {
    if snapshot.schema_version != 1
        || !valid_hash(&snapshot.evidence_hash)
        || !["nominal_start", "nominal_end", "window_start", "window_end"]
            .iter()
            .all(|key| utc_wire_time(&raw["aggregate"]["manifest"][key]))
        || !raw["aggregate"]["members"]
            .as_array()
            .is_some_and(|members| {
                members.iter().all(|member| {
                    utc_wire_time(&member["updated_at"])
                        && (member["completed_at"].is_null()
                            || utc_wire_time(&member["completed_at"]))
                })
            })
    {
        return false;
    }
    let Ok(expected) =
        routine_occurrence_snapshot(&snapshot.aggregate, &evidence(&snapshot.aggregate))
    else {
        return false;
    };
    let actual: BTreeMap<_, _> = snapshot
        .members
        .iter()
        .map(|member| (member.item_id, member))
        .collect();
    actual.len() == snapshot.members.len()
        && actual.len() == expected.members.len()
        && expected.members.iter().all(|member| {
            actual.get(&member.item_id).is_some_and(|actual| {
                actual.counts == member.counts
                    && actual.occurrence_evidence_required == member.occurrence_evidence_required
            })
        })
}

fn admitted(kind: &str, value: &Value) -> bool {
    if serde_json::to_vec(value).unwrap().len() > MAX_ROUTINE_OCCURRENCE_BYTES {
        return false;
    }
    match kind {
        "snapshot" => serde_json::from_value::<RoutineOccurrenceSnapshot>(value.clone())
            .is_ok_and(|snapshot| snapshot_admitted(&snapshot, value)),
        "command" => serde_json::from_value::<RoutineOccurrenceCommand>(value.clone())
            .is_ok_and(|command| command.validate(id(3)).is_ok()),
        "mutation" => serde_json::from_value::<RoutineOccurrenceMutation>(value.clone()).is_ok_and(
            |mutation| {
                !mutation.operation_id.is_nil()
                    && mutation.occurrence.aggregate.revision >= 2
                    && mutation.occurrence.fresh_edit_eligible
                    && snapshot_admitted(&mutation.occurrence, &value["occurrence"])
            },
        ),
        "page" => {
            serde_json::from_value::<RoutineOccurrencePage>(value.clone()).is_ok_and(|page| {
                page.schema_version == 1
                    && page.changes.len() <= 100
                    && (!page.has_more || !page.changes.is_empty())
                    && !page.cursor.is_empty()
                    && page.cursor.len() <= 512
                    && page.cursor.bytes().all(|byte| byte.is_ascii_graphic())
                    && page
                        .changes
                        .windows(2)
                        .all(|pair| pair[0].sequence < pair[1].sequence)
                    && page.changes.iter().enumerate().all(|(index, change)| {
                        change.sequence > 0
                            && i64::try_from(change.sequence).is_ok()
                            && snapshot_admitted(
                                &change.occurrence,
                                &value["changes"][index]["occurrence"],
                            )
                    })
            })
        }
        unknown => panic!("unknown fixture kind {unknown}"),
    }
}

#[test]
#[ignore = "maintenance-only stdout fixture generator; root runner invokes explicitly"]
fn emit_shared_routine_occurrence_fixture() {
    let fixtures = produced_fixtures();
    for case in &fixtures.valid {
        assert!(
            admitted(&case.kind, &case.value),
            "valid producer {}",
            case.name
        );
    }
    for case in &fixtures.invalid {
        assert!(
            !admitted(&case.kind, &case.value),
            "invalid producer {}",
            case.name
        );
    }
    println!(
        "ROUTINE_OCCURRENCE_FIXTURE={}",
        serde_json::to_string(&fixtures).unwrap()
    );
}

#[test]
fn shared_wire_fixtures_match_real_producer_and_portable_admission() {
    let fixtures: Fixtures = serde_json::from_str(FIXTURES).unwrap();
    assert_eq!(fixtures, produced_fixtures());
    assert_eq!(fixtures.schema_version, 1);
    assert!(!fixtures.valid.is_empty() && !fixtures.invalid.is_empty());
    let mut names = BTreeSet::new();
    for (expected, cases) in [(true, fixtures.valid), (false, fixtures.invalid)] {
        for case in cases {
            assert!(names.insert(case.name.clone()), "duplicate {}", case.name);
            assert_eq!(admitted(&case.kind, &case.value), expected, "{}", case.name);
        }
    }
}

fn matches_request(
    mutation: &RoutineOccurrenceMutation,
    command: &RoutineOccurrenceCommand,
    instance: Uuid,
    target: Uuid,
) -> bool {
    mutation.operation_id == command.operation_id
        && mutation.occurrence.aggregate.manifest.id == instance
        && command.expected_instance_revision.checked_add(1)
            == Some(mutation.occurrence.aggregate.revision)
        && mutation.occurrence.aggregate.members.iter().any(|member| {
            member.item_id == target
                && command.expected_member_revision.checked_add(1) == Some(member.revision)
        })
}

#[test]
fn historical_receipt_binds_exact_route_operation_and_cas_not_current_read_proof() {
    let valid = valid_cases();
    let command: RoutineOccurrenceCommand =
        serde_json::from_value(case_value(&valid, "required_done_command")).unwrap();
    let receipt: RoutineOccurrenceMutation =
        serde_json::from_value(case_value(&valid, "required_done_mutation")).unwrap();
    let replay: RoutineOccurrenceMutation =
        serde_json::from_value(case_value(&valid, "required_done_historical_replay")).unwrap();
    assert!(!receipt.replayed && replay.replayed);
    assert_eq!(receipt.occurrence, replay.occurrence);
    assert!(matches_request(&receipt, &command, id(100), id(3)));
    assert!(matches_request(&replay, &command, id(100), id(3)));
    assert!(!matches_request(&replay, &command, id(101), id(3)));
    assert!(!matches_request(&replay, &command, id(100), id(4)));
    let mut different = command.clone();
    different.operation_id = id(999);
    assert!(!matches_request(&replay, &different, id(100), id(3)));
    different = command;
    different.expected_instance_revision = u64::try_from(i64::MAX).unwrap();
    assert!(!matches_request(&replay, &different, id(100), id(3)));
    let newer: RoutineOccurrenceSnapshot = serde_json::from_value(case_value(
        &valid,
        "runtime_policy_differs_from_initial_definition_snapshot",
    ))
    .unwrap();
    assert!(newer.aggregate.revision > replay.occurrence.aggregate.revision);
    assert_ne!(newer.evidence_hash, replay.occurrence.evidence_hash);
}

#[test]
fn raw_duplicate_and_noninteger_tokens_are_rejected_before_value_conversion() {
    let command = case_value(&valid_cases(), "required_done_command").to_string();
    let duplicate = command.replacen('{', "{\"schema_version\":1,", 1);
    assert!(serde_json::from_str::<RoutineOccurrenceCommand>(&duplicate).is_err());
    for spelling in ["1.0", "1e0", "1E+0"] {
        let raw = command.replace(
            "\"expected_instance_revision\":1",
            &format!("\"expected_instance_revision\":{spelling}"),
        );
        assert_ne!(raw, command);
        assert!(
            serde_json::from_str::<RoutineOccurrenceCommand>(&raw).is_err(),
            "{spelling}"
        );
    }
    let snapshot = case_value(&valid_cases(), "initial_required_and_optional_snapshot").to_string();
    assert!(
        serde_json::from_str::<RoutineOccurrenceSnapshot>(&snapshot.replacen(
            '{',
            "{\"schema_version\":1,",
            1
        ))
        .is_err()
    );
}

#[test]
fn generated_deep_tree_and_resource_bounds_need_no_massive_shared_fixture() {
    let mut manifest = initial().manifest;
    manifest.members = (1..=5_001)
        .map(|item| definition(item, (item > 1).then_some(item - 1)))
        .collect();
    for member in &mut manifest.members {
        member.required_for_parent = true;
    }
    manifest.members.reverse();
    let aggregate = initialize_routine_occurrence(manifest.clone(), now()).unwrap();
    let command = reviewed(
        &aggregate,
        5_001,
        RoutineOccurrenceAction::SetOutcome {
            status: ItemStatus::Completed,
        },
    );
    let plan = plan_routine_occurrence(
        &aggregate,
        &evidence(&aggregate),
        id(5_001),
        &command,
        now(),
    )
    .unwrap();
    assert_eq!(plan.effects.len(), 5_001);
    assert_eq!(plan.snapshot.members[0].counts.completed, 5_000);
    assert!(
        plan.snapshot
            .aggregate
            .members
            .iter()
            .all(|member| member.status == ItemStatus::Completed)
    );
    manifest.members = (1..=u128::try_from(MAX_ROUTINE_OCCURRENCE_MEMBERS + 1).unwrap())
        .map(|item| definition(item, (item > 1).then_some(1)))
        .collect();
    assert_eq!(
        initialize_routine_occurrence(manifest.clone(), now()),
        Err(RoutineOccurrenceError::TooLarge)
    );
    manifest.members.pop();
    for member in &mut manifest.members {
        member.title = "界".repeat(500);
    }
    assert!(serde_json::to_vec(&manifest).unwrap().len() > MAX_ROUTINE_OCCURRENCE_BYTES);
    assert_eq!(
        initialize_routine_occurrence(manifest, now()),
        Err(RoutineOccurrenceError::TooLarge)
    );
}
