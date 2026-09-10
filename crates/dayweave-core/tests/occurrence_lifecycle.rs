use std::collections::BTreeSet;

use dayweave_core::*;
use time::{Duration, OffsetDateTime, macros::datetime};
use uuid::Uuid;

const START: OffsetDateTime = datetime!(2026-09-01 0:00 UTC);

fn id(value: u128) -> ItemId {
    ItemId(Uuid::from_u128(value))
}

fn item(value: u128, parent: Option<ItemId>) -> WorkItem {
    WorkItem {
        id: id(value),
        is_sensitive: false,
        revision: 1,
        title: format!("Synthetic occurrence member {value}"),
        kind: ItemKind::Task,
        status: WorkStatus::NotStarted,
        parent_id: parent,
        sibling_order: None,
        has_own_effort: false,
        has_children_outside_plan: false,
        goal_ids: BTreeSet::new(),
        priority: Priority {
            importance: 5,
            urgency: 5,
        },
        duration: Some(DurationEstimate::exact(5)),
        constraints: SchedulingConstraints::default(),
        split_policy: SplitPolicy::Indivisible,
        energy: None,
        tags: BTreeSet::new(),
        created_at: START,
        updated_at: START,
    }
}

fn request(days: i64) -> PlanRequest {
    let mut root = item(1, None);
    root.kind = ItemKind::Routine(RoutineSpec {
        ordered: false,
        recurrence: Some(Recurrence::Daily { times_per_day: 1 }),
    });
    root.duration = None;
    PlanRequest {
        as_of: START,
        horizon_start: START,
        horizon_end: START + Duration::days(days),
        items: vec![root, item(2, Some(id(1))), item(3, Some(id(1)))],
        availability: vec![AvailabilityWindow {
            start: START,
            end: START + Duration::days(days),
            contexts: BTreeSet::new(),
            location: None,
            energy: EnergyLevel::Deep,
        }],
        fixed_blocks: Vec::new(),
        previous_assignments: Vec::new(),
        config: SchedulerConfig::default(),
        recurrence_context: RecurrenceContext::default(),
    }
}

fn context(input: &PlanRequest) -> OccurrenceLifecycleContext {
    OccurrenceLifecycleContext {
        snapshot_revision: 1,
        instances: expand_occurrences(input)
            .unwrap()
            .into_iter()
            .map(|occurrence| OccurrenceLifecycleInstance {
                root_item_id: occurrence.series_item_id,
                occurrence_id: occurrence.id,
                identity: occurrence.identity,
                members: input
                    .items
                    .iter()
                    .map(|item| OccurrenceLifecycleMember {
                        item_id: item.id,
                        parent_id: if item.id == occurrence.series_item_id {
                            None
                        } else {
                            item.parent_id
                        },
                        source_revision: item.revision,
                        status: WorkStatus::NotStarted,
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[test]
fn reopening_an_occurrence_does_not_remove_a_current_template_wide_block() {
    let mut input = request(1);
    input.items[1].status = WorkStatus::Blocked;
    let mut lifecycle = context(&input);
    let blocked = Scheduler
        .plan_with_lifecycle(&input, &ExecutionPlanningContext::default(), &lifecycle)
        .unwrap();
    assert!(
        !blocked
            .blocks
            .iter()
            .any(|block| block.item_id == Some(id(2)))
    );
    input.items[1].status = WorkStatus::NotStarted;
    let unblocked = Scheduler
        .plan_with_lifecycle(&input, &ExecutionPlanningContext::default(), &lifecycle)
        .unwrap();
    assert!(
        unblocked
            .blocks
            .iter()
            .any(|block| block.item_id == Some(id(2)))
    );
    input.items[1].status = WorkStatus::Blocked;
    lifecycle.instances[0].members[1].status = WorkStatus::Completed;
    assert!(
        Scheduler
            .plan_with_lifecycle(&input, &ExecutionPlanningContext::default(), &lifecycle,)
            .is_ok()
    );
}

fn plan(input: &PlanRequest, lifecycle: &OccurrenceLifecycleContext) -> SchedulePlan {
    Scheduler
        .plan_with_lifecycle(input, &ExecutionPlanningContext::default(), lifecycle)
        .unwrap()
}

fn lifecycle_error(
    input: &PlanRequest,
    lifecycle: &OccurrenceLifecycleContext,
) -> OccurrenceLifecycleError {
    match Scheduler.plan_with_lifecycle(input, &ExecutionPlanningContext::default(), lifecycle) {
        Err(ScheduleError::InvalidOccurrenceLifecycle(error)) => error,
        result => panic!("expected lifecycle rejection, received {result:?}"),
    }
}

fn member_mut(
    instance: &mut OccurrenceLifecycleInstance,
    item_id: ItemId,
) -> &mut OccurrenceLifecycleMember {
    instance
        .members
        .iter_mut()
        .find(|member| member.item_id == item_id)
        .unwrap()
}

fn reservation(item_id: ItemId, occurrence_id: OccurrenceId) -> ExecutionPlanningContext {
    ExecutionPlanningContext {
        snapshot_revision: 1,
        work_units: vec![ExecutionWorkUnit {
            item_id,
            occurrence_id: Some(occurrence_id),
            progress_epoch: 1,
            credited_seconds: 0,
            disposition: None,
            used_session_indices: vec![0],
            reservations: vec![ExecutionReservation {
                session_index: 0,
                start: START,
                end: START + Duration::minutes(5),
                kind: ExecutionReservationKind::InFlight,
            }],
        }],
    }
}

#[test]
fn empty_context_is_byte_identical_to_existing_planning_entrypoints() {
    let input = request(2);
    let legacy = Scheduler.plan(&input).unwrap();
    let execution = ExecutionPlanningContext::default();
    for head in [0, 1, i64::MAX as u64] {
        let lifecycle = OccurrenceLifecycleContext {
            snapshot_revision: head,
            instances: Vec::new(),
        };
        assert_eq!(
            serde_json::to_vec(&legacy).unwrap(),
            serde_json::to_vec(&plan(&input, &lifecycle)).unwrap()
        );
        assert_eq!(
            legacy,
            Scheduler.plan_with_execution(&input, &execution).unwrap()
        );
    }
}

#[test]
fn completion_is_isolated_to_the_exact_member_and_occurrence() {
    let input = request(2);
    let original = input.clone();
    let mut lifecycle = context(&input);
    let first = lifecycle.instances[0].occurrence_id;
    let second = lifecycle.instances[1].occurrence_id;
    member_mut(&mut lifecycle.instances[0], id(2)).status = WorkStatus::Completed;
    let original_lifecycle = lifecycle.clone();
    let plan = plan(&input, &lifecycle);
    assert!(
        !plan
            .blocks
            .iter()
            .any(|block| block.item_id == Some(id(2)) && block.occurrence_id == Some(first))
    );
    assert!(
        plan.blocks
            .iter()
            .any(|block| block.item_id == Some(id(3)) && block.occurrence_id == Some(first))
    );
    assert!(
        plan.blocks
            .iter()
            .any(|block| block.item_id == Some(id(2)) && block.occurrence_id == Some(second))
    );
    assert_eq!(input, original);
    assert_eq!(lifecycle, original_lifecycle);
}

#[test]
fn completed_parent_keeps_unfinished_optional_or_manually_overridden_descendants() {
    let input = request(1);
    let mut lifecycle = context(&input);
    member_mut(&mut lifecycle.instances[0], id(1)).status = WorkStatus::Completed;
    member_mut(&mut lifecycle.instances[0], id(2)).status = WorkStatus::Completed;
    let plan = plan(&input, &lifecycle);
    assert_eq!(plan.blocks_for(id(3)).count(), 1);
    assert_eq!(plan.blocks_for(id(1)).count(), 0);
    assert_eq!(plan.blocks_for(id(2)).count(), 0);
    assert_eq!(plan.occurrences[0].state, OccurrenceState::Generated);
    assert!(
        plan.decisions
            .iter()
            .any(|decision| decision.item_id == id(1)
                && decision.kind == DecisionKind::TerminalItemIgnored)
    );
}

#[test]
fn projection_does_not_infer_parent_completion_from_completed_children() {
    let input = request(1);
    let mut lifecycle = context(&input);
    for item_id in [id(2), id(3)] {
        member_mut(&mut lifecycle.instances[0], item_id).status = WorkStatus::Completed;
    }
    let plan = plan(&input, &lifecycle);
    assert!(plan.blocks.is_empty());
    assert!(
        plan.decisions
            .iter()
            .any(|decision| decision.item_id == id(1)
                && decision.kind == DecisionKind::ContainerRolledUp)
    );
    assert_eq!(plan.occurrences[0].state, OccurrenceState::Generated);
}

#[test]
fn completed_member_still_proves_its_occurrence_local_dependency() {
    let mut input = request(2);
    input.items[2].constraints.dependencies.push(Dependency {
        item_id: id(2),
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });
    let mut lifecycle = context(&input);
    member_mut(&mut lifecycle.instances[0], id(2)).status = WorkStatus::Completed;
    member_mut(&mut lifecycle.instances[1], id(2)).status = WorkStatus::Skipped;
    let plan = plan(&input, &lifecycle);
    assert!(plan.blocks.iter().any(|block| block.item_id == Some(id(3))
        && block.occurrence_id == Some(lifecycle.instances[0].occurrence_id)));
    assert!(plan.unscheduled.iter().any(|work| work.item_id == id(3)
        && work.occurrence_id == Some(lifecycle.instances[1].occurrence_id)
        && work.reason == UnscheduledReason::DependencyUnavailable));
}

#[test]
fn root_can_have_an_external_canonical_parent_without_importing_it_into_the_instance() {
    let mut input = request(1);
    let lifecycle = context(&input);
    input.items[0].parent_id = Some(id(10));
    let mut ancestor = item(10, None);
    ancestor.kind = ItemKind::Project;
    ancestor.duration = None;
    input.items.push(ancestor);
    let plan = plan(&input, &lifecycle);
    assert_eq!(plan.blocks.len(), 2);
    assert_eq!(plan.occurrences.len(), 1);
}

#[test]
fn exact_occurrence_root_identity_and_source_revisions_are_required() {
    let input = request(1);
    let baseline = context(&input);
    let occurrence_id = baseline.instances[0].occurrence_id;
    let mut changed = baseline.clone();
    changed.instances[0].root_item_id = id(2);
    changed.instances[0].members[0].parent_id = Some(id(2));
    changed.instances[0].members[1].parent_id = None;
    assert_eq!(
        lifecycle_error(&input, &changed),
        OccurrenceLifecycleError::OccurrenceMismatch(occurrence_id)
    );
    let mut changed = baseline.clone();
    changed.instances[0].identity = RecurrenceOccurrenceIdentity::CalendarDay {
        date: START.date(),
        bucket_ordinal: 1,
    };
    assert_eq!(
        lifecycle_error(&input, &changed),
        OccurrenceLifecycleError::OccurrenceMismatch(occurrence_id)
    );
    let mut changed = baseline;
    member_mut(&mut changed.instances[0], id(2)).source_revision = 2;
    assert_eq!(
        lifecycle_error(&input, &changed),
        OccurrenceLifecycleError::SourceMismatch {
            occurrence_id,
            item_id: id(2)
        }
    );
}

#[test]
fn present_source_parent_and_complete_materialized_membership_must_match() {
    let input = request(1);
    let mut lifecycle = context(&input);
    let occurrence_id = lifecycle.instances[0].occurrence_id;
    member_mut(&mut lifecycle.instances[0], id(3)).parent_id = Some(id(2));
    assert_eq!(
        lifecycle_error(&input, &lifecycle),
        OccurrenceLifecycleError::SourceMismatch {
            occurrence_id,
            item_id: id(3)
        }
    );
    let mut lifecycle = context(&input);
    lifecycle.instances[0]
        .members
        .retain(|member| member.item_id != id(3));
    assert_eq!(
        lifecycle_error(&input, &lifecycle),
        OccurrenceLifecycleError::MissingMember {
            occurrence_id,
            item_id: id(3)
        }
    );
}

#[test]
fn omitted_canonical_members_are_not_injected_and_the_parent_cannot_become_a_false_leaf() {
    let mut input = request(1);
    let lifecycle = context(&input);
    input.items.truncate(1);
    input.items[0].has_children_outside_plan = true;
    input.items[0].has_own_effort = true;
    input.items[0].duration = Some(DurationEstimate::exact(30));
    let plan = plan(&input, &lifecycle);
    assert!(plan.blocks.is_empty());
    assert_eq!(plan.decisions.len(), 1);
    assert_eq!(plan.decisions[0].item_id, id(1));
    input.items[0].has_children_outside_plan = false;
    assert!(
        matches!(lifecycle_error(&input, &lifecycle), OccurrenceLifecycleError::SourceMismatch { item_id, .. } if item_id == id(1))
    );
}

#[test]
fn omitted_child_marker_also_rejects_a_context_that_claims_no_omitted_children() {
    let mut input = request(1);
    input.items[0].has_children_outside_plan = true;
    let lifecycle = context(&input);
    assert!(
        matches!(lifecycle_error(&input, &lifecycle), OccurrenceLifecycleError::SourceMismatch { item_id, .. } if item_id == id(1))
    );
}

#[test]
fn root_completed_skipped_or_paused_envelopes_conflict_instead_of_dropping_members() {
    let baseline = request(1);
    let lifecycle = context(&baseline);
    let occurrence_id = lifecycle.instances[0].occurrence_id;
    for envelope in [
        OccurrenceState::Completed,
        OccurrenceState::Skipped,
        OccurrenceState::Paused,
    ] {
        let mut input = baseline.clone();
        match envelope {
            OccurrenceState::Completed => {
                input
                    .recurrence_context
                    .completed_occurrence_ids
                    .insert(occurrence_id);
            }
            OccurrenceState::Skipped => {
                input
                    .recurrence_context
                    .exceptions
                    .push(RecurrenceException {
                        item_id: id(1),
                        selector: RecurrenceExceptionSelector::Occurrence { id: occurrence_id },
                        action: RecurrenceExceptionAction::Skip,
                    });
            }
            OccurrenceState::Paused => input.recurrence_context.pauses.push(RecurrencePause {
                item_id: id(1),
                start: START,
                end: input.horizon_end,
            }),
            OccurrenceState::Generated => unreachable!(),
        }
        let before = input.clone();
        assert_eq!(
            lifecycle_error(&input, &lifecycle),
            OccurrenceLifecycleError::OccurrenceMismatch(occurrence_id)
        );
        assert_eq!(input, before);
    }
}

#[test]
fn out_of_horizon_or_invented_occurrences_are_rejected_not_ignored() {
    let mut input = request(1);
    let mut lifecycle = context(&input);
    let original = lifecycle.instances[0].occurrence_id;
    input.horizon_start += Duration::days(1);
    input.horizon_end += Duration::days(1);
    input.as_of += Duration::days(1);
    input.availability[0].start += Duration::days(1);
    input.availability[0].end += Duration::days(1);
    assert_eq!(
        lifecycle_error(&input, &lifecycle),
        OccurrenceLifecycleError::OccurrenceMismatch(original)
    );
    let forged = OccurrenceId(Uuid::from_u128(999));
    lifecycle.instances[0].occurrence_id = forged;
    assert_eq!(
        lifecycle_error(&input, &lifecycle),
        OccurrenceLifecycleError::OccurrenceMismatch(forged)
    );
}

#[test]
fn malformed_revisions_identifiers_and_execution_owned_statuses_fail_closed() {
    let input = request(1);
    let baseline = context(&input);
    for revision in [0, (i64::MAX as u64) + 1, u64::MAX] {
        let mut invalid = baseline.clone();
        invalid.snapshot_revision = revision;
        assert_eq!(
            invalid.validate(),
            Err(OccurrenceLifecycleError::InvalidSnapshotRevision)
        );
        let mut invalid = baseline.clone();
        invalid.instances[0].members[1].source_revision = revision;
        assert!(matches!(
            invalid.validate(),
            Err(OccurrenceLifecycleError::InvalidMember { .. })
        ));
    }
    for status in [WorkStatus::Active, WorkStatus::Paused] {
        let mut invalid = baseline.clone();
        invalid.instances[0].members[1].status = status;
        assert!(matches!(
            lifecycle_error(&input, &invalid),
            OccurrenceLifecycleError::InvalidMember { .. }
        ));
    }
    for invalid_root in [true, false] {
        let mut invalid = baseline.clone();
        if invalid_root {
            invalid.instances[0].root_item_id = ItemId(Uuid::nil());
        } else {
            invalid.instances[0].occurrence_id = OccurrenceId(Uuid::nil());
        }
        assert!(matches!(
            invalid.validate(),
            Err(OccurrenceLifecycleError::InvalidInstance(_))
        ));
    }
    let mut invalid = baseline.clone();
    invalid.instances[0].members[1].item_id = ItemId(Uuid::nil());
    assert!(matches!(
        invalid.validate(),
        Err(OccurrenceLifecycleError::InvalidMember { .. })
    ));
    let mut invalid = baseline;
    invalid.instances[0].members[1].parent_id = Some(ItemId(Uuid::nil()));
    assert!(matches!(
        invalid.validate(),
        Err(OccurrenceLifecycleError::InvalidMember { .. })
    ));
}

#[test]
fn duplicates_empty_instances_and_incomplete_or_cyclic_trees_are_rejected() {
    let baseline = context(&request(1));
    let mut invalid = baseline.clone();
    invalid.instances.push(invalid.instances[0].clone());
    assert!(matches!(
        invalid.validate(),
        Err(OccurrenceLifecycleError::DuplicateInstance(_))
    ));
    let mut invalid = baseline.clone();
    let member = invalid.instances[0].members[1].clone();
    invalid.instances[0].members.push(member);
    assert!(matches!(
        invalid.validate(),
        Err(OccurrenceLifecycleError::DuplicateMember { .. })
    ));
    let mut invalid = baseline.clone();
    invalid.instances[0].members.clear();
    assert!(matches!(
        invalid.validate(),
        Err(OccurrenceLifecycleError::InvalidInstance(_))
    ));
    for parents in [
        [None, None, Some(id(1))],
        [None, Some(id(9)), Some(id(1))],
        [None, Some(id(2)), Some(id(1))],
        [None, Some(id(3)), Some(id(2))],
        [Some(id(2)), Some(id(1)), Some(id(1))],
    ] {
        let mut invalid = baseline.clone();
        for (member, parent) in invalid.instances[0].members.iter_mut().zip(parents) {
            member.parent_id = parent;
        }
        assert!(matches!(
            invalid.validate(),
            Err(OccurrenceLifecycleError::InvalidHierarchy(_))
        ));
    }
    let mut invalid = baseline;
    invalid.instances[0].members.remove(0);
    assert!(matches!(
        invalid.validate(),
        Err(OccurrenceLifecycleError::InvalidHierarchy(_))
    ));
}

#[test]
fn context_budget_counts_every_member_across_instances_and_accepts_exact_limit() {
    assert_eq!(MAX_OCCURRENCE_LIFECYCLE_MEMBERS, 10_000);
    let mut lifecycle = context(&request(2));
    for instance in &mut lifecycle.instances {
        instance.members = (1..=5_000)
            .map(|value| OccurrenceLifecycleMember {
                item_id: id(value),
                parent_id: (value != 1).then_some(id(1)),
                source_revision: 1,
                status: WorkStatus::NotStarted,
            })
            .collect();
    }
    assert_eq!(lifecycle.validate(), Ok(()));
    lifecycle.instances[1]
        .members
        .push(OccurrenceLifecycleMember {
            item_id: id(5_001),
            parent_id: Some(id(1)),
            source_revision: 1,
            status: WorkStatus::NotStarted,
        });
    assert_eq!(
        lifecycle.validate(),
        Err(OccurrenceLifecycleError::TooLarge { limit: 10_000 })
    );
}

#[test]
fn five_thousand_deep_members_are_validated_and_projected_iteratively() {
    let mut input = request(1);
    input.items.truncate(1);
    for value in 2..=5_000 {
        let mut member = item(value, Some(id(value - 1)));
        if value != 5_000 {
            member.duration = None;
        }
        input.items.push(member);
    }
    let mut lifecycle = context(&input);
    member_mut(&mut lifecycle.instances[0], id(1)).status = WorkStatus::Completed;
    let plan = plan(&input, &lifecycle);
    assert_eq!(plan.blocks.len(), 1);
    assert_eq!(plan.blocks[0].item_id, Some(id(5_000)));
    assert_eq!(
        plan.blocks[0].occurrence_id,
        Some(lifecycle.instances[0].occurrence_id)
    );
    input.items.reverse();
    lifecycle.instances[0].members.reverse();
    assert_eq!(
        plan,
        Scheduler
            .plan_with_lifecycle(&input, &ExecutionPlanningContext::default(), &lifecycle)
            .unwrap()
    );
}

#[test]
fn member_completion_cannot_remove_a_live_or_deferred_execution_reservation() {
    let input = request(1);
    let baseline = context(&input);
    let occurrence_id = baseline.instances[0].occurrence_id;
    for status in [
        WorkStatus::Completed,
        WorkStatus::Skipped,
        WorkStatus::Canceled,
        WorkStatus::Blocked,
    ] {
        for deferred in [false, true] {
            let mut lifecycle = baseline.clone();
            member_mut(&mut lifecycle.instances[0], id(2)).status = status;
            let mut execution = reservation(id(2), occurrence_id);
            if deferred {
                execution.work_units[0].reservations[0].session_index = 1;
                execution.work_units[0].reservations[0].kind =
                    ExecutionReservationKind::DeferredReplacement {
                        source_session_index: 0,
                    };
            }
            assert!(
                matches!(Scheduler.plan_with_lifecycle(&input, &execution, &lifecycle),
                Err(ScheduleError::InvalidOccurrenceLifecycle(OccurrenceLifecycleError::ExecutionConflict { item_id, occurrence_id: rejected }))
                    if item_id == id(2) && rejected == occurrence_id)
            );
        }
    }
}

#[test]
fn completed_parent_does_not_cancel_its_open_childs_execution() {
    let input = request(1);
    let mut lifecycle = context(&input);
    member_mut(&mut lifecycle.instances[0], id(1)).status = WorkStatus::Completed;
    let occurrence_id = lifecycle.instances[0].occurrence_id;
    let execution = reservation(id(2), occurrence_id);
    let plan = Scheduler
        .plan_with_lifecycle(&input, &execution, &lifecycle)
        .unwrap();
    assert!(plan.blocks.iter().any(|block| block.item_id == Some(id(2))
        && block.occurrence_id == Some(occurrence_id)
        && block.kind == ScheduleBlockKind::Pinned));
    assert_eq!(plan.blocks_for(id(3)).count(), 1);
}

#[test]
fn conflicting_execution_skip_is_not_allowed_to_overwrite_authoritative_completion() {
    let input = request(1);
    let mut lifecycle = context(&input);
    member_mut(&mut lifecycle.instances[0], id(2)).status = WorkStatus::Completed;
    let occurrence_id = lifecycle.instances[0].occurrence_id;
    let mut execution = reservation(id(2), occurrence_id);
    execution.work_units[0].reservations.clear();
    execution.work_units[0].disposition = Some(ExecutionDisposition::Skipped);
    assert!(matches!(
        Scheduler.plan_with_lifecycle(&input, &execution, &lifecycle),
        Err(ScheduleError::InvalidOccurrenceLifecycle(
            OccurrenceLifecycleError::ExecutionConflict { .. }
        ))
    ));
    member_mut(&mut lifecycle.instances[0], id(2)).status = WorkStatus::Skipped;
    let plan = Scheduler
        .plan_with_lifecycle(&input, &execution, &lifecycle)
        .unwrap();
    assert_eq!(plan.blocks_for(id(2)).count(), 0);
    assert_eq!(plan.blocks_for(id(3)).count(), 1);
}

#[test]
fn defer_uses_the_same_lifecycle_context_and_empty_context_preserves_legacy_assessment() {
    let input = request(1);
    let mut lifecycle = context(&input);
    let occurrence_id = lifecycle.instances[0].occurrence_id;
    let execution = reservation(id(2), occurrence_id);
    let candidate = DeferCandidateAssessmentInput {
        placement_id: Uuid::from_u128(900),
        item_id: id(2),
        occurrence_id: Some(occurrence_id),
        source_session_index: 0,
        replacement_session_index: 1,
        credited_seconds_after_source: 0,
        move_start: START + Duration::minutes(10),
        move_end: START + Duration::minutes(15),
    };
    let legacy = Scheduler
        .assess_defer_candidate(&input, &execution, &candidate)
        .unwrap();
    let empty = Scheduler
        .assess_defer_candidate_with_lifecycle(
            &input,
            &execution,
            &OccurrenceLifecycleContext::default(),
            &candidate,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&legacy).unwrap(),
        serde_json::to_vec(&empty).unwrap()
    );
    member_mut(&mut lifecycle.instances[0], id(1)).status = WorkStatus::Completed;
    assert!(
        Scheduler
            .assess_defer_candidate_with_lifecycle(&input, &execution, &lifecycle, &candidate)
            .is_ok()
    );
    member_mut(&mut lifecycle.instances[0], id(2)).status = WorkStatus::Completed;
    assert!(matches!(
        Scheduler.assess_defer_candidate_with_lifecycle(&input, &execution, &lifecycle, &candidate),
        Err(ScheduleError::InvalidOccurrenceLifecycle(
            OccurrenceLifecycleError::ExecutionConflict { .. }
        ))
    ));
}
