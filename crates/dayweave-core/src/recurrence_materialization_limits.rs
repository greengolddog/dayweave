use std::collections::{BTreeMap, BTreeSet};

use time::{Duration, OffsetDateTime, macros::datetime};
use uuid::Uuid;

use super::{
    MAX_RECURRENCE_MATERIALIZED_ITEMS, RecurrenceError, checked_materialized_item_count,
    children_by_parent, collect_subtree, expand_occurrences, materialize_recurrences,
    minimum_spacing, recurrence_roots,
};
use crate::{
    AvailabilityWindow, ConstraintStrength, DeferCandidateAssessmentInput, Dependency,
    DependencyRelation, DurationEstimate, EnergyLevel, ExecutionPlanningContext,
    ExecutionReservation, ExecutionReservationKind, ExecutionWorkUnit, HabitMissedPolicy,
    HabitSpec, HierarchyError, ItemId, ItemKind, Minutes, OccurrenceId, OccurrenceState,
    PlanRequest, Priority, Recurrence, RecurrenceContext, RecurrenceException,
    RecurrenceExceptionAction, RecurrenceExceptionSelector, RecurrencePause, RecurrencePeriod,
    RecurrenceSemantics, RoutineSpec, ScheduleError, Scheduler, SchedulerConfig,
    SchedulingConstraints, SplitPolicy, WorkItem, WorkStatus,
};

const START: OffsetDateTime = datetime!(2026-09-01 0:00 UTC);

fn id(value: usize) -> ItemId {
    ItemId(Uuid::from_u128(value as u128))
}

fn item(value: usize) -> WorkItem {
    WorkItem {
        id: id(value),
        is_sensitive: false,
        revision: 1,
        title: format!("Synthetic member {value}"),
        kind: ItemKind::Task,
        status: WorkStatus::NotStarted,
        parent_id: None,
        sibling_order: None,
        has_own_effort: false,
        has_children_outside_plan: false,
        goal_ids: BTreeSet::new(),
        priority: Priority {
            importance: 5,
            urgency: 5,
        },
        duration: Some(DurationEstimate::exact(1)),
        constraints: SchedulingConstraints::default(),
        split_policy: SplitPolicy::Indivisible,
        energy: None,
        tags: BTreeSet::new(),
        created_at: START,
        updated_at: START,
    }
}

fn chain(first: usize, size: usize, times_per_day: u16) -> Vec<WorkItem> {
    let mut items = Vec::with_capacity(size);
    for index in 0..size {
        let mut member = item(first + index);
        if index == 0 {
            member.kind = ItemKind::Routine(RoutineSpec {
                ordered: true,
                recurrence: Some(Recurrence::Daily { times_per_day }),
            });
        } else {
            member.parent_id = Some(id(first + index - 1));
        }
        if index + 1 < size {
            member.duration = None;
        }
        items.push(member);
    }
    items
}

fn request(items: Vec<WorkItem>) -> PlanRequest {
    PlanRequest {
        as_of: START,
        horizon_start: START,
        horizon_end: START + Duration::days(1),
        items,
        availability: Vec::new(),
        fixed_blocks: Vec::new(),
        previous_assignments: Vec::new(),
        config: SchedulerConfig::default(),
        recurrence_context: RecurrenceContext::default(),
    }
}

fn limit_error() -> RecurrenceError {
    RecurrenceError::MaterializedItemLimitExceeded {
        limit: MAX_RECURRENCE_MATERIALIZED_ITEMS,
    }
}

#[test]
fn checked_count_accepts_exact_budget_and_rejects_products_sums_and_retained_overflow() {
    assert_eq!(MAX_RECURRENCE_MATERIALIZED_ITEMS, 10_000);
    assert_eq!(checked_materialized_item_count(0, [(5_000, 2)]), Ok(10_000));
    assert_eq!(
        checked_materialized_item_count(9_990, [(2, 2), (2, 3)]),
        Ok(10_000)
    );
    assert_eq!(
        checked_materialized_item_count(10_000, [(usize::MAX, 0)]),
        Ok(10_000)
    );
    for (retained, subtrees) in [
        (0, vec![(5_001, 2)]),
        (9_991, vec![(2, 2), (2, 3)]),
        (10_001, Vec::new()),
        (0, vec![(usize::MAX, 2)]),
        (1, vec![(usize::MAX, 1)]),
    ] {
        assert_eq!(
            checked_materialized_item_count(retained, subtrees),
            Err(limit_error())
        );
    }
}

#[test]
fn actual_aggregate_includes_independent_roots_and_retained_nonrecurring_items() {
    let mut items = chain(1, 2, 2);
    items.extend(chain(10, 2, 3));
    items.extend((100..10_090).map(item));
    let mut input = request(items);
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(
        materialized.request.items.len(),
        MAX_RECURRENCE_MATERIALIZED_ITEMS
    );
    assert_eq!(materialized.identities.len(), 10);
    assert_eq!(materialized.occurrences.len(), 5);
    drop(materialized);

    input.items.push(item(20_000));
    assert!(matches!(materialize_recurrences(&input), Err(error) if error == limit_error()));
    // The public scheduler still exposes its established recurrence error category.
    assert!(
        matches!(Scheduler.plan(&input), Err(ScheduleError::InvalidRecurrence(message))
        if message == limit_error().to_string())
    );
}

#[test]
fn materialization_limit_also_applies_without_any_recurring_root() {
    let mut input = request((1..=MAX_RECURRENCE_MATERIALIZED_ITEMS).map(item).collect());
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(materialized.request.items, input.items);
    assert!(materialized.identities.is_empty());
    assert!(materialized.occurrences.is_empty());
    drop(materialized);
    input
        .items
        .push(item(MAX_RECURRENCE_MATERIALIZED_ITEMS + 1));
    assert!(matches!(materialize_recurrences(&input), Err(error) if error == limit_error()));
}

#[test]
fn suppressed_occurrences_do_not_consume_clone_budget() {
    let mut items = chain(1, 3_000, 4);
    items.push(item(20_000));
    let mut input = request(items);
    assert!(matches!(materialize_recurrences(&input), Err(error) if error == limit_error()));
    let occurrences = expand_occurrences(&input).unwrap();
    assert_eq!(occurrences.len(), 4);
    input
        .recurrence_context
        .completed_occurrence_ids
        .insert(occurrences[0].id);
    input
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: id(1),
            selector: RecurrenceExceptionSelector::Occurrence {
                id: occurrences[1].id,
            },
            action: RecurrenceExceptionAction::Skip,
        });
    input.recurrence_context.pauses.push(RecurrencePause {
        item_id: id(1),
        start: occurrences[2].window_start,
        end: occurrences[2].window_end,
    });
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(materialized.request.items.len(), 3_001);
    assert_eq!(materialized.identities.len(), 3_000);
    assert_eq!(
        materialized
            .occurrences
            .iter()
            .map(|value| value.state)
            .collect::<Vec<_>>(),
        vec![
            OccurrenceState::Completed,
            OccurrenceState::Skipped,
            OccurrenceState::Paused,
            OccurrenceState::Generated
        ]
    );
    assert!(
        materialized
            .identities
            .values()
            .all(|identity| identity.occurrence_id == occurrences[3].id)
    );
}

#[test]
fn zero_generated_occurrences_leave_only_retained_items() {
    let mut items = chain(1, 5_000, 1);
    items.push(item(20_000));
    let mut input = request(items);
    let occurrence = expand_occurrences(&input).unwrap()[0];
    input
        .recurrence_context
        .completed_occurrence_ids
        .insert(occurrence.id);
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(materialized.request.items, vec![item(20_000)]);
    assert!(materialized.identities.is_empty());
    assert_eq!(
        materialized.occurrences[0].state,
        OccurrenceState::Completed
    );

    let ItemKind::Routine(spec) = &mut input.items[0].kind else {
        unreachable!()
    };
    spec.recurrence = Some(Recurrence::AfterCompletion {
        interval: Minutes(2 * 24 * 60),
    });
    input.recurrence_context.completed_occurrence_ids.clear();
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(materialized.request.items, vec![item(20_000)]);
    assert!(materialized.identities.is_empty());
    assert!(materialized.occurrences.is_empty());
}

#[test]
fn five_thousand_deep_routine_materializes_stable_members_without_stack_recursion() {
    let input = request(chain(1, 5_000, 1));
    let occurrence_id = OccurrenceId(Uuid::new_v5(&id(1).0, b"daily:2026-09-01:0"));
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(materialized.request.items.len(), 5_000);
    assert_eq!(materialized.identities.len(), 5_000);
    assert_eq!(materialized.occurrences[0].id, occurrence_id);
    let by_id: BTreeMap<_, _> = materialized
        .request
        .items
        .iter()
        .map(|item| (item.id, item))
        .collect();
    let mut expected_parent = None;
    for original in &input.items {
        let clone_id = if original.id == id(1) {
            ItemId(occurrence_id.0)
        } else {
            ItemId(Uuid::new_v5(&original.id.0, occurrence_id.0.as_bytes()))
        };
        let cloned = by_id[&clone_id];
        assert_eq!(cloned.parent_id, expected_parent);
        assert_eq!(cloned.kind, original.kind);
        assert_eq!(cloned.status, original.status);
        assert_eq!(
            materialized.identities[&clone_id].series_item_id,
            original.id
        );
        assert_eq!(
            materialized.identities[&clone_id].occurrence_id,
            occurrence_id
        );
        expected_parent = Some(clone_id);
    }

    let mut reversed = input.clone();
    reversed.items.reverse();
    let reversed = materialize_recurrences(&reversed).unwrap();
    assert_eq!(materialized.request, reversed.request);
    assert_eq!(materialized.occurrences, reversed.occurrences);
    // Exercise the complete public core entry point, not only the private walk.
    let plan = Scheduler.plan(&input).unwrap();
    assert_eq!(plan.occurrences, materialized.occurrences);
    assert_eq!(plan.unscheduled.len(), 1);
    assert_eq!(plan.unscheduled[0].item_id, id(5_000));
    assert_eq!(plan.unscheduled[0].occurrence_id, Some(occurrence_id));
}

#[test]
fn branching_preorder_and_cloned_dependency_identity_remain_unchanged() {
    let mut items = chain(1, 5, 1);
    items[2].parent_id = Some(id(2));
    items[3].parent_id = Some(id(1));
    items[4].parent_id = Some(id(2));
    items[3].constraints.dependencies.push(Dependency {
        item_id: id(3),
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes(7),
        strength: ConstraintStrength::Hard,
    });
    let input = request(items);
    let children = children_by_parent(&input.items);
    assert_eq!(
        collect_subtree(id(1), &children),
        vec![id(1), id(2), id(3), id(5), id(4)]
    );
    let materialized = materialize_recurrences(&input).unwrap();
    let occurrence_id = materialized.occurrences[0].id;
    let clone_id = |source: ItemId| ItemId(Uuid::new_v5(&source.0, occurrence_id.0.as_bytes()));
    let cloned_successor = materialized
        .request
        .items
        .iter()
        .find(|item| item.id == clone_id(id(4)))
        .unwrap();
    assert_eq!(cloned_successor.parent_id, Some(ItemId(occurrence_id.0)));
    assert_eq!(
        cloned_successor.constraints.dependencies,
        vec![Dependency {
            item_id: clone_id(id(3)),
            relation: DependencyRelation::FinishToStart,
            minimum_lag: Minutes(7),
            strength: ConstraintStrength::Hard,
        }]
    );
    let mut reversed = input;
    reversed.items.reverse();
    assert_eq!(
        materialized.request,
        materialize_recurrences(&reversed).unwrap().request
    );
}

#[test]
fn ordered_routine_still_schedules_siblings_by_their_original_order() {
    let mut items = chain(1, 3, 1);
    items[1].parent_id = Some(id(1));
    items[1].sibling_order = Some(2);
    items[1].duration = Some(DurationEstimate::exact(5));
    items[2].parent_id = Some(id(1));
    items[2].sibling_order = Some(1);
    items[2].duration = Some(DurationEstimate::exact(5));
    let mut input = request(items);
    input.availability.push(AvailabilityWindow {
        start: START,
        end: START + Duration::hours(1),
        contexts: BTreeSet::new(),
        location: None,
        energy: EnergyLevel::Deep,
    });
    let plan = Scheduler.plan(&input).unwrap();
    let first = plan.blocks_for(id(3)).next().unwrap();
    let second = plan.blocks_for(id(2)).next().unwrap();
    assert!(first.end <= second.start);
    assert_eq!(first.occurrence_id, second.occurrence_id);
    input.items.reverse();
    assert_eq!(plan, Scheduler.plan(&input).unwrap());
}

#[test]
fn root_classification_preserves_source_order_and_nested_recurrence_boundary() {
    let outer = chain(1, 1, 1).remove(0);
    let mut nested = chain(2, 1, 2).remove(0);
    nested.parent_id = Some(id(1));
    let context = item(10);
    let mut independent = chain(11, 1, 1).remove(0);
    independent.parent_id = Some(id(10));
    let input = request(vec![nested, independent, context, outer]);
    let by_id = input.items.iter().map(|item| (item.id, item)).collect();
    let children = children_by_parent(&input.items);
    assert_eq!(
        recurrence_roots(&input.items, &by_id, &children),
        vec![id(11), id(1)]
    );
    let occurrences = expand_occurrences(&input).unwrap();
    assert_eq!(occurrences.len(), 2);
    assert_eq!(
        occurrences
            .iter()
            .map(|value| value.series_item_id)
            .collect::<Vec<_>>(),
        vec![id(1), id(11)]
    );
    let materialized = materialize_recurrences(&input).unwrap();
    assert_eq!(materialized.request.items.len(), 4);
    let nested_identity = materialized
        .identities
        .values()
        .find(|identity| identity.series_item_id == id(2))
        .unwrap();
    assert_eq!(nested_identity.occurrence_id, occurrences[0].id);
}

#[test]
fn public_expansion_and_scheduler_reject_invalid_topology_before_traversal() {
    let valid = request(chain(1, 3, 1));
    let mut duplicate = valid.clone();
    duplicate.items.push(duplicate.items[0].clone());
    let mut missing = valid.clone();
    missing.items[0].parent_id = Some(id(9));
    let mut self_cycle = valid.clone();
    self_cycle.items[0].parent_id = Some(id(1));
    let mut cycle = valid.clone();
    cycle.items[0].parent_id = Some(id(3));
    // The recurring node is below an entirely nonrecurring parent cycle. The
    // old repeated ancestor walk could loop forever before reaching expansion.
    let mut ancestral_cycle = request(vec![item(1), item(2), valid.items[0].clone()]);
    ancestral_cycle.items[0].parent_id = Some(id(2));
    ancestral_cycle.items[1].parent_id = Some(id(1));
    ancestral_cycle.items[2].id = id(3);
    ancestral_cycle.items[2].parent_id = Some(id(2));
    for (input, error) in [
        (duplicate, HierarchyError::DuplicateItem(id(1))),
        (
            missing,
            HierarchyError::MissingParent {
                item: id(1),
                parent: id(9),
            },
        ),
        (self_cycle, HierarchyError::Cycle(id(1))),
        (cycle, HierarchyError::Cycle(id(1))),
        (ancestral_cycle, HierarchyError::Cycle(id(1))),
    ] {
        assert_eq!(
            expand_occurrences(&input),
            Err(RecurrenceError::InvalidHierarchy(error))
        );
        assert!(matches!(
            Scheduler.plan(&input),
            Err(ScheduleError::InvalidHierarchy(_) | ScheduleError::DuplicateItem(_))
        ));
    }
}

#[test]
fn direct_item_spacing_preserves_explicit_override_and_habit_maximum() {
    let mut habit = item(1);
    habit.kind = ItemKind::Habit(HabitSpec {
        recurrence: Recurrence::Frequency {
            target: 2,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Calendar,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes(7),
            anchor: None,
        },
        target: None,
        preserves_streak_when_paused: true,
        missed_policy: HabitMissedPolicy::Ask,
        minimum_spacing: Minutes(11),
    });
    let mut input = request(vec![habit.clone()]);
    assert_eq!(minimum_spacing(&input, &habit), Minutes(11));
    input
        .recurrence_context
        .minimum_spacing
        .insert(habit.id, Minutes(3));
    assert_eq!(minimum_spacing(&input, &habit), Minutes(3));
    input.recurrence_context.minimum_spacing.clear();
    let ItemKind::Habit(spec) = &mut habit.kind else {
        unreachable!()
    };
    spec.minimum_spacing = Minutes(2);
    assert_eq!(minimum_spacing(&input, &habit), Minutes(7));
    assert_eq!(minimum_spacing(&input, &item(2)), Minutes::ZERO);
}

#[test]
fn defer_assessment_cannot_bypass_the_aggregate_materialization_limit() {
    let mut items = chain(1, 5_000, 2);
    let mut source = item(20_000);
    source.duration = Some(DurationEstimate::exact(5));
    items.push(source.clone());
    let input = request(items);
    let execution = ExecutionPlanningContext {
        snapshot_revision: 1,
        work_units: vec![ExecutionWorkUnit {
            item_id: source.id,
            occurrence_id: None,
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
    };
    let candidate = DeferCandidateAssessmentInput {
        placement_id: Uuid::from_u128(30_000),
        item_id: source.id,
        occurrence_id: None,
        source_session_index: 0,
        replacement_session_index: 1,
        credited_seconds_after_source: 0,
        move_start: START + Duration::minutes(10),
        move_end: START + Duration::minutes(15),
    };
    assert!(
        matches!(Scheduler.assess_defer_candidate(&input, &execution, &candidate),
        Err(ScheduleError::InvalidRecurrence(message)) if message == limit_error().to_string())
    );
}
