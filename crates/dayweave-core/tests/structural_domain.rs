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
