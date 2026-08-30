use std::collections::{BTreeSet, HashSet};

use dayweave_core::*;
use time::{Duration, OffsetDateTime, macros::datetime};
use uuid::Uuid;

const DAY: OffsetDateTime = datetime!(2026-09-01 0:00 UTC);

fn id(value: u128) -> ItemId {
    ItemId::from_uuid(Uuid::from_u128(value))
}

fn item(value: u128, title: &str, minutes: u32) -> WorkItem {
    WorkItem {
        id: id(value),
        is_sensitive: false,
        revision: 1,
        title: title.to_owned(),
        kind: ItemKind::Task,
        status: WorkStatus::NotStarted,
        parent_id: None,
        sibling_order: None,
        has_own_effort: false,
        goal_ids: BTreeSet::new(),
        priority: Priority {
            importance: 5,
            urgency: 5,
        },
        duration: Some(DurationEstimate::exact(minutes)),
        constraints: SchedulingConstraints::default(),
        split_policy: SplitPolicy::Indivisible,
        energy: None,
        tags: BTreeSet::new(),
        created_at: DAY,
        updated_at: DAY,
    }
}

fn availability(start_hour: i64, end_hour: i64) -> AvailabilityWindow {
    AvailabilityWindow {
        start: DAY + Duration::hours(start_hour),
        end: DAY + Duration::hours(end_hour),
        contexts: BTreeSet::from(["computer".to_owned(), "home".to_owned()]),
        location: Some("home".to_owned()),
        energy: EnergyLevel::Deep,
    }
}

fn request(items: Vec<WorkItem>) -> PlanRequest {
    PlanRequest {
        as_of: DAY + Duration::hours(7),
        horizon_start: DAY,
        horizon_end: DAY + Duration::days(2),
        items,
        availability: vec![availability(8, 18)],
        fixed_blocks: Vec::new(),
        previous_assignments: Vec::new(),
        config: SchedulerConfig::default(),
        recurrence_context: RecurrenceContext::default(),
    }
}

fn planned<'a>(plan: &'a SchedulePlan, item: &WorkItem) -> Vec<&'a ScheduleBlock> {
    plan.blocks_for(item.id)
        .filter(|block| block.kind == ScheduleBlockKind::Planned)
        .collect()
}

fn execution_context(work_units: Vec<ExecutionWorkUnit>) -> ExecutionPlanningContext {
    ExecutionPlanningContext {
        snapshot_revision: 1,
        work_units,
    }
}

fn execution_work(
    item_id: ItemId,
    credited_seconds: u64,
    used_session_indices: Vec<u16>,
) -> ExecutionWorkUnit {
    ExecutionWorkUnit {
        item_id,
        occurrence_id: None,
        progress_epoch: 1,
        credited_seconds,
        disposition: None,
        used_session_indices,
        reservations: Vec::new(),
    }
}

#[test]
fn sensitivity_is_output_metadata_and_never_changes_placement() {
    let ordinary = item(9_001, "SYNTHETIC-SENSITIVE-SCHEDULER-CANARY", 60);
    let mut sensitive = ordinary.clone();
    sensitive.is_sensitive = true;

    let ordinary_plan = Scheduler.plan(&request(vec![ordinary])).unwrap();
    let sensitive_plan = Scheduler.plan(&request(vec![sensitive])).unwrap();
    assert_eq!(ordinary_plan.blocks.len(), 1);
    assert_eq!(sensitive_plan.blocks.len(), 1);
    assert_eq!(
        ordinary_plan.blocks[0].start,
        sensitive_plan.blocks[0].start
    );
    assert_eq!(ordinary_plan.blocks[0].end, sensitive_plan.blocks[0].end);
    assert!(!ordinary_plan.blocks[0].is_sensitive);
    assert!(sensitive_plan.blocks[0].is_sensitive);
}

#[test]
fn hard_deadline_precedes_higher_priority_work_when_capacity_is_tight() {
    let mut important = item(1, "Important but later", 60);
    important.priority = Priority {
        importance: 10,
        urgency: 10,
    };
    let mut deadline = item(2, "Small hard deadline", 60);
    deadline.priority = Priority {
        importance: 1,
        urgency: 1,
    };
    deadline.constraints.latest_finish = Some(Qualified::hard(DAY + Duration::hours(9)));

    let mut input = request(vec![important.clone(), deadline.clone()]);
    input.availability = vec![availability(8, 9)];
    let plan = Scheduler.plan(&input).unwrap();

    assert_eq!(planned(&plan, &deadline).len(), 1);
    assert!(planned(&plan, &important).is_empty());
    assert_eq!(plan.unscheduled[0].item_id, important.id);
    assert!(
        planned(&plan, &deadline)[0]
            .explanations
            .iter()
            .any(|value| value.code == ExplanationCode::HardDeadline)
    );
}

#[test]
fn fixed_meeting_splits_flexible_work_into_valid_sessions() {
    let mut task = item(10, "Write design", 120);
    task.split_policy = SplitPolicy::Splittable {
        minimum_session: Minutes(45),
        maximum_session: Minutes(60),
        maximum_sessions: 3,
        minimum_gap: Minutes(15),
        maximum_days: Some(1),
    };
    let meeting = WorkItem {
        id: id(11),
        is_sensitive: false,
        revision: 1,
        title: "Meeting".to_owned(),
        kind: ItemKind::CalendarEvent(CalendarEventSpec {
            start: DAY + Duration::hours(10),
            end: DAY + Duration::hours(11),
            immutable: true,
            all_day: false,
            source_calendar_id: Some("work".to_owned()),
        }),
        status: WorkStatus::Scheduled,
        parent_id: None,
        sibling_order: None,
        has_own_effort: false,
        goal_ids: BTreeSet::new(),
        priority: Priority::NONE,
        duration: None,
        constraints: SchedulingConstraints::default(),
        split_policy: SplitPolicy::Indivisible,
        energy: None,
        tags: BTreeSet::new(),
        created_at: DAY,
        updated_at: DAY,
    };
    let mut input = request(vec![task.clone(), meeting]);
    input.availability = vec![availability(9, 13)];

    let plan = Scheduler.plan(&input).unwrap();
    let blocks = planned(&plan, &task);
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        (blocks[0].start, blocks[0].end),
        (DAY + Duration::hours(9), DAY + Duration::hours(10),)
    );
    assert_eq!(
        (blocks[1].start, blocks[1].end),
        (DAY + Duration::hours(11), DAY + Duration::hours(12),)
    );
    assert!(plan.unscheduled.is_empty());
    assert!(blocks.iter().all(|block| {
        block
            .explanations
            .iter()
            .any(|value| value.code == ExplanationCode::SplitSession)
    }));
}

#[test]
fn hierarchy_rollup_counts_only_leaf_work_plus_explicit_parent_effort() {
    let mut goal = item(20, "Ship product", 15);
    goal.kind = ItemKind::Goal(GoalSpec {
        measures: Vec::new(),
        weekly_allocation: None,
    });
    goal.has_own_effort = true;

    let mut project = item(21, "Project container", 500);
    project.parent_id = Some(goal.id);

    let mut leaf_a = item(22, "Leaf A", 30);
    leaf_a.parent_id = Some(project.id);
    let mut leaf_b = item(23, "Leaf B", 45);
    leaf_b.parent_id = Some(project.id);

    let items = vec![
        goal.clone(),
        project.clone(),
        leaf_a.clone(),
        leaf_b.clone(),
    ];
    let totals = roll_up_expected_durations(&items).unwrap();
    assert_eq!(totals[&project.id], Minutes(75));
    assert_eq!(totals[&goal.id], Minutes(90));

    let plan = Scheduler.plan(&request(items)).unwrap();
    assert!(planned(&plan, &project).is_empty());
    assert_eq!(planned(&plan, &goal).len(), 1);
    assert_eq!(planned(&plan, &leaf_a).len(), 1);
    assert_eq!(planned(&plan, &leaf_b).len(), 1);
    assert!(plan.decisions.iter().any(|decision| {
        decision.item_id == project.id && decision.kind == DecisionKind::ContainerRolledUp
    }));
}

#[test]
fn empty_goal_is_a_container_until_independent_effort_is_enabled() {
    let mut goal = item(24, "Outcome without actions yet", 120);
    goal.kind = ItemKind::Goal(GoalSpec {
        measures: Vec::new(),
        weekly_allocation: None,
    });

    let plan = Scheduler.plan(&request(vec![goal.clone()])).unwrap();
    assert!(planned(&plan, &goal).is_empty());
    assert!(plan.unscheduled.is_empty());
    assert!(plan.decisions.iter().any(|decision| {
        decision.item_id == goal.id && decision.kind == DecisionKind::ContainerRolledUp
    }));
}

#[test]
fn ordered_routine_children_follow_their_declared_order() {
    let mut routine = item(30, "Morning routine", 1);
    routine.kind = ItemKind::Routine(RoutineSpec {
        ordered: true,
        recurrence: Some(Recurrence::Daily { times_per_day: 1 }),
    });
    routine.duration = None;

    let mut second = item(32, "Second", 20);
    second.parent_id = Some(routine.id);
    second.sibling_order = Some(2);
    second.priority = Priority {
        importance: 10,
        urgency: 10,
    };
    let mut first = item(31, "First", 20);
    first.parent_id = Some(routine.id);
    first.sibling_order = Some(1);
    first.priority = Priority {
        importance: 1,
        urgency: 1,
    };

    let plan = Scheduler
        .plan(&request(vec![routine, second.clone(), first.clone()]))
        .unwrap();
    assert!(planned(&plan, &first)[0].end <= planned(&plan, &second)[0].start);
    assert!(
        planned(&plan, &second)[0]
            .explanations
            .iter()
            .any(|value| value.code == ExplanationCode::Dependency)
    );
}

#[test]
fn hard_dependency_cycle_is_reported_without_guessing() {
    let mut one = item(40, "One", 20);
    let mut two = item(41, "Two", 20);
    one.constraints.dependencies.push(Dependency {
        item_id: two.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });
    two.constraints.dependencies.push(Dependency {
        item_id: one.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });

    let plan = Scheduler.plan(&request(vec![one, two])).unwrap();
    assert!(plan.blocks.is_empty());
    assert_eq!(plan.unscheduled.len(), 2);
    assert!(
        plan.unscheduled
            .iter()
            .all(|work| work.reason == UnscheduledReason::DependencyCycle)
    );
}

#[test]
fn stability_hint_is_preserved_when_still_valid() {
    let task = item(50, "Stable task", 60);
    let old_start = DAY + Duration::hours(14);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: old_start,
            end: old_start + Duration::hours(1),
            session_index: 0,
        }],
        pinned: false,
        manual_placement_id: None,
    }];

    let first = Scheduler.plan(&input).unwrap();
    let second = Scheduler.plan(&input).unwrap();
    assert_eq!(
        first, second,
        "planning must be byte-for-byte deterministic"
    );
    assert_eq!(planned(&first, &task)[0].start, old_start);
    assert_eq!(first.score.moved_minutes, 0);
    assert!(
        planned(&first, &task)[0]
            .explanations
            .iter()
            .any(|value| value.code == ExplanationCode::StableTime)
    );
}

#[test]
fn soft_window_can_be_broken_but_is_never_silent() {
    let mut task = item(60, "Preferred afternoon", 30);
    task.constraints.preferred_daily_windows = vec![Qualified::soft(
        DailyTimeWindow {
            weekdays: BTreeSet::new(),
            start_minute: 14 * 60,
            end_minute: 16 * 60,
        },
        20,
    )];
    let mut input = request(vec![task.clone()]);
    input.availability = vec![availability(9, 10)];

    let plan = Scheduler.plan(&input).unwrap();
    assert_eq!(planned(&plan, &task).len(), 1);
    assert!(plan.score.soft_penalty > 0);
    assert!(plan.violations.iter().any(|violation| {
        violation.item_ids == vec![task.id]
            && violation.severity == ViolationSeverity::Warning
            && violation.message.contains("preferred daily")
    }));
}

#[test]
fn hard_context_and_energy_rules_filter_availability() {
    let mut task = item(70, "Deep computer work", 45);
    task.constraints.required_contexts = vec![Qualified::hard("computer".to_owned())];
    task.constraints.required_location = Some(Qualified::hard("office".to_owned()));
    task.energy = Some(Qualified::hard(EnergyLevel::Deep));
    let mut morning = availability(8, 10);
    morning.location = Some("home".to_owned());
    let mut afternoon = availability(13, 15);
    afternoon.location = Some("office".to_owned());
    let mut input = request(vec![task.clone()]);
    input.availability = vec![morning, afternoon];

    let plan = Scheduler.plan(&input).unwrap();
    assert_eq!(planned(&plan, &task)[0].start, DAY + Duration::hours(13));
}

#[test]
fn habits_and_breaks_are_first_class_schedulable_work() {
    let mut habit = item(75, "Walk", 30);
    habit.kind = ItemKind::Habit(HabitSpec {
        recurrence: Recurrence::Daily { times_per_day: 1 },
        target: Some(QuantityTarget {
            amount: 3_000,
            unit: "steps".to_owned(),
        }),
        preserves_streak_when_paused: true,
    });
    let mut rest = item(76, "Lunch break", 30);
    rest.kind = ItemKind::Break(BreakSpec {
        category: BreakCategory::Meal,
        mandatory: true,
        prompt_to_resume: true,
    });
    rest.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(12)));

    let plan = Scheduler
        .plan(&request(vec![rest.clone(), habit.clone()]))
        .unwrap();
    assert_eq!(planned(&plan, &habit).len(), 1);
    assert_eq!(planned(&plan, &rest).len(), 1);
    assert!(
        planned(&plan, &habit)[0]
            .explanations
            .iter()
            .any(|value| value.code == ExplanationCode::HabitOrRoutine)
    );
    assert!(planned(&plan, &rest)[0].start >= DAY + Duration::hours(12));
}

#[test]
fn pinned_and_fixed_overlap_stays_visible_as_an_error() {
    let task = item(80, "Pinned", 60);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(9),
            end: DAY + Duration::hours(10),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: None,
    }];
    input.fixed_blocks = vec![FixedBlock {
        id: Uuid::from_u128(81),
        is_sensitive: true,
        title: "SYNTHETIC-SENSITIVE-FIXED-BLOCK".to_owned(),
        start: DAY + Duration::hours(9) + Duration::minutes(30),
        end: DAY + Duration::hours(10) + Duration::minutes(30),
        source: FixedBlockSource::GoogleCalendar,
    }];

    let plan = Scheduler.plan(&input).unwrap();
    assert!(plan.violations.iter().any(|violation| {
        violation.kind == ViolationKind::PinnedConflict
            && violation.severity == ViolationSeverity::Error
    }));
    assert!(
        plan.blocks
            .iter()
            .any(|block| block.kind == ScheduleBlockKind::Pinned)
    );
    assert!(
        plan.blocks
            .iter()
            .any(|block| block.kind == ScheduleBlockKind::ExternalFixed)
    );
    assert!(
        plan.blocks
            .iter()
            .find(|block| block.kind == ScheduleBlockKind::ExternalFixed)
            .expect("external fixed block")
            .is_sensitive
    );
}

#[test]
fn immutable_overlap_evidence_is_bounded_before_quadratic_growth() {
    let mut input = request(Vec::new());
    input.fixed_blocks = (0_u128..92)
        .map(|offset| FixedBlock {
            id: Uuid::from_u128(10_000 + offset),
            is_sensitive: false,
            title: format!("Overlapping fixed block {offset}"),
            start: DAY + Duration::hours(9),
            end: DAY + Duration::hours(10),
            source: FixedBlockSource::ProtectedTime,
        })
        .collect();

    assert_eq!(
        Scheduler.plan(&input),
        Err(ScheduleError::ConflictEvidenceLimit)
    );
}

#[test]
fn manual_placement_is_exact_and_reports_digestible_hard_conflicts() {
    let mut task = item(82, "Manual placement", 60);
    task.constraints.latest_finish = Some(Qualified::hard(DAY + Duration::hours(9)));
    let placement_id = Uuid::from_u128(83);
    let fixed_id = Uuid::from_u128(84);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(10),
            end: DAY + Duration::hours(11),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(placement_id),
    }];
    input.fixed_blocks = vec![FixedBlock {
        id: fixed_id,
        is_sensitive: false,
        title: "Immutable context".to_owned(),
        start: DAY + Duration::hours(10) + Duration::minutes(30),
        end: DAY + Duration::hours(11) + Duration::minutes(30),
        source: FixedBlockSource::ProtectedTime,
    }];

    let plan = Scheduler.plan(&input).unwrap();
    let pinned = plan
        .blocks
        .iter()
        .find(|block| block.item_id == Some(task.id))
        .expect("manual pinned block");
    assert_eq!(pinned.kind, ScheduleBlockKind::Pinned);
    assert_eq!(pinned.start, DAY + Duration::hours(10));
    assert_eq!(pinned.end, DAY + Duration::hours(11));

    let [assessment] = plan.manual_placement_assessments.as_slice() else {
        panic!("one manual assessment");
    };
    assert_eq!(assessment.placement_id, placement_id);
    assert!(assessment.violations.iter().any(|violation| {
        violation.code == ManualPlacementViolationCode::LatestFinish
            && violation.boundary_end == Some(DAY + Duration::hours(9))
    }));
    assert!(assessment.violations.iter().any(|violation| {
        violation.code == ManualPlacementViolationCode::ImmutableOverlap
            && violation.conflicting_block_ids == vec![fixed_id]
            && violation.conflicting_blocks.len() == 1
            && violation.conflicting_blocks[0].block_id == fixed_id
            && violation.conflicting_blocks[0].start
                == DAY + Duration::hours(10) + Duration::minutes(30)
    }));
    assert!(plan.violations.iter().any(|violation| {
        violation.kind == ViolationKind::DeadlineRisk
            && violation.severity == ViolationSeverity::Error
            && violation.item_ids == vec![task.id]
    }));
    assert!(plan.violations.iter().any(|violation| {
        violation.kind == ViolationKind::PinnedConflict
            && violation.severity == ViolationSeverity::Error
            && violation.item_ids == vec![task.id]
    }));

    let original_environment = assessment.environment_digest;
    input.items.push(item(840, "Dependency environment", 30));
    let changed = Scheduler.plan(&input).unwrap();
    assert_ne!(
        changed.manual_placement_assessments[0].environment_digest,
        original_environment
    );
}

#[test]
fn malformed_manual_placement_cannot_be_treated_as_a_stability_hint() {
    let task = item(85, "Manual placement", 60);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(10),
            end: DAY + Duration::hours(11),
            session_index: 0,
        }],
        pinned: false,
        manual_placement_id: Some(Uuid::from_u128(86)),
    }];

    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidItem { item_id, .. }) if item_id == task.id
    ));
}

#[test]
fn manual_placement_requires_exact_minute_grid_without_subseconds() {
    let task = item(861, "Manual placement", 60);
    let mut input = request(vec![task.clone()]);
    let fractional_offset = Duration::microseconds(123_456);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(10) + fractional_offset,
            end: DAY + Duration::hours(11) + fractional_offset,
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(Uuid::from_u128(862)),
    }];

    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidItem { item_id, message })
            if item_id == task.id && message.contains("whole-minute scheduler slots")
    ));

    input.previous_assignments[0].blocks[0].start = DAY + Duration::hours(10);
    input.previous_assignments[0].blocks[0].end = DAY + Duration::hours(11);
    Scheduler
        .plan(&input)
        .expect("whole-second endpoints on the scheduler minute grid are valid");
}

#[test]
fn manual_placement_cannot_change_duration_or_indivisible_shape() {
    let task = item(87, "Manual placement", 60);
    let placement_id = Uuid::from_u128(88);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(10),
            end: DAY + Duration::hours(12),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(placement_id),
    }];
    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidItem { item_id, message })
            if item_id == task.id && message.contains("exact remaining work duration")
    ));

    input.previous_assignments[0].blocks = vec![
        PreviousBlock {
            start: DAY + Duration::hours(10),
            end: DAY + Duration::hours(10) + Duration::minutes(30),
            session_index: 0,
        },
        PreviousBlock {
            start: DAY + Duration::hours(11),
            end: DAY + Duration::hours(11) + Duration::minutes(30),
            session_index: 1,
        },
    ];
    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidItem { item_id, message })
            if item_id == task.id && message.contains("indivisible")
    ));
}

#[test]
fn manual_placement_requires_one_availability_window_with_all_capabilities() {
    let mut task = item(89, "Manual placement", 60);
    task.constraints.required_contexts = vec![Qualified::hard("computer".to_owned())];
    task.constraints.required_location = Some(Qualified::hard("home".to_owned()));
    task.energy = Some(Qualified::hard(EnergyLevel::Deep));
    let mut input = request(vec![task.clone()]);
    input.availability = vec![
        AvailabilityWindow {
            start: DAY + Duration::hours(8),
            end: DAY + Duration::hours(18),
            contexts: BTreeSet::from(["computer".to_owned()]),
            location: Some("office".to_owned()),
            energy: EnergyLevel::Deep,
        },
        AvailabilityWindow {
            start: DAY + Duration::hours(8),
            end: DAY + Duration::hours(18),
            contexts: BTreeSet::new(),
            location: Some("home".to_owned()),
            energy: EnergyLevel::Deep,
        },
    ];
    input.previous_assignments = vec![PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(10),
            end: DAY + Duration::hours(11),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(Uuid::from_u128(90)),
    }];

    let plan = Scheduler.plan(&input).unwrap();
    assert!(
        plan.manual_placement_assessments[0]
            .violations
            .iter()
            .any(|violation| {
                violation.code == ManualPlacementViolationCode::RequiredCapabilities
            })
    );
}

#[test]
fn manual_successor_reports_dependency_when_predecessor_is_only_partly_scheduled() {
    let mut predecessor = item(91, "Partly scheduled predecessor", 120);
    predecessor.split_policy = SplitPolicy::Splittable {
        minimum_session: Minutes(30),
        maximum_session: Minutes(60),
        maximum_sessions: 2,
        minimum_gap: Minutes::ZERO,
        maximum_days: Some(1),
    };
    let mut successor = item(92, "Manually placed successor", 30);
    successor.constraints.dependencies = vec![Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    }];
    let placement_id = Uuid::from_u128(93);
    let mut input = request(vec![predecessor.clone(), successor.clone()]);
    input.availability = vec![availability(8, 9)];
    input.previous_assignments = vec![PreviousAssignment {
        item_id: successor.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(9),
            end: DAY + Duration::hours(9) + Duration::minutes(30),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(placement_id),
    }];

    let plan = Scheduler.plan(&input).unwrap();
    assert!(
        plan.unscheduled
            .iter()
            .any(|work| { work.item_id == predecessor.id && work.remaining == Minutes(60) })
    );
    let assessment = plan
        .manual_placement_assessments
        .iter()
        .find(|assessment| assessment.placement_id == placement_id)
        .expect("manual placement assessment");
    assert!(assessment.violations.iter().any(|violation| {
        violation.code == ManualPlacementViolationCode::Dependency
            && !violation.conflicting_blocks.is_empty()
    }));
}

#[test]
fn manual_assessment_rejects_conflict_fact_amplification_before_cloning() {
    const PREDECESSOR_BLOCKS: u16 = 4_097;
    let predecessor_minutes = u32::from(PREDECESSOR_BLOCKS);
    let predecessor = item(94, "Many source sessions", predecessor_minutes);
    let mut successor = item(95, "Bounded manual successor", 30);
    successor.constraints.dependencies = vec![Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    }];
    let mut input = request(vec![predecessor.clone(), successor.clone()]);
    input.horizon_end = DAY + Duration::days(4);
    let source_start = DAY + Duration::hours(8);
    input.previous_assignments = vec![
        PreviousAssignment {
            item_id: predecessor.id,
            occurrence_id: None,
            blocks: (0..PREDECESSOR_BLOCKS)
                .map(|session_index| PreviousBlock {
                    start: source_start + Duration::minutes(i64::from(session_index)),
                    end: source_start + Duration::minutes(i64::from(session_index) + 1),
                    session_index,
                })
                .collect(),
            pinned: true,
            manual_placement_id: None,
        },
        PreviousAssignment {
            item_id: successor.id,
            occurrence_id: None,
            blocks: vec![PreviousBlock {
                start: source_start,
                end: source_start + Duration::minutes(30),
                session_index: 0,
            }],
            pinned: true,
            manual_placement_id: Some(Uuid::from_u128(96)),
        },
    ];

    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidItem { item_id, message })
            if item_id == successor.id && message.contains("supported evidence limit")
    ));
}

#[test]
fn plan_and_request_are_lossless_json_contracts() {
    let mut task = item(90, "Serializable", 25);
    task.constraints.allowed_weekdays = Some(Qualified::hard(BTreeSet::from([
        DayOfWeek::Monday,
        DayOfWeek::Tuesday,
    ])));
    let input = request(vec![task]);
    let encoded = serde_json::to_string(&input).unwrap();
    let decoded: PlanRequest = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, input);

    let plan = Scheduler.plan(&input).unwrap();
    let encoded = serde_json::to_string(&plan).unwrap();
    let decoded: SchedulePlan = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, plan);
}

#[test]
fn property_style_permutations_keep_invariants_and_same_plan() {
    let mut items: Vec<_> = (0_u128..6)
        .map(|index| {
            let offset = u32::try_from(index).unwrap();
            let mut value = item(100 + index, &format!("Task {index}"), 20 + offset * 5);
            value.priority = Priority {
                importance: u8::try_from(index + 1).unwrap(),
                urgency: u8::try_from(6 - index).unwrap(),
            };
            value
        })
        .collect();
    let baseline = Scheduler.plan(&request(items.clone())).unwrap();

    for rotation in 0..items.len() {
        items.rotate_left(rotation);
        let plan = Scheduler.plan(&request(items.clone())).unwrap();
        assert_eq!(plan, baseline, "input order must not affect the plan");

        let planned_blocks: Vec<_> = plan
            .blocks
            .iter()
            .filter(|block| block.kind == ScheduleBlockKind::Planned)
            .collect();
        let ids: HashSet<_> = planned_blocks.iter().map(|block| block.id).collect();
        assert_eq!(ids.len(), planned_blocks.len(), "block ids must be unique");
        for (index, left) in planned_blocks.iter().enumerate() {
            assert!(left.start >= DAY + Duration::hours(8));
            assert!(left.end <= DAY + Duration::hours(18));
            assert!(left.start < left.end);
            for right in &planned_blocks[(index + 1)..] {
                assert!(left.end <= right.start || right.end <= left.start);
            }
        }
    }
}

#[test]
fn execution_credit_reduces_remaining_once_and_uses_a_fresh_index() {
    let task = item(200, "Partly complete", 60);
    let input = request(vec![task.clone()]);
    let execution = execution_context(vec![execution_work(task.id, 20 * 60 + 1, vec![0])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    let blocks = planned(&plan, &task);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].session_index, 1);
    assert_eq!((blocks[0].end - blocks[0].start).whole_minutes(), 39);
    assert!(plan.unscheduled.is_empty());
}

#[test]
fn zero_execution_credit_preserves_demand_but_advances_the_index() {
    let task = item(201, "Zero-credit completion", 60);
    let input = request(vec![task.clone()]);
    let execution = execution_context(vec![execution_work(task.id, 0, vec![0])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    let blocks = planned(&plan, &task);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].session_index, 1);
    assert_eq!((blocks[0].end - blocks[0].start).whole_minutes(), 60);
}

#[test]
fn skipped_execution_unit_does_not_suppress_other_work() {
    let skipped = item(202, "Skip only this", 60);
    let retained = item(203, "Still required", 60);
    let mut input = request(vec![skipped.clone(), retained.clone()]);
    input.previous_assignments.push(PreviousAssignment {
        item_id: skipped.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(12),
            end: DAY + Duration::hours(13),
            session_index: 5,
        }],
        pinned: true,
        manual_placement_id: None,
    });
    let mut skipped_work = execution_work(skipped.id, 0, vec![0]);
    skipped_work.disposition = Some(ExecutionDisposition::Skipped);

    let plan = Scheduler
        .plan_with_execution(&input, &execution_context(vec![skipped_work]))
        .unwrap();
    assert!(plan.blocks_for(skipped.id).next().is_none());
    assert_eq!(planned(&plan, &retained).len(), 1);
    assert!(plan.decisions.iter().any(|decision| {
        decision.item_id == skipped.id && decision.kind == DecisionKind::TerminalItemIgnored
    }));
}

#[test]
fn deferred_replacement_is_exact_and_new_work_starts_after_it() {
    let task = item(204, "Deferred split work", 90);
    let input = request(vec![task.clone()]);
    let mut work = execution_work(task.id, 30 * 60, vec![0]);
    work.reservations.push(ExecutionReservation {
        session_index: 1,
        start: DAY + Duration::hours(10),
        end: DAY + Duration::hours(10) + Duration::minutes(30),
        kind: ExecutionReservationKind::DeferredReplacement {
            source_session_index: 0,
        },
    });

    let plan = Scheduler
        .plan_with_execution(&input, &execution_context(vec![work]))
        .unwrap();
    let pinned = plan
        .blocks_for(task.id)
        .find(|block| block.kind == ScheduleBlockKind::Pinned)
        .unwrap();
    assert_eq!(pinned.session_index, 1);
    assert_eq!(pinned.start, DAY + Duration::hours(10));
    assert_eq!(
        pinned.end,
        DAY + Duration::hours(10) + Duration::minutes(30)
    );
    let planned = planned(&plan, &task);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].session_index, 2);
    assert_eq!((planned[0].end - planned[0].start).whole_minutes(), 30);
}

#[test]
fn disjoint_execution_reservation_reduces_demand_without_emitting_a_block() {
    let task = item(205, "Reserved outside this horizon", 60);
    let input = request(vec![task.clone()]);
    let mut work = execution_work(task.id, 0, vec![0]);
    work.reservations.push(ExecutionReservation {
        session_index: 1,
        start: DAY + Duration::days(3),
        end: DAY + Duration::days(3) + Duration::minutes(30),
        kind: ExecutionReservationKind::DeferredReplacement {
            source_session_index: 0,
        },
    });

    let plan = Scheduler
        .plan_with_execution(&input, &execution_context(vec![work]))
        .unwrap();
    assert!(
        plan.blocks_for(task.id)
            .all(|block| block.kind != ScheduleBlockKind::Pinned)
    );
    let planned = planned(&plan, &task);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].session_index, 2);
    assert_eq!((planned[0].end - planned[0].start).whole_minutes(), 30);
}

#[test]
fn partially_covered_execution_reservation_is_rejected() {
    let task = item(206, "Partly outside", 60);
    let input = request(vec![task.clone()]);
    let mut work = execution_work(task.id, 0, vec![0]);
    work.reservations.push(ExecutionReservation {
        session_index: 1,
        start: DAY - Duration::minutes(30),
        end: DAY + Duration::minutes(30),
        kind: ExecutionReservationKind::DeferredReplacement {
            source_session_index: 0,
        },
    });

    assert!(matches!(
        Scheduler.plan_with_execution(&input, &execution_context(vec![work])),
        Err(ScheduleError::InvalidItem { item_id, message })
            if item_id == task.id && message.contains("only partly covered")
    ));
}

#[test]
fn caller_blocks_at_or_below_execution_high_water_are_removed() {
    let task = item(207, "Do not resurrect old blocks", 60);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments.push(PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(9),
            end: DAY + Duration::hours(9) + Duration::minutes(30),
            session_index: 1,
        }],
        pinned: true,
        manual_placement_id: None,
    });
    let execution = execution_context(vec![execution_work(task.id, 0, vec![0, 2])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    assert!(
        plan.blocks_for(task.id)
            .all(|block| block.kind != ScheduleBlockKind::Pinned)
    );
    let planned = planned(&plan, &task);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].session_index, 3);
    assert_eq!((planned[0].end - planned[0].start).whole_minutes(), 60);
}

#[test]
fn terminal_history_does_not_consume_the_live_maximum_session_count() {
    let mut task = item(208, "History is not live capacity", 60);
    task.split_policy = SplitPolicy::Splittable {
        minimum_session: Minutes(30),
        maximum_session: Minutes(30),
        maximum_sessions: 2,
        minimum_gap: Minutes::ZERO,
        maximum_days: None,
    };
    let input = request(vec![task.clone()]);
    let execution = execution_context(vec![execution_work(task.id, 0, vec![0, 1, 2, 3])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    let blocks = planned(&plan, &task);
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.session_index)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert!(plan.unscheduled.is_empty());
}

#[test]
fn exhausted_u16_session_space_never_reuses_the_last_index() {
    let task = item(209, "No index reuse", 60);
    let input = request(vec![task.clone()]);
    let execution = execution_context(vec![execution_work(task.id, 0, vec![u16::MAX])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    assert!(plan.blocks_for(task.id).next().is_none());
    assert_eq!(plan.unscheduled.len(), 1);
    assert_eq!(plan.unscheduled[0].reason, UnscheduledReason::SessionLimit);
    assert_eq!(plan.unscheduled[0].remaining, Minutes(60));
}
