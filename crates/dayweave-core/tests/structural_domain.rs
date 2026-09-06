use std::collections::BTreeSet;

use dayweave_core::*;
use time::{Month, macros::datetime};
use uuid::Uuid;

fn id(value: u128) -> ItemId {
    ItemId::from_uuid(Uuid::from_u128(value))
}

fn item(duration: Option<DurationEstimate>) -> WorkItem {
    let now = datetime!(2026-09-03 8:00 UTC);
    WorkItem {
        id: id(1),
        is_sensitive: false,
        revision: 1,
        title: "Structural fixture".to_owned(),
        kind: ItemKind::Task,
        status: WorkStatus::NotStarted,
        parent_id: None,
        sibling_order: None,
        has_own_effort: false,
        has_children_outside_plan: false,
        goal_ids: BTreeSet::new(),
        priority: Priority::NONE,
        duration,
        constraints: SchedulingConstraints::default(),
        split_policy: SplitPolicy::Indivisible,
        energy: None,
        tags: BTreeSet::new(),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn duration_shapes_keep_unknown_exact_and_range_distinct() {
    assert_eq!(item(None).duration_kind(), DurationKind::Unknown);

    let exact = DurationEstimate::try_exact(Minutes(45), EstimateSource::Learned).unwrap();
    assert_eq!(exact.kind(), DurationKind::Exact);
    assert_eq!(exact.planning_minutes(), Minutes(45));
    assert_eq!(item(Some(exact)).duration_kind(), DurationKind::Exact);

    let ranged =
        DurationEstimate::try_range(Minutes(30), Minutes(45), Minutes(75), EstimateSource::Ai)
            .unwrap();
    assert_eq!(ranged.kind(), DurationKind::Range);
    assert_eq!(ranged.planning_minutes(), Minutes(45));
    assert_eq!(
        ranged
            .try_with_remaining(Minutes(20))
            .unwrap()
            .planning_minutes(),
        Minutes(20)
    );
}

#[test]
fn duration_constructors_enforce_range_invariants() {
    assert_eq!(
        DurationEstimate::try_exact(Minutes::ZERO, EstimateSource::User),
        Err(DurationEstimateError::ZeroMinimum)
    );
    assert_eq!(
        DurationEstimate::try_range(Minutes(30), Minutes(20), Minutes(60), EstimateSource::User,),
        Err(DurationEstimateError::InvalidRange)
    );
    assert_eq!(
        DurationEstimate::try_range(Minutes(30), Minutes(30), Minutes(30), EstimateSource::User,),
        Err(DurationEstimateError::RangeMustVary)
    );
    assert!(
        DurationEstimate::try_range(Minutes(30), Minutes(30), Minutes(60), EstimateSource::User,)
            .is_ok()
    );
    assert!(
        DurationEstimate::try_range(Minutes(30), Minutes(60), Minutes(60), EstimateSource::User,)
            .is_ok()
    );
    assert_eq!(
        DurationEstimate {
            minimum: Minutes(30),
            expected: Minutes(30),
            maximum: Minutes(30),
            remaining: None,
            source: EstimateSource::User,
        }
        .validate(),
        Ok(())
    );
    let estimate = DurationEstimate::try_range(
        Minutes(20),
        Minutes(30),
        Minutes(40),
        EstimateSource::Imported,
    )
    .unwrap();
    assert_eq!(
        estimate.try_with_remaining(Minutes(41)),
        Err(DurationEstimateError::RemainingExceedsMaximum)
    );
}

#[test]
fn deadline_round_trip_preserves_date_time_and_strength() {
    let date = time::Date::from_calendar_date(2026, Month::September, 30).unwrap();
    let date_deadline = Deadline::date(date, ConstraintStrength::Soft { weight: 240 });
    let encoded = serde_json::to_value(date_deadline).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({
            "type": "date",
            "date": "2026-09-30",
            "strength": {"level": "soft", "weight": 240}
        })
    );
    assert_eq!(
        serde_json::from_value::<Deadline>(encoded).unwrap(),
        date_deadline
    );

    let instant = datetime!(2026-09-30 17:30 +02:00);
    let timed = Deadline::date_time(instant, ConstraintStrength::Hard);
    assert_eq!(
        serde_json::from_value::<Deadline>(serde_json::to_value(timed).unwrap()).unwrap(),
        timed
    );
    assert_eq!(timed.strength(), Some(ConstraintStrength::Hard));
    assert!(Deadline::default().is_none());

    assert_eq!(date_deadline.validate(), Ok(()));
    assert_eq!(
        Deadline::date(
            date,
            ConstraintStrength::Soft {
                weight: MAX_SOFT_CONSTRAINT_WEIGHT,
            },
        )
        .validate(),
        Ok(())
    );
    assert_eq!(
        Deadline::date(
            date,
            ConstraintStrength::Soft {
                weight: MAX_DEPENDENCY_WEIGHT + 1,
            },
        )
        .validate(),
        Err(DeadlineError::WeightTooLarge)
    );
}

#[test]
fn dependency_edges_support_all_relations_and_enforce_bounds() {
    let target = id(2);
    for relation in [
        DependencyRelation::FinishToStart,
        DependencyRelation::StartToStart,
        DependencyRelation::FinishToFinish,
        DependencyRelation::StartToFinish,
    ] {
        let dependency = Dependency::try_new(
            target,
            relation,
            Minutes(MAX_DEPENDENCY_LAG_MINUTES),
            ConstraintStrength::Soft {
                weight: MAX_DEPENDENCY_WEIGHT,
            },
        )
        .unwrap();
        assert_eq!(dependency.relation, relation);
    }

    assert_eq!(
        Dependency::try_new(
            target,
            DependencyRelation::StartToFinish,
            Minutes(MAX_DEPENDENCY_LAG_MINUTES + 1),
            ConstraintStrength::Hard,
        ),
        Err(DependencyError::LagTooLarge)
    );
    assert_eq!(
        Dependency::try_new(
            target,
            DependencyRelation::FinishToStart,
            Minutes::ZERO,
            ConstraintStrength::Soft {
                weight: MAX_DEPENDENCY_WEIGHT + 1,
            },
        ),
        Err(DependencyError::WeightTooLarge)
    );
    let self_reference = Dependency::try_new(
        target,
        DependencyRelation::FinishToStart,
        Minutes::ZERO,
        ConstraintStrength::Hard,
    )
    .unwrap();
    assert_eq!(
        self_reference.validate(Some(target)),
        Err(DependencyError::SelfReference)
    );
}

#[test]
fn project_kind_has_a_stable_tagged_wire_shape() {
    assert_eq!(
        serde_json::to_value(ItemKind::Project).unwrap(),
        serde_json::json!({"type": "project"})
    );
    assert_eq!(
        serde_json::from_value::<ItemKind>(serde_json::json!({"type": "project"})).unwrap(),
        ItemKind::Project
    );
}

#[test]
fn only_completed_status_proves_a_prerequisite() {
    assert!(WorkStatus::Completed.satisfies_prerequisite());
    for status in [
        WorkStatus::NotStarted,
        WorkStatus::Scheduled,
        WorkStatus::Active,
        WorkStatus::Paused,
        WorkStatus::Skipped,
        WorkStatus::Canceled,
        WorkStatus::Blocked,
    ] {
        assert!(!status.satisfies_prerequisite());
    }
}

#[test]
fn quantity_targets_use_the_portable_unicode_scalar_contract() {
    let target = |amount, unit: String| QuantityTarget { amount, unit };

    assert!(target(1, "🧵".repeat(200)).is_valid());
    assert!(target(1, " pages ".to_owned()).is_valid());
    assert!(!target(0, "pages".to_owned()).is_valid());
    assert!(!target(1, " ".to_owned()).is_valid());
    assert!(!target(1, "🧵".repeat(201)).is_valid());
    assert!(!target(1, "pages\nweekly".to_owned()).is_valid());
}

#[test]
fn non_leaf_own_effort_is_not_a_separate_executable_identity() {
    for kind in [
        ItemKind::Task,
        ItemKind::Project,
        ItemKind::Goal(GoalSpec {
            measures: Vec::new(),
            weekly_allocation: None,
        }),
        ItemKind::Routine(RoutineSpec {
            ordered: false,
            recurrence: None,
        }),
    ] {
        let semantic_container = !matches!(kind, ItemKind::Task);
        let mut candidate = item(Some(DurationEstimate::exact(30)));
        candidate.kind = kind;
        for has_own_effort in [false, true] {
            candidate.has_own_effort = has_own_effort;
            assert!(!candidate.occupies_time(true));
            assert_eq!(
                candidate.occupies_time(false),
                !semantic_container || has_own_effort
            );
        }
    }
}

#[test]
fn omitted_child_topology_round_trips_without_changing_legacy_leaf_payloads() {
    let mut parent = item(Some(DurationEstimate::exact(30)));
    parent.has_own_effort = true;
    let legacy = serde_json::to_value(&parent).unwrap();
    assert!(legacy.get("has_children_outside_plan").is_none());
    let restored: WorkItem = serde_json::from_value(legacy).unwrap();
    assert!(!restored.has_children_outside_plan);
    assert!(restored.occupies_time(false));

    parent.has_children_outside_plan = true;
    let encoded = serde_json::to_value(&parent).unwrap();
    assert_eq!(encoded["has_children_outside_plan"], true);
    let restored: WorkItem = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored, parent);
    assert!(!restored.occupies_time(false));
    assert_eq!(
        roll_up_expected_durations(&[restored]).unwrap()[&parent.id],
        Minutes::ZERO
    );
}

#[test]
fn rollup_handles_five_thousand_logical_levels_without_recursive_stack_growth() {
    let mut items = Vec::new();
    for index in 1..=5_000_u128 {
        let mut current = item(Some(DurationEstimate::exact(15)));
        current.id = id(index);
        current.parent_id = (index > 1).then(|| id(index - 1));
        // Even explicit own estimates on all 4,999 containers must not be
        // mistaken for separately executable components or counted again.
        current.has_own_effort = true;
        items.push(current);
    }
    items.reverse();

    let totals = roll_up_expected_durations(&items).unwrap();
    assert_eq!(totals.len(), 5_000);
    assert!(totals.values().all(|duration| *duration == Minutes(15)));
}

#[test]
fn rollup_rejects_duplicate_missing_self_and_disconnected_cycle_graphs() {
    let one = item(Some(DurationEstimate::exact(15)));
    assert_eq!(
        roll_up_expected_durations(&[one.clone(), one.clone()]),
        Err(HierarchyError::DuplicateItem(one.id))
    );

    let mut missing = one.clone();
    missing.parent_id = Some(id(999));
    assert_eq!(
        roll_up_expected_durations(&[missing]),
        Err(HierarchyError::MissingParent {
            item: one.id,
            parent: id(999),
        })
    );

    let mut self_parent = one.clone();
    self_parent.parent_id = Some(self_parent.id);
    assert_eq!(
        roll_up_expected_durations(&[self_parent]),
        Err(HierarchyError::Cycle(one.id))
    );

    let mut two = one.clone();
    two.id = id(2);
    two.parent_id = Some(id(3));
    let mut three = one.clone();
    three.id = id(3);
    three.parent_id = Some(two.id);
    for items in [
        vec![one.clone(), two.clone(), three.clone()],
        vec![three, one, two],
    ] {
        assert_eq!(
            roll_up_expected_durations(&items),
            Err(HierarchyError::Cycle(id(2))),
            "an independent valid component cannot hide a cycle; input order is irrelevant"
        );
    }
}

#[test]
fn rollup_counts_each_leaf_once_and_saturates_without_parent_estimates() {
    let mut root = item(Some(DurationEstimate::exact(u32::MAX)));
    root.has_own_effort = true;
    let mut large = item(Some(DurationEstimate::exact(u32::MAX - 5)));
    large.id = id(2);
    large.parent_id = Some(root.id);
    let mut small = item(Some(DurationEstimate::exact(3)));
    small.id = id(3);
    small.parent_id = Some(root.id);

    let ordinary =
        roll_up_expected_durations(&[small.clone(), root.clone(), large.clone()]).unwrap();
    assert_eq!(ordinary[&root.id], Minutes(u32::MAX - 2));
    assert_eq!(ordinary[&small.id], Minutes(3));
    assert_eq!(ordinary[&large.id], Minutes(u32::MAX - 5));

    small.duration = Some(DurationEstimate::exact(10));
    let saturated =
        roll_up_expected_durations(&[large.clone(), small.clone(), root.clone()]).unwrap();
    assert_eq!(saturated[&root.id], Minutes(u32::MAX));
    assert_eq!(saturated[&small.id], Minutes(10));
    assert_eq!(saturated[&large.id], Minutes(u32::MAX - 5));
}
