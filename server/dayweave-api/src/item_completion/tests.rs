use super::*;
use crate::items::NewItem;
use serde_json::json;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-09T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn item(id: u128, parent: Option<u128>, status: ItemStatus) -> Item {
    let new: NewItem = serde_json::from_value(json!({
        "id": Uuid::from_u128(id), "is_sensitive": false, "kind": "task", "status": status,
        "title": "Completion fixture", "timezone_name": "UTC", "duration_seconds": 1800,
        "parent_id": parent.map(Uuid::from_u128),
    }))
    .unwrap();
    Item::new(new, now()).unwrap()
}

fn command(
    items: &[Item],
    states: &[ItemCompletionState],
    target: usize,
    mode: ItemCompletionMode,
) -> ItemCompletionCommand {
    ItemCompletionCommand {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        expected_item_revision: items[target].revision,
        expected_completion_revision: states
            .iter()
            .find(|state| state.item_id == items[target].id)
            .map_or(0, |state| state.revision),
        expected_evidence_hash: item_completion_evidence_hash(
            items,
            states,
            &ItemCompletionExecutionEvidence::default(),
        )
        .unwrap(),
        required_for_parent: true,
        mode,
        reopening: None,
    }
}

fn apply(items: &mut [Item], states: &mut Vec<ItemCompletionState>, plan: ItemCompletionPlan) {
    for effect in plan.effects {
        let id = effect.after_item.id;
        *items.iter_mut().find(|item| item.id == id).unwrap() = effect.after_item;
        states.retain(|state| state.item_id != effect.after_state.item_id);
        states.push(effect.after_state);
    }
}

#[test]
fn completion_retains_and_restores_exact_blocked_tuple() {
    let mut parent = item(1, None, ItemStatus::Planned);
    parent.status = ItemStatus::Blocked;
    parent.blocked_reason_kind = Some(BlockedReasonKind::Dependency);
    parent.blocked_by_item_id = Some(Uuid::from_u128(3));
    parent.blocked_reason = Some("Waiting for reviewed input".into());
    parent.is_executable = false;
    let before = ItemCompletionReopenState::from_item(&parent).unwrap();
    let mut items = vec![
        parent,
        item(2, Some(1), ItemStatus::Completed),
        item(3, None, ItemStatus::Planned),
    ];
    let mut states = Vec::new();
    let execution = ItemCompletionExecutionEvidence::default();
    let plan = plan_item_completion(&items, &states, &execution, None, now()).unwrap();
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(
        plan.effects[0]
            .after_state
            .provenance
            .as_ref()
            .unwrap()
            .reopen,
        before
    );
    assert!(plan.effects[0].after_item.blocked_reason.is_none());
    apply(&mut items, &mut states, plan);
    items[1].status = ItemStatus::Planned;
    items[1].completed_at = None;
    items[1].revision += 1;
    let plan = plan_item_completion(&items, &states, &execution, None, now()).unwrap();
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(
        ItemCompletionReopenState::from_item(&plan.effects[0].after_item).unwrap(),
        before
    );
    assert!(plan.effects[0].after_state.provenance.is_none());
    assert_eq!(plan.effects[0].after_state.revision, 2);
}

#[test]
fn completion_and_reopening_normalize_nanosecond_clock_precision() {
    let mut items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, Some(1), ItemStatus::Completed),
    ];
    items[0].is_executable = false;
    let mut states = Vec::new();
    let execution = ItemCompletionExecutionEvidence::default();
    let precise_now = now() + chrono::Duration::nanoseconds(123_456_789);
    let stored_now = now() + chrono::Duration::microseconds(123_456);
    let plan = plan_item_completion(&items, &states, &execution, None, precise_now).unwrap();
    assert_eq!(plan.effects.len(), 1);
    let effect = &plan.effects[0];
    assert_eq!(effect.after_item.updated_at, stored_now);
    assert_eq!(effect.after_item.completed_at, Some(stored_now));
    assert_eq!(effect.after_state.updated_at, Some(stored_now));
    assert!(effect.after_state.validate().is_ok());
    let mut invalid = effect.after_state.clone();
    invalid.updated_at = Some(precise_now);
    assert_eq!(invalid.validate(), Err(ItemCompletionError::Invalid));
    apply(&mut items, &mut states, plan);

    items[1].status = ItemStatus::Planned;
    items[1].completed_at = None;
    items[1].revision += 1;
    let plan = plan_item_completion(
        &items,
        &states,
        &execution,
        None,
        precise_now + chrono::Duration::seconds(1),
    )
    .unwrap();
    assert_eq!(plan.effects.len(), 1);
    let effect = &plan.effects[0];
    let reopened_at = stored_now + chrono::Duration::seconds(1);
    assert_eq!(effect.after_item.status, ItemStatus::Planned);
    assert_eq!(effect.after_item.updated_at, reopened_at);
    assert_eq!(effect.after_item.completed_at, None);
    assert_eq!(effect.after_state.updated_at, Some(reopened_at));
    assert!(effect.after_state.validate().is_ok());
}

#[test]
fn manual_policy_release_retains_custody_and_canonical_staleness() {
    let mut items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, Some(1), ItemStatus::Planned),
    ];
    items[0].is_executable = false;
    let mut states = Vec::new();
    let execution = ItemCompletionExecutionEvidence::default();
    let request = command(&items, &states, 0, ItemCompletionMode::Complete);
    let plan = plan_item_completion(
        &items,
        &states,
        &execution,
        Some((items[0].id, &request)),
        now(),
    )
    .unwrap();
    apply(&mut items, &mut states, plan);
    assert_eq!(items[0].status, ItemStatus::Completed);
    assert_eq!(states[0].mode, ItemCompletionMode::Complete);
    assert_eq!(
        states[0].provenance.as_ref().unwrap().kind,
        ItemCompletionProvenanceKind::Manual
    );
    let request = command(&items, &states, 0, ItemCompletionMode::Automatic);
    let plan = plan_item_completion(
        &items,
        &states,
        &execution,
        Some((items[0].id, &request)),
        now(),
    )
    .unwrap();
    assert_eq!(plan.effects[0].after_item.status, ItemStatus::Planned);
    assert_eq!(plan.effects[0].after_item.revision, 3);
    assert_eq!(plan.effects[0].after_state.revision, 2);
    assert!(plan.effects[0].after_state.provenance.is_none());
}

#[test]
fn policy_only_changes_also_invalidate_item_and_global_review() {
    let mut items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, Some(1), ItemStatus::Completed),
    ];
    let execution = ItemCompletionExecutionEvidence::default();
    let mut states = Vec::new();
    let request = command(&items, &states, 0, ItemCompletionMode::Complete);
    let plan = plan_item_completion(
        &items,
        &states,
        &execution,
        Some((items[0].id, &request)),
        now(),
    )
    .unwrap();
    apply(&mut items, &mut states, plan);
    let prior_hash = item_completion_evidence_hash(&items, &states, &execution).unwrap();
    let request = command(&items, &states, 0, ItemCompletionMode::Automatic);
    let plan = plan_item_completion(
        &items,
        &states,
        &execution,
        Some((items[0].id, &request)),
        now(),
    )
    .unwrap();
    assert_eq!(plan.effects.len(), 1);
    assert_eq!(
        plan.effects[0].before_item.status,
        plan.effects[0].after_item.status
    );
    assert_eq!(
        plan.effects[0]
            .after_state
            .provenance
            .as_ref()
            .unwrap()
            .kind,
        ItemCompletionProvenanceKind::Automatic
    );
    apply(&mut items, &mut states, plan);
    assert_ne!(
        item_completion_evidence_hash(&items, &states, &execution).unwrap(),
        prior_hash
    );
}

#[test]
fn complete_five_thousand_chain_then_reopen_without_recursive_walk() {
    let mut items = (1..=5_000)
        .map(|id| {
            let mut value = item(
                id,
                (id > 1).then_some(id - 1),
                if id == 5_000 {
                    ItemStatus::Completed
                } else {
                    ItemStatus::Planned
                },
            );
            value.is_executable = id == 5_000;
            value
        })
        .collect::<Vec<_>>();
    let mut states = Vec::new();
    let execution = ItemCompletionExecutionEvidence::default();
    let plan = plan_item_completion(&items, &states, &execution, None, now()).unwrap();
    assert_eq!(plan.effects.len(), 4_999);
    assert_eq!(
        plan.snapshots[&items[0].id].counts.required_descendants,
        4_999
    );
    apply(&mut items, &mut states, plan);
    items[4_999].status = ItemStatus::Planned;
    items[4_999].completed_at = None;
    items[4_999].revision += 1;
    let plan = plan_item_completion(&items, &states, &execution, None, now()).unwrap();
    assert_eq!(plan.effects.len(), 4_999);
    assert!(
        plan.effects
            .iter()
            .all(|effect| effect.after_item.status == ItemStatus::Planned
                && effect.after_state.provenance.is_none())
    );
}

#[test]
fn ambiguous_terminal_parent_requires_explicit_known_reopening() {
    let mut items = vec![
        item(1, None, ItemStatus::Completed),
        item(2, Some(1), ItemStatus::Planned),
    ];
    items[0].is_executable = false;
    let execution = ItemCompletionExecutionEvidence::default();
    assert_eq!(
        plan_item_completion(&items, &[], &execution, None, now()),
        Err(ItemCompletionError::ReopeningReviewRequired)
    );
    let mut request = command(&items, &[], 0, ItemCompletionMode::KeepOpen);
    request.reopening = Some(ItemCompletionReopenState {
        status: ItemStatus::Inbox,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    });
    let plan = plan_item_completion(
        &items,
        &[],
        &execution,
        Some((items[0].id, &request)),
        now(),
    )
    .unwrap();
    assert_eq!(plan.effects[0].after_item.status, ItemStatus::Inbox);
    assert_eq!(
        plan.effects[0].after_state.mode,
        ItemCompletionMode::KeepOpen
    );
}

#[test]
fn optional_branch_is_cut_without_forgiving_required_grandchildren() {
    let mut items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, Some(1), ItemStatus::Completed),
        item(3, Some(1), ItemStatus::Planned),
        item(4, Some(3), ItemStatus::Planned),
    ];
    items[0].is_executable = false;
    items[2].is_executable = false;
    let mut request = command(&items, &[], 2, ItemCompletionMode::Automatic);
    request.required_for_parent = false;
    let plan = plan_item_completion(
        &items,
        &[],
        &ItemCompletionExecutionEvidence::default(),
        Some((items[2].id, &request)),
        now(),
    )
    .unwrap();
    let root = plan
        .effects
        .iter()
        .find(|effect| effect.after_item.id == items[0].id)
        .unwrap();
    assert_eq!(root.after_item.status, ItemStatus::Completed);
    assert_eq!(plan.snapshots[&items[0].id].counts.required_descendants, 1);
    assert!(
        !plan
            .effects
            .iter()
            .any(|effect| effect.after_item.id == items[2].id
                && effect.after_item.status == ItemStatus::Completed)
    );
}

#[test]
fn unqualified_recurrence_never_becomes_template_completion() {
    let mut items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, Some(1), ItemStatus::Completed),
    ];
    items[0].is_executable = false;
    items[0].recurrence = Some(json!({"frequency":"daily"}));
    let execution = ItemCompletionExecutionEvidence::default();
    let plan = plan_item_completion(&items, &[], &execution, None, now()).unwrap();
    assert!(plan.effects.is_empty());
    assert!(plan.snapshots[&items[0].id].occurrence_evidence_required);
    let request = command(&items, &[], 0, ItemCompletionMode::Complete);
    assert_eq!(
        plan_item_completion(
            &items,
            &[],
            &execution,
            Some((items[0].id, &request)),
            now()
        ),
        Err(ItemCompletionError::OccurrenceEvidenceRequired)
    );
}

#[test]
fn evidence_covers_execution_revision_order_independently() {
    let items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, None, ItemStatus::Planned),
    ];
    let evidence = ItemCompletionExecutionEvidence::default();
    let expected = item_completion_evidence_hash(&items, &[], &evidence).unwrap();
    assert_eq!(
        expected,
        item_completion_evidence_hash(&[items[1].clone(), items[0].clone()], &[], &evidence)
            .unwrap()
    );
    let changed = ItemCompletionExecutionEvidence {
        revision: 1,
        ..evidence
    };
    assert_ne!(
        expected,
        item_completion_evidence_hash(&items, &[], &changed).unwrap()
    );
    let request = command(&items, &[], 0, ItemCompletionMode::Automatic);
    assert_eq!(
        plan_item_completion(&items, &[], &changed, Some((items[0].id, &request)), now()),
        Err(ItemCompletionError::EvidenceStale)
    );
}

#[test]
fn active_execution_prevents_derived_lifecycle_mutation() {
    let mut items = vec![
        item(1, None, ItemStatus::Planned),
        item(2, Some(1), ItemStatus::Completed),
    ];
    items[0].is_executable = false;
    let evidence = ItemCompletionExecutionEvidence {
        revision: 1,
        live_item_ids: [items[0].id].into(),
    };
    assert_eq!(
        plan_item_completion(&items, &[], &evidence, None, now()),
        Err(ItemCompletionError::ExecutionConflict)
    );
}

#[test]
fn strict_nullable_custody_and_known_blocker_limits() {
    let invalid = ItemCompletionState {
        revision: 1,
        updated_at: Some(now()),
        mode: ItemCompletionMode::Complete,
        ..ItemCompletionState::empty(Uuid::from_u128(1))
    };
    assert_eq!(invalid.validate(), Err(ItemCompletionError::Invalid));
    let mut value = serde_json::to_value(ItemCompletionState::empty(Uuid::from_u128(1))).unwrap();
    value.as_object_mut().unwrap().remove("provenance");
    assert!(serde_json::from_value::<ItemCompletionState>(value).is_err());
    let mut reopen = ItemCompletionReopenState {
        status: ItemStatus::Blocked,
        blocked_reason_kind: Some(BlockedReasonKind::Manual),
        blocked_by_item_id: None,
        blocked_reason: Some("x".repeat(1_000)),
    };
    assert!(reopen.validate(Uuid::from_u128(1)).is_ok());
    reopen.blocked_reason.as_mut().unwrap().push('x');
    assert_eq!(
        reopen.validate(Uuid::from_u128(1)),
        Err(ItemCompletionError::Invalid)
    );
    reopen.status = ItemStatus::InProgress;
    assert_eq!(
        reopen.validate(Uuid::from_u128(1)),
        Err(ItemCompletionError::ReopeningReviewRequired)
    );
}

#[test]
fn bounded_hash_writer_rejects_before_allocating_a_serialized_body() {
    let mut writer = BoundedDigest::new(8);
    assert_eq!(
        writer.serialize(&"nine bytes"),
        Err(ItemCompletionError::TooLarge)
    );
    let mut writer = BoundedDigest::new(3);
    assert!(writer.serialize(&"x").is_ok());
    assert_eq!(writer.remaining, 0);
}
