use super::*;
use crate::items::BlockedReasonKind;
use chrono::{Duration, Timelike as _};
use serde_json::json;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn now() -> DateTime<Utc> {
    "2026-09-10T08:00:00.123456Z".parse().unwrap()
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
        required_for_parent: true,
        initial_open: open(if parent.is_none() {
            ItemStatus::Blocked
        } else {
            ItemStatus::Planned
        }),
    }
}

fn manifest(members: Vec<RoutineOccurrenceMemberDefinition>) -> RoutineOccurrenceManifest {
    RoutineOccurrenceManifest {
        schema_version: 1,
        id: id(100),
        series_item_id: id(1),
        occurrence_id: "10000000-0000-5000-8000-000000000001".parse().unwrap(),
        identity: RecurrenceOccurrenceIdentity::CalendarDay {
            date: time::Date::from_calendar_date(2026, time::Month::September, 10).unwrap(),
            bucket_ordinal: 0,
        },
        nominal_start: "2026-09-10T08:00:00Z".parse().unwrap(),
        nominal_end: "2026-09-10T09:00:00Z".parse().unwrap(),
        window_start: "2026-09-10T00:00:00Z".parse().unwrap(),
        window_end: "2026-09-11T00:00:00Z".parse().unwrap(),
        timezone_name: "UTC".into(),
        definition_hash: format!("sha256:{}", "a".repeat(64)),
        members,
    }
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
    evidence: &RoutineOccurrenceEvidence,
    target: u128,
    action: RoutineOccurrenceAction,
) -> RoutineOccurrenceCommand {
    let snapshot = routine_occurrence_snapshot(aggregate, evidence).unwrap();
    RoutineOccurrenceCommand {
        schema_version: 1,
        operation_id: id(1_000 + u128::from(aggregate.revision)),
        expected_instance_revision: aggregate.revision,
        expected_member_revision: member(aggregate, target).revision,
        expected_evidence_hash: snapshot.evidence_hash,
        action,
    }
}

fn apply(
    aggregate: &RoutineOccurrenceAggregate,
    target: u128,
    action: RoutineOccurrenceAction,
) -> RoutineOccurrencePlan {
    let evidence = evidence(aggregate);
    let command = reviewed(aggregate, &evidence, target, action);
    plan_routine_occurrence(aggregate, &evidence, id(target), &command, now()).unwrap()
}

fn member(aggregate: &RoutineOccurrenceAggregate, target: u128) -> &RoutineOccurrenceMemberState {
    aggregate
        .members
        .iter()
        .find(|member| member.item_id == id(target))
        .unwrap()
}

fn outcome(status: ItemStatus) -> RoutineOccurrenceAction {
    RoutineOccurrenceAction::SetOutcome { status }
}

fn policy(required: bool, mode: ItemCompletionMode) -> RoutineOccurrenceAction {
    RoutineOccurrenceAction::SetPolicy {
        required_for_parent: required,
        mode,
    }
}

#[test]
fn required_completion_and_correction_preserve_exact_blocked_ancestor_custody() {
    let mut optional = definition(4, Some(1));
    optional.required_for_parent = false;
    let initial = initialize_routine_occurrence(
        manifest(vec![
            definition(1, None),
            definition(2, Some(1)),
            definition(3, Some(2)),
            optional,
        ]),
        now(),
    )
    .unwrap();
    let immutable = initial.manifest.clone();
    let completed = apply(&initial, 3, outcome(ItemStatus::Completed));
    assert_eq!(completed.effects.len(), 3);
    assert_eq!(completed.snapshot.aggregate.revision, 2);
    for target in [1, 2, 3] {
        assert_eq!(
            member(&completed.snapshot.aggregate, target).status,
            ItemStatus::Completed
        );
        assert_eq!(member(&completed.snapshot.aggregate, target).revision, 2);
    }
    assert_eq!(
        member(&completed.snapshot.aggregate, 4),
        member(&initial, 4)
    );
    let root = member(&completed.snapshot.aggregate, 1);
    assert_eq!(
        root.provenance.as_ref().unwrap().reopen,
        open(ItemStatus::Blocked)
    );
    assert_eq!(
        root.provenance.as_ref().unwrap().kind,
        ItemCompletionProvenanceKind::Automatic
    );
    let corrected = apply(
        &completed.snapshot.aggregate,
        3,
        RoutineOccurrenceAction::Reopen {
            open: open(ItemStatus::Planned),
        },
    );
    assert_eq!(corrected.effects.len(), 3);
    assert_eq!(
        member(&corrected.snapshot.aggregate, 1).status,
        ItemStatus::Blocked
    );
    assert_eq!(
        member(&corrected.snapshot.aggregate, 1).open,
        open(ItemStatus::Blocked)
    );
    assert_eq!(
        member(&corrected.snapshot.aggregate, 2).status,
        ItemStatus::Planned
    );
    assert!(
        corrected
            .snapshot
            .aggregate
            .members
            .iter()
            .all(|member| member.provenance.is_none())
    );
    assert_eq!(corrected.snapshot.aggregate.manifest, immutable);
    assert_eq!(initial.revision, 1);
}

#[test]
fn manual_parent_completion_does_not_waive_descendants_or_stop_child_timer() {
    let initial = initialize_routine_occurrence(
        manifest(vec![
            definition(1, None),
            definition(2, Some(1)),
            definition(3, Some(2)),
        ]),
        now(),
    )
    .unwrap();
    let mut evidence = evidence(&initial);
    evidence.execution_revision = 4;
    evidence.live_work_units.insert(RoutineOccurrenceWorkUnit {
        item_id: id(3),
        occurrence_id: initial.manifest.occurrence_id,
    });
    let command = reviewed(
        &initial,
        &evidence,
        2,
        policy(true, ItemCompletionMode::Complete),
    );
    let result = plan_routine_occurrence(&initial, &evidence, id(2), &command, now()).unwrap();
    assert_eq!(result.effects.len(), 1);
    assert_eq!(
        member(&result.snapshot.aggregate, 2).status,
        ItemStatus::Completed
    );
    assert_eq!(
        member(&result.snapshot.aggregate, 1).status,
        ItemStatus::Blocked
    );
    assert_eq!(member(&result.snapshot.aggregate, 3), member(&initial, 3));
    let root = result
        .snapshot
        .members
        .iter()
        .find(|member| member.item_id == id(1))
        .unwrap();
    assert_eq!(root.counts.completed, 1);
    assert_eq!(root.counts.incomplete, 1);
    let released = apply(
        &result.snapshot.aggregate,
        2,
        policy(true, ItemCompletionMode::KeepOpen),
    );
    assert_eq!(
        member(&released.snapshot.aggregate, 2).status,
        ItemStatus::Planned
    );
    assert_eq!(
        member(&released.snapshot.aggregate, 2).mode,
        ItemCompletionMode::KeepOpen
    );
}

#[test]
fn skipped_and_cancelled_are_not_done_and_optional_empty_parent_does_not_complete() {
    let initial = initialize_routine_occurrence(
        manifest(vec![definition(1, None), definition(2, Some(1))]),
        now(),
    )
    .unwrap();
    let skipped = apply(&initial, 2, outcome(ItemStatus::Skipped));
    assert_eq!(
        member(&skipped.snapshot.aggregate, 1).status,
        ItemStatus::Blocked
    );
    assert_eq!(skipped.snapshot.members[0].counts.incomplete, 1);
    let mut cancelled = skipped.snapshot.aggregate.clone();
    cancelled
        .members
        .iter_mut()
        .find(|member| member.item_id == id(2))
        .unwrap()
        .status = ItemStatus::Cancelled;
    let snapshot = routine_occurrence_snapshot(&cancelled, &evidence(&cancelled)).unwrap();
    assert_eq!(snapshot.members[0].counts.incomplete, 1);
    let optional = apply(&initial, 2, policy(false, ItemCompletionMode::Automatic));
    assert_eq!(optional.snapshot.members[0].counts.required_descendants, 0);
    assert_eq!(
        member(&optional.snapshot.aggregate, 1).status,
        ItemStatus::Blocked
    );
}

#[test]
fn nested_recurrence_and_its_descendants_never_inherit_outer_occurrence_authority() {
    let mut nested = definition(2, Some(1));
    nested.recurs = true;
    let initial = initialize_routine_occurrence(
        manifest(vec![definition(1, None), nested, definition(3, Some(2))]),
        now(),
    )
    .unwrap();
    let evidence = evidence(&initial);
    let snapshot = routine_occurrence_snapshot(&initial, &evidence).unwrap();
    assert_eq!(snapshot.members[0].counts.occurrence_evidence_required, 2);
    for (target, action) in [
        (2, policy(true, ItemCompletionMode::Complete)),
        (3, outcome(ItemStatus::Completed)),
    ] {
        let command = reviewed(&initial, &evidence, target, action);
        assert_eq!(
            plan_routine_occurrence(&initial, &evidence, id(target), &command, now()),
            Err(RoutineOccurrenceError::OccurrenceEvidenceRequired)
        );
    }
}

#[test]
fn exact_live_member_is_fenced_but_other_occurrence_does_not_block() {
    let initial = initialize_routine_occurrence(
        manifest(vec![definition(1, None), definition(2, Some(1))]),
        now(),
    )
    .unwrap();
    let mut evidence = evidence(&initial);
    evidence.live_work_units.insert(RoutineOccurrenceWorkUnit {
        item_id: id(2),
        occurrence_id: id(999),
    });
    let command = reviewed(&initial, &evidence, 2, outcome(ItemStatus::Completed));
    assert!(plan_routine_occurrence(&initial, &evidence, id(2), &command, now()).is_ok());
    for live in [1, 2] {
        evidence.live_work_units.insert(RoutineOccurrenceWorkUnit {
            item_id: id(live),
            occurrence_id: initial.manifest.occurrence_id,
        });
        let command = reviewed(&initial, &evidence, 2, outcome(ItemStatus::Completed));
        assert_eq!(
            plan_routine_occurrence(&initial, &evidence, id(2), &command, now()),
            Err(RoutineOccurrenceError::ExecutionConflict)
        );
        evidence
            .live_work_units
            .retain(|unit| unit.occurrence_id != initial.manifest.occurrence_id);
    }
    assert_eq!(member(&initial, 2).status, ItemStatus::Planned);
}

#[test]
fn current_source_revisions_and_execution_fence_review_without_rebinding_manifest() {
    let initial = initialize_routine_occurrence(
        manifest(vec![definition(1, None), definition(2, Some(1))]),
        now(),
    )
    .unwrap();
    let original_evidence = evidence(&initial);
    let command = reviewed(
        &initial,
        &original_evidence,
        2,
        outcome(ItemStatus::Completed),
    );
    for execution_change in [false, true] {
        let mut changed = original_evidence.clone();
        if execution_change {
            changed.execution_revision = 1;
        } else {
            changed.sources[0].current_revision = Some(8);
        }
        assert_eq!(
            plan_routine_occurrence(&initial, &changed, id(2), &command, now()),
            Err(RoutineOccurrenceError::EvidenceStale)
        );
        let fresh = reviewed(&initial, &changed, 2, outcome(ItemStatus::Completed));
        let result = plan_routine_occurrence(&initial, &changed, id(2), &fresh, now()).unwrap();
        assert_eq!(result.snapshot.aggregate.manifest, initial.manifest);
    }
    let result = apply(&initial, 2, outcome(ItemStatus::Completed));
    assert_eq!(
        plan_routine_occurrence(
            &result.snapshot.aggregate,
            &original_evidence,
            id(2),
            &command,
            now()
        ),
        Err(RoutineOccurrenceError::InstanceStale)
    );
}

#[test]
fn stale_definition_and_missing_source_remain_readable_but_not_editable() {
    let initial = initialize_routine_occurrence(
        manifest(vec![definition(1, None), definition(2, Some(1))]),
        now(),
    )
    .unwrap();
    for definition_changed in [true, false] {
        let mut evidence = evidence(&initial);
        let expected = if definition_changed {
            evidence.current_definition_hash = format!("sha256:{}", "b".repeat(64));
            RoutineOccurrenceError::DefinitionChanged
        } else {
            evidence.sources[1].current_revision = None;
            evidence.sources[1].eligible = false;
            RoutineOccurrenceError::SourceIneligible
        };
        assert!(
            !routine_occurrence_snapshot(&initial, &evidence)
                .unwrap()
                .fresh_edit_eligible
        );
        let command = reviewed(&initial, &evidence, 2, outcome(ItemStatus::Completed));
        assert_eq!(
            plan_routine_occurrence(&initial, &evidence, id(2), &command, now()),
            Err(expected)
        );
    }
}

#[test]
fn completion_anchor_survives_policy_and_done_on_done_but_clears_on_correction() {
    let initial = initialize_routine_occurrence(
        manifest(vec![definition(1, None), definition(2, Some(1))]),
        now(),
    )
    .unwrap();
    let completed = apply(&initial, 2, outcome(ItemStatus::Completed))
        .snapshot
        .aggregate;
    for (target, action) in [
        (2, outcome(ItemStatus::Completed)),
        (1, policy(true, ItemCompletionMode::Complete)),
    ] {
        let evidence = evidence(&completed);
        let command = reviewed(&completed, &evidence, target, action);
        let later = now() + Duration::days(2) + Duration::nanoseconds(789);
        let result =
            plan_routine_occurrence(&completed, &evidence, id(target), &command, later).unwrap();
        for member in &result.snapshot.aggregate.members {
            assert_eq!(member.completed_at, Some(now()));
            assert!(member.updated_at.nanosecond().is_multiple_of(1_000));
        }
        assert_eq!(
            member(&result.snapshot.aggregate, target).updated_at,
            now() + Duration::days(2)
        );
    }
    let skipped = apply(&completed, 2, outcome(ItemStatus::Skipped));
    assert!(
        skipped
            .snapshot
            .aggregate
            .members
            .iter()
            .all(|member| member.completed_at.is_none())
    );
    let redone = apply(
        &skipped.snapshot.aggregate,
        2,
        outcome(ItemStatus::Completed),
    );
    assert!(
        redone
            .snapshot
            .aggregate
            .members
            .iter()
            .all(|member| member.completed_at == Some(now()))
    );
}

#[test]
fn recurring_task_is_supported_but_habit_and_terminal_initialization_are_rejected() {
    let mut single = definition(1, None);
    single.kind = ItemKind::Task;
    let initial = initialize_routine_occurrence(manifest(vec![single.clone()]), now()).unwrap();
    let completed = apply(&initial, 1, outcome(ItemStatus::Completed));
    assert_eq!(completed.effects.len(), 1);
    assert_eq!(
        member(&completed.snapshot.aggregate, 1).status,
        ItemStatus::Completed
    );
    assert!(
        member(&completed.snapshot.aggregate, 1)
            .provenance
            .is_none()
    );
    single.kind = ItemKind::Habit;
    assert_eq!(
        initialize_routine_occurrence(manifest(vec![single]), now()),
        Err(RoutineOccurrenceError::Invalid)
    );
    for status in [
        ItemStatus::Completed,
        ItemStatus::Skipped,
        ItemStatus::Cancelled,
        ItemStatus::Scheduled,
        ItemStatus::InProgress,
        ItemStatus::Paused,
    ] {
        let mut root = definition(1, None);
        root.initial_open = open(status);
        assert_eq!(
            initialize_routine_occurrence(manifest(vec![root]), now()),
            Err(RoutineOccurrenceError::Invalid)
        );
    }
}

#[test]
fn complete_input_and_evidence_order_do_not_change_snapshot_or_effects() {
    let initial = initialize_routine_occurrence(
        manifest(vec![
            definition(1, None),
            definition(2, Some(1)),
            definition(3, Some(2)),
        ]),
        now(),
    )
    .unwrap();
    let mut reordered = initial.clone();
    reordered.members.reverse();
    reordered.manifest.members.reverse();
    let mut sources = evidence(&initial);
    sources.sources.reverse();
    assert_eq!(
        routine_occurrence_snapshot(&initial, &evidence(&initial)),
        routine_occurrence_snapshot(&reordered, &sources)
    );
    let command = reviewed(&initial, &sources, 3, outcome(ItemStatus::Completed));
    assert_eq!(
        plan_routine_occurrence(&initial, &sources, id(3), &command, now()),
        plan_routine_occurrence(&reordered, &sources, id(3), &command, now())
    );
}

#[test]
fn five_thousand_level_occurrence_completes_and_reopens_iteratively() {
    let definitions = (1..=5_000)
        .rev()
        .map(|value| definition(value, (value > 1).then_some(value - 1)))
        .collect();
    let initial = initialize_routine_occurrence(manifest(definitions), now()).unwrap();
    let result = apply(&initial, 5_000, outcome(ItemStatus::Completed));
    assert_eq!(result.effects.len(), 5_000);
    assert_eq!(
        result.snapshot.members[0].counts.required_descendants,
        4_999
    );
    assert!(
        result
            .snapshot
            .aggregate
            .members
            .iter()
            .all(|member| member.status == ItemStatus::Completed)
    );
    let reopened = apply(
        &result.snapshot.aggregate,
        5_000,
        RoutineOccurrenceAction::Reopen {
            open: open(ItemStatus::Inbox),
        },
    );
    assert_eq!(reopened.effects.len(), 5_000);
    assert_eq!(
        member(&reopened.snapshot.aggregate, 1).open,
        open(ItemStatus::Blocked)
    );
    assert!(
        reopened
            .snapshot
            .aggregate
            .members
            .iter()
            .all(|member| member.completed_at.is_none())
    );
}

#[test]
fn malformed_topology_precision_and_resource_bounds_fail_closed() {
    let valid = manifest(vec![definition(1, None), definition(2, Some(1))]);
    let mut invalid = valid.clone();
    invalid.members.push(invalid.members[1].clone());
    assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::Invalid));
    invalid = valid.clone();
    invalid.members[1].parent_id = Some(id(999));
    assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::Invalid));
    invalid = valid.clone();
    invalid.members[0].parent_id = Some(id(2));
    assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::Invalid));
    invalid = valid.clone();
    invalid.nominal_start += Duration::nanoseconds(1);
    assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::Invalid));
    invalid = valid.clone();
    invalid.members = vec![definition(1, None); MAX_ROUTINE_OCCURRENCE_MEMBERS + 1];
    assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::TooLarge));
    invalid = valid;
    invalid.members = (1..=2_000)
        .map(|value| {
            let mut member = definition(value, (value > 1).then_some(1));
            member.title = "𐀀".repeat(500);
            member.initial_open = open(ItemStatus::Blocked);
            member.initial_open.blocked_reason = Some("𐀀".repeat(1_000));
            member
        })
        .collect();
    assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::TooLarge));
}

#[test]
fn closed_wire_requires_nullable_fields_and_rejects_unknown_action_keys() {
    let initial =
        initialize_routine_occurrence(manifest(vec![definition(1, None)]), now()).unwrap();
    let command = reviewed(
        &initial,
        &evidence(&initial),
        1,
        outcome(ItemStatus::Completed),
    );
    let mut command_json = serde_json::to_value(&command).unwrap();
    command_json["action"]["silent_template_write"] = json!(true);
    assert!(serde_json::from_value::<RoutineOccurrenceCommand>(command_json).is_err());
    for nullable in ["provenance", "completed_at"] {
        let mut value = serde_json::to_value(&initial.members[0]).unwrap();
        value.as_object_mut().unwrap().remove(nullable);
        assert!(serde_json::from_value::<RoutineOccurrenceMemberState>(value).is_err());
    }
    let mut value = serde_json::to_value(&initial.manifest).unwrap();
    value["identity"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<RoutineOccurrenceManifest>(value).is_err());
    let mut value = serde_json::to_value(&initial.manifest.members[0]).unwrap();
    value.as_object_mut().unwrap().remove("parent_id");
    assert!(serde_json::from_value::<RoutineOccurrenceMemberDefinition>(value).is_err());
}

#[test]
fn clipped_and_moved_effective_windows_preserve_independent_nominal_identity() {
    let original = manifest(vec![definition(1, None), definition(2, Some(1))]);
    let mut clipped = original.clone();
    clipped.window_start = original.nominal_start + Duration::minutes(10);
    clipped.window_end = original.nominal_end - Duration::minutes(10);
    let mut moved = original.clone();
    moved.window_start = original.nominal_start + Duration::days(2);
    moved.window_end = original.nominal_end + Duration::days(2);
    for manifest in [clipped, moved] {
        let initial = initialize_routine_occurrence(manifest.clone(), now()).unwrap();
        assert_eq!(initial.manifest, manifest);
        assert_eq!(initial.manifest.identity, original.identity);
        assert_eq!(initial.manifest.nominal_start, original.nominal_start);
        assert_eq!(initial.manifest.nominal_end, original.nominal_end);
        assert!(
            routine_occurrence_snapshot(&initial, &evidence(&initial))
                .unwrap()
                .fresh_edit_eligible
        );
    }
    for empty_window in [true, false] {
        let mut invalid = original.clone();
        if empty_window {
            invalid.window_end = invalid.window_start;
        } else {
            invalid.nominal_end = invalid.nominal_start;
        }
        assert_eq!(invalid.validate(), Err(RoutineOccurrenceError::Invalid));
    }
}
