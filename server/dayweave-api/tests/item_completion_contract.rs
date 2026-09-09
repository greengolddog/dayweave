//! Portable wire admission is intentionally stricter than plain response serde:
//! response identity, bounded counts, hash syntax and raw UTC spelling are checked
//! here explicitly. Production state/command validation remains the real oracle.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use dayweave_api::{
    item_completion::{
        ItemCompletionCommand, ItemCompletionExecutionEvidence, ItemCompletionMode,
        ItemCompletionMutation, ItemCompletionSnapshot, ItemCompletionState, MAX_COMPLETION_ITEMS,
        item_completion_evidence_hash, plan_item_completion,
    },
    items::{BlockedReasonKind, Item, ItemStatus, NewItem},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

const FIXTURES: &str = include_str!("../../../fixtures/item-completion/wire-v1.json");
const TARGET: Uuid = Uuid::from_u128(1);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    schema_version: u16,
    valid: Vec<Case>,
    invalid: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    kind: String,
    value: Value,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(FIXTURES).expect("portable completion fixtures")
}

fn fixture_value(name: &str) -> Value {
    fixtures()
        .valid
        .into_iter()
        .find(|case| case.name == name)
        .expect("named valid fixture")
        .value
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_utc_wire_time(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let Some(body) = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))
    else {
        return false;
    };
    if !(body.len() == 19 || (21..=26).contains(&body.len())) || body.starts_with("0000-") {
        return false;
    }
    let shape = body.bytes().enumerate().all(|(index, byte)| match index {
        4 | 7 => byte == b'-',
        10 => byte == b'T',
        13 | 16 => byte == b':',
        19 => byte == b'.',
        _ => byte.is_ascii_digit(),
    });
    shape && DateTime::parse_from_rfc3339(text).is_ok()
}

fn valid_state(state: &ItemCompletionState, raw: &Value) -> bool {
    state.validate().is_ok() && (state.revision == 0 || valid_utc_wire_time(&raw["updated_at"]))
}

fn valid_snapshot(snapshot: &ItemCompletionSnapshot, raw: &Value) -> bool {
    let counts = &snapshot.counts;
    let partition = counts
        .completed
        .checked_add(counts.incomplete)
        .and_then(|sum| sum.checked_add(counts.occurrence_evidence_required));
    snapshot.schema_version == 1
        && !snapshot.item_id.is_nil()
        && snapshot.item_revision > 0
        && i64::try_from(snapshot.item_revision).is_ok()
        && snapshot.item_id == snapshot.state.item_id
        && valid_state(&snapshot.state, &raw["state"])
        && valid_hash(&snapshot.evidence_hash)
        && partition == Some(counts.required_descendants)
        && [
            counts.required_descendants,
            counts.completed,
            counts.incomplete,
            counts.occurrence_evidence_required,
        ]
        .iter()
        .all(|count| *count <= MAX_COMPLETION_ITEMS as u64)
}

fn admitted(kind: &str, value: &Value) -> bool {
    match kind {
        "state" => serde_json::from_value::<ItemCompletionState>(value.clone())
            .is_ok_and(|state| valid_state(&state, value)),
        "command" => serde_json::from_value::<ItemCompletionCommand>(value.clone())
            .is_ok_and(|command| command.validate(TARGET).is_ok()),
        "snapshot" => serde_json::from_value::<ItemCompletionSnapshot>(value.clone())
            .is_ok_and(|snapshot| valid_snapshot(&snapshot, value)),
        "receipt" => {
            serde_json::from_value::<ItemCompletionMutation>(value.clone()).is_ok_and(|receipt| {
                !receipt.operation_id.is_nil()
                    && receipt.completion.state.revision > 0
                    && receipt.completion.item_revision >= 2
                    && valid_snapshot(&receipt.completion, &value["completion"])
            })
        }
        unknown => panic!("unknown completion fixture kind {unknown}"),
    }
}

#[test]
fn portable_completion_fixture_admission_matches_closed_wire_contract() {
    let fixtures = fixtures();
    assert_eq!(fixtures.schema_version, 1);
    assert!(!fixtures.valid.is_empty() && !fixtures.invalid.is_empty());
    let mut names = BTreeSet::new();
    for (expected, cases) in [(true, fixtures.valid), (false, fixtures.invalid)] {
        for case in cases {
            assert!(
                names.insert(case.name.clone()),
                "duplicate fixture name {}",
                case.name
            );
            assert_eq!(admitted(&case.kind, &case.value), expected, "{}", case.name);
        }
    }
}

#[test]
fn integer_revision_boundaries_are_read_without_floating_point_rounding() {
    let state = fixture_value("state_int64_revision_limit");
    assert_eq!(state["revision"].as_u64(), u64::try_from(i64::MAX).ok());
    let command: ItemCompletionCommand =
        serde_json::from_value(fixture_value("command_int64_revision_limits")).unwrap();
    assert!(command.validate(TARGET).is_ok());
    assert_eq!(
        i64::try_from(command.expected_item_revision).unwrap(),
        i64::MAX
    );
    assert_eq!(
        i64::try_from(command.expected_completion_revision).unwrap(),
        i64::MAX
    );
}

fn matches_request(receipt: &ItemCompletionMutation, command: &ItemCompletionCommand) -> bool {
    receipt.operation_id == command.operation_id
        && receipt.completion.item_id == TARGET
        && command.expected_item_revision.checked_add(1) == Some(receipt.completion.item_revision)
        && command.expected_completion_revision.checked_add(1)
            == Some(receipt.completion.state.revision)
        && receipt.completion.state.required_for_parent == command.required_for_parent
        && receipt.completion.state.mode == command.mode
}

#[test]
fn reviewed_request_and_historical_receipt_keep_exact_cas_relationships() {
    let command =
        serde_json::from_value::<ItemCompletionCommand>(fixture_value("review_keep_open_command"))
            .unwrap();
    let receipt: ItemCompletionMutation =
        serde_json::from_value(fixture_value("review_keep_open_receipt")).unwrap();
    let historical: ItemCompletionMutation =
        serde_json::from_value(fixture_value("review_keep_open_historical_replay")).unwrap();
    assert!(matches_request(&receipt, &command) && matches_request(&historical, &command));
    assert!(!receipt.replayed && historical.replayed);
    assert_eq!(receipt.completion, historical.completion);
    let mut wrong_revision = receipt.clone();
    wrong_revision.completion.state.revision += 1;
    assert!(!matches_request(&wrong_revision, &command));
    let mut wrong_identity = receipt.clone();
    wrong_identity.operation_id = Uuid::from_u128(101);
    assert!(!matches_request(&wrong_identity, &command));
    let mut at_limit = command;
    at_limit.expected_item_revision = u64::try_from(i64::MAX).unwrap();
    assert!(!matches_request(&receipt, &at_limit));
}

#[test]
fn original_json_rejects_duplicate_keys_and_noninteger_revision_spellings() {
    let command = fixture_value("review_keep_open_command").to_string();
    let duplicate = command.replacen('{', "{\"expected_item_revision\":7,", 1);
    assert!(serde_json::from_str::<ItemCompletionCommand>(&duplicate).is_err());
    for spelling in ["7.0", "7e0", "7E+0"] {
        let raw = command.replace(
            "\"expected_item_revision\":7",
            &format!("\"expected_item_revision\":{spelling}"),
        );
        assert_ne!(raw, command);
        assert!(
            serde_json::from_str::<ItemCompletionCommand>(&raw).is_err(),
            "{spelling}"
        );
    }
    let snapshot = fixture_value("default_leaf_snapshot").to_string();
    let duplicate = snapshot.replacen('{', "{\"schema_version\":1,", 1);
    assert!(serde_json::from_str::<ItemCompletionSnapshot>(&duplicate).is_err());
    let raw = snapshot.replace("\"item_revision\":7", "\"item_revision\":7e0");
    assert_ne!(raw, snapshot);
    assert!(serde_json::from_str::<ItemCompletionSnapshot>(&raw).is_err());
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-09T10:11:12.123456Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn item(id: u128, parent: Option<u128>, status: ItemStatus) -> Item {
    let input: NewItem = serde_json::from_value(json!({
        "id":Uuid::from_u128(id),"kind":"task","status":status,"title":"Synthetic wire fixture",
        "is_sensitive":false,"timezone_name":"UTC","duration_seconds":60,
        "parent_id":parent.map(Uuid::from_u128),
    }))
    .unwrap();
    Item::new(input, now()).unwrap()
}

fn assert_snapshot_fixture(actual: &ItemCompletionSnapshot, fixture_name: &str) {
    let mut expected = fixture_value(fixture_name);
    // Fixture hashes are deliberately opaque syntax samples. The actual proof
    // is produced by the real canonical/state/execution hashing implementation.
    expected["evidence_hash"] = json!(actual.evidence_hash);
    let actual = serde_json::to_value(actual).unwrap();
    assert_eq!(actual, expected, "{fixture_name}");
    assert!(admitted("snapshot", &actual));
}

#[test]
fn actual_planner_serializes_automatic_blocked_parent_fixture() {
    let mut parent = item(1, None, ItemStatus::Planned);
    parent.status = ItemStatus::Blocked;
    parent.revision = 7;
    parent.is_executable = false;
    parent.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
    parent.blocked_by_item_id = Some(Uuid::from_u128(2));
    parent.blocked_reason = Some("Waiting for a synthetic prerequisite".into());
    let mut items = vec![
        parent,
        item(2, Some(1), ItemStatus::Completed),
        item(3, Some(1), ItemStatus::Completed),
    ];
    let execution = ItemCompletionExecutionEvidence::default();
    let plan = plan_item_completion(&items, &[], &execution, None, now()).unwrap();
    let effect = plan
        .effects
        .iter()
        .find(|effect| effect.after_item.id == TARGET)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&effect.after_state).unwrap(),
        fixture_value("automatic_blocked_parent_state")
    );
    assert_eq!(effect.after_item.status, ItemStatus::Completed);
    assert!(effect.after_item.blocked_reason.is_none());
    items[0] = effect.after_item.clone();
    let read = plan_item_completion(
        &items,
        std::slice::from_ref(&effect.after_state),
        &execution,
        None,
        now(),
    )
    .unwrap();
    assert!(read.effects.is_empty());
    assert_snapshot_fixture(
        &read.snapshots[&TARGET],
        "automatic_blocked_parent_snapshot",
    );
}

#[test]
fn actual_planner_serializes_manual_and_keep_open_fixtures() {
    for (mode, child_status, required, fixture_name) in [
        (
            ItemCompletionMode::Complete,
            ItemStatus::Planned,
            true,
            "manual_complete_open_descendant_snapshot",
        ),
        (
            ItemCompletionMode::KeepOpen,
            ItemStatus::Completed,
            false,
            "keep_open_completed_descendant_snapshot",
        ),
    ] {
        let mut parent = item(1, None, ItemStatus::Planned);
        parent.revision = 7;
        parent.is_executable = false;
        let mut items = vec![parent, item(2, Some(1), child_status)];
        let execution = ItemCompletionExecutionEvidence::default();
        let command = ItemCompletionCommand {
            schema_version: 1,
            operation_id: Uuid::from_u128(100),
            expected_item_revision: 7,
            expected_completion_revision: 0,
            expected_evidence_hash: item_completion_evidence_hash(&items, &[], &execution).unwrap(),
            required_for_parent: required,
            mode,
            reopening: None,
        };
        let plan =
            plan_item_completion(&items, &[], &execution, Some((TARGET, &command)), now()).unwrap();
        let effect = plan
            .effects
            .iter()
            .find(|effect| effect.after_item.id == TARGET)
            .unwrap();
        items[0] = effect.after_item.clone();
        let read = plan_item_completion(
            &items,
            std::slice::from_ref(&effect.after_state),
            &execution,
            None,
            now(),
        )
        .unwrap();
        assert!(read.effects.is_empty());
        assert_snapshot_fixture(&read.snapshots[&TARGET], fixture_name);
    }
}

#[test]
fn actual_recurring_counts_keep_own_and_descendant_evidence_separate() {
    let execution = ItemCompletionExecutionEvidence::default();
    let mut leaf = item(1, None, ItemStatus::Planned);
    leaf.revision = 7;
    leaf.recurrence = Some(json!({"frequency":"daily"}));
    let read = plan_item_completion(&[leaf], &[], &execution, None, now()).unwrap();
    assert_snapshot_fixture(
        &read.snapshots[&TARGET],
        "unresolved_recurring_leaf_snapshot",
    );
    let mut parent = item(1, None, ItemStatus::Planned);
    parent.revision = 7;
    parent.is_executable = false;
    let mut child = item(2, Some(1), ItemStatus::Completed);
    child.recurrence = Some(json!({"frequency":"daily"}));
    let read = plan_item_completion(&[parent, child], &[], &execution, None, now()).unwrap();
    assert_snapshot_fixture(
        &read.snapshots[&TARGET],
        "unresolved_required_branch_snapshot",
    );
}
