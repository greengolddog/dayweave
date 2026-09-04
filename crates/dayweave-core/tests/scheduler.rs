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

fn calendar_event(
    value: u128,
    title: &str,
    start: OffsetDateTime,
    end: OffsetDateTime,
) -> WorkItem {
    WorkItem {
        id: id(value),
        is_sensitive: false,
        revision: 1,
        title: title.to_owned(),
        kind: ItemKind::CalendarEvent(CalendarEventSpec {
            start,
            end,
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

fn in_flight_defer_work(item_id: ItemId, source_minutes: i64) -> ExecutionWorkUnit {
    let mut work = execution_work(item_id, 0, vec![0]);
    work.reservations.push(ExecutionReservation {
        session_index: 0,
        start: DAY + Duration::hours(8),
        end: DAY + Duration::hours(8) + Duration::minutes(source_minutes),
        kind: ExecutionReservationKind::InFlight,
    });
    work
}

fn defer_candidate(item_id: ItemId) -> DeferCandidateAssessmentInput {
    DeferCandidateAssessmentInput {
        placement_id: Uuid::from_u128(30_002),
        item_id,
        occurrence_id: None,
        source_session_index: 0,
        replacement_session_index: 1,
        credited_seconds_after_source: 601,
        move_start: DAY + Duration::hours(10),
        move_end: DAY + Duration::hours(10) + Duration::minutes(49),
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
fn extreme_timestamp_offsets_fail_without_panicking() {
    let horizon_end = datetime!(9999-12-31 23:59:59 UTC);
    let horizon_start = horizon_end
        .checked_sub(Duration::hours(2))
        .expect("bounded extreme horizon");
    let mut task = item(9_002, "Extreme timestamp", 30);
    task.created_at = horizon_start;
    task.updated_at = horizon_start;
    task.constraints.minimum_notice = Some(Qualified::hard(Minutes(u32::MAX)));
    task.constraints.buffers = BufferPolicy {
        before: Minutes(u32::MAX),
        after: Minutes(u32::MAX),
        strength: Some(ConstraintStrength::Hard),
    };
    let input = PlanRequest {
        as_of: horizon_start,
        horizon_start,
        horizon_end,
        items: vec![task],
        availability: vec![AvailabilityWindow {
            start: horizon_start,
            end: horizon_end,
            contexts: BTreeSet::new(),
            location: None,
            energy: EnergyLevel::Deep,
        }],
        fixed_blocks: Vec::new(),
        previous_assignments: Vec::new(),
        config: SchedulerConfig::default(),
        recurrence_context: RecurrenceContext::default(),
    };

    let outcome = std::panic::catch_unwind(|| Scheduler.plan(&input));
    assert!(outcome.is_ok(), "extreme timestamp input must not unwind");
    assert!(outcome.expect("catch result").is_err());
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
fn project_is_a_container_until_independent_effort_is_enabled() {
    let mut project = item(25, "Project", 60);
    project.kind = ItemKind::Project;

    let container_plan = Scheduler.plan(&request(vec![project.clone()])).unwrap();
    assert!(planned(&container_plan, &project).is_empty());
    assert_eq!(
        roll_up_expected_durations(&[project.clone()]).unwrap()[&project.id],
        Minutes::ZERO
    );

    project.has_own_effort = true;
    let own_effort_plan = Scheduler.plan(&request(vec![project.clone()])).unwrap();
    assert_eq!(planned(&own_effort_plan, &project).len(), 1);
    assert_eq!(
        roll_up_expected_durations(&[project.clone()]).unwrap()[&project.id],
        Minutes(60)
    );

    let mut child = item(26, "Project action", 30);
    child.parent_id = Some(project.id);
    let items = vec![project.clone(), child.clone()];
    assert_eq!(
        roll_up_expected_durations(&items).unwrap()[&project.id],
        Minutes(90)
    );
    let combined_plan = Scheduler.plan(&request(items)).unwrap();
    assert_eq!(planned(&combined_plan, &project).len(), 1);
    assert_eq!(planned(&combined_plan, &child).len(), 1);
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
fn only_completed_terminal_work_satisfies_a_hard_finish_to_start_prerequisite() {
    for (status, satisfies) in [
        (WorkStatus::Completed, true),
        (WorkStatus::Skipped, false),
        (WorkStatus::Canceled, false),
    ] {
        let mut predecessor = item(42, "Prerequisite", 20);
        predecessor.status = status;
        let mut successor = item(43, "Dependent work", 20);
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation: DependencyRelation::FinishToStart,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        });

        let plan = Scheduler
            .plan(&request(vec![predecessor, successor.clone()]))
            .unwrap();
        assert_eq!(!planned(&plan, &successor).is_empty(), satisfies);
        assert_eq!(
            plan.unscheduled.iter().any(|work| {
                work.item_id == successor.id
                    && work.reason == UnscheduledReason::DependencyUnavailable
            }),
            !satisfies
        );
    }
}

#[test]
fn blocked_predecessor_makes_hard_dependency_unavailable() {
    let mut predecessor = item(48, "Blocked prerequisite", 20);
    predecessor.status = WorkStatus::Blocked;
    let mut successor = item(49, "Dependent work", 20);
    successor.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });

    let plan = Scheduler
        .plan(&request(vec![predecessor.clone(), successor.clone()]))
        .unwrap();

    assert!(plan.blocks_for(predecessor.id).next().is_none());
    assert!(plan.blocks_for(successor.id).next().is_none());
    assert!(plan.unscheduled.iter().any(|work| {
        work.item_id == predecessor.id && work.reason == UnscheduledReason::Blocked
    }));
    assert!(plan.unscheduled.iter().any(|work| {
        work.item_id == successor.id && work.reason == UnscheduledReason::DependencyUnavailable
    }));
}

#[test]
fn partial_predecessor_readiness_uses_the_relation_predecessor_boundary() {
    for (relation, successor_can_be_scheduled) in [
        (DependencyRelation::FinishToStart, false),
        (DependencyRelation::StartToStart, true),
        (DependencyRelation::FinishToFinish, false),
        (DependencyRelation::StartToFinish, true),
    ] {
        let mut predecessor = item(49_001, "Partly scheduled predecessor", 120);
        predecessor.split_policy = SplitPolicy::Splittable {
            minimum_session: Minutes(60),
            maximum_session: Minutes(60),
            maximum_sessions: 2,
            minimum_gap: Minutes::ZERO,
            maximum_days: Some(1),
        };
        let mut successor = item(49_002, "Dependent work", 30);
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: Minutes(120),
            strength: ConstraintStrength::Hard,
        });
        let successor_window = AvailabilityWindow {
            end: DAY + Duration::hours(10) + Duration::minutes(30),
            ..availability(10, 11)
        };
        let mut input = request(vec![predecessor.clone(), successor.clone()]);
        input.availability = vec![availability(8, 9), successor_window];

        let plan = Scheduler.plan(&input).unwrap();

        assert_eq!(planned(&plan, &predecessor).len(), 1, "{relation:?}");
        assert!(
            plan.unscheduled
                .iter()
                .any(|work| { work.item_id == predecessor.id && work.remaining == Minutes(60) })
        );
        assert_eq!(
            !planned(&plan, &successor).is_empty(),
            successor_can_be_scheduled,
            "{relation:?}"
        );
        assert_eq!(
            plan.unscheduled.iter().any(|work| {
                work.item_id == successor.id
                    && work.reason == UnscheduledReason::DependencyUnavailable
            }),
            !successor_can_be_scheduled,
            "{relation:?}"
        );
    }
}

#[test]
fn blocked_semantic_container_without_own_effort_has_no_phantom_demand() {
    let mut project = item(48_001, "Blocked project container", 180);
    project.kind = ItemKind::Project;
    project.status = WorkStatus::Blocked;

    let plan = Scheduler.plan(&request(vec![project.clone()])).unwrap();

    assert!(plan.blocks_for(project.id).next().is_none());
    assert!(
        plan.unscheduled
            .iter()
            .all(|work| work.item_id != project.id)
    );
    assert_eq!(plan.score.unscheduled_minutes, 0);
    assert!(plan.decisions.iter().any(|decision| {
        decision.item_id == project.id && decision.kind == DecisionKind::ContainerRolledUp
    }));

    project.has_own_effort = true;
    let own_effort = Scheduler.plan(&request(vec![project.clone()])).unwrap();
    assert!(own_effort.unscheduled.iter().any(|work| {
        work.item_id == project.id
            && work.reason == UnscheduledReason::Blocked
            && work.remaining == Minutes(180)
    }));
}

#[test]
fn inactive_calendar_events_do_not_reserve_time_and_dependency_assessments_agree() {
    for (status, predecessor_is_satisfied) in [
        (WorkStatus::Completed, true),
        (WorkStatus::Skipped, false),
        (WorkStatus::Canceled, false),
        (WorkStatus::Blocked, false),
    ] {
        let event = WorkItem {
            id: id(48_010),
            is_sensitive: false,
            revision: 1,
            title: "Inactive fixed predecessor".to_owned(),
            kind: ItemKind::CalendarEvent(CalendarEventSpec {
                start: DAY + Duration::hours(9),
                end: DAY + Duration::hours(10),
                immutable: true,
                all_day: false,
                source_calendar_id: Some("work".to_owned()),
            }),
            status,
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

        let free_work = item(48_011, "Uses released event time", 60);
        let mut occupancy_request = request(vec![event.clone(), free_work.clone()]);
        occupancy_request.availability = vec![availability(9, 10)];
        let occupancy_plan = Scheduler.plan(&occupancy_request).unwrap();
        assert_eq!(planned(&occupancy_plan, &free_work).len(), 1);
        assert!(occupancy_plan.blocks_for(event.id).next().is_none());

        let mut successor = item(48_012, "Depends on inactive event", 30);
        successor.constraints.dependencies.push(Dependency {
            item_id: event.id,
            relation: DependencyRelation::FinishToStart,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        });
        let mut ordinary_request = request(vec![event.clone(), successor.clone()]);
        ordinary_request.availability = vec![availability(10, 11)];
        let ordinary = Scheduler.plan(&ordinary_request).unwrap();
        assert_eq!(
            !planned(&ordinary, &successor).is_empty(),
            predecessor_is_satisfied
        );

        let placement_id = Uuid::from_u128(48_013);
        let mut manual_request = request(vec![event, successor.clone()]);
        manual_request.availability = vec![availability(10, 11)];
        manual_request.previous_assignments = vec![PreviousAssignment {
            item_id: successor.id,
            occurrence_id: None,
            blocks: vec![PreviousBlock {
                start: DAY + Duration::hours(10),
                end: DAY + Duration::hours(10) + Duration::minutes(30),
                session_index: 0,
            }],
            pinned: true,
            manual_placement_id: Some(placement_id),
        }];
        let manual = Scheduler.plan(&manual_request).unwrap();
        let dependency_violation = manual.manual_placement_assessments[0]
            .violations
            .iter()
            .any(|violation| violation.code == ManualPlacementViolationCode::Dependency);
        assert_eq!(dependency_violation, !predecessor_is_satisfied);
    }
}

#[test]
fn fixed_calendar_event_remains_a_valid_hard_dependency_predecessor() {
    let meeting = WorkItem {
        id: id(50),
        is_sensitive: false,
        revision: 1,
        title: "Fixed predecessor".to_owned(),
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
    let mut successor = item(51, "After meeting", 30);
    successor.constraints.dependencies.push(Dependency {
        item_id: meeting.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });

    let plan = Scheduler
        .plan(&request(vec![meeting, successor.clone()]))
        .unwrap();

    let successor_blocks = planned(&plan, &successor);
    let [block] = successor_blocks.as_slice() else {
        panic!("the dependent work must be scheduled once");
    };
    assert!(block.start >= DAY + Duration::hours(11));
}

#[test]
fn finish_relations_constrain_only_the_aggregate_finish_of_a_split_successor() {
    for (relation, lag) in [
        (DependencyRelation::FinishToFinish, Minutes::ZERO),
        (DependencyRelation::StartToFinish, Minutes(60)),
    ] {
        let predecessor = calendar_event(
            51_001,
            "Fixed predecessor",
            DAY + Duration::hours(10),
            DAY + Duration::hours(11),
        );
        let mut successor = item(51_002, "Split dependent work", 120);
        successor.split_policy = SplitPolicy::Splittable {
            minimum_session: Minutes(60),
            maximum_session: Minutes(60),
            maximum_sessions: 2,
            minimum_gap: Minutes::ZERO,
            maximum_days: Some(1),
        };
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: lag,
            strength: ConstraintStrength::Hard,
        });
        let mut input = request(vec![predecessor, successor.clone()]);
        input.availability = vec![availability(8, 12)];

        let plan = Scheduler.plan(&input).unwrap();
        let blocks = planned(&plan, &successor);

        assert_eq!(blocks.len(), 2, "{relation:?}");
        assert_eq!(blocks[0].start, DAY + Duration::hours(8), "{relation:?}");
        assert_eq!(blocks[0].end, DAY + Duration::hours(9), "{relation:?}");
        assert_eq!(blocks[1].start, DAY + Duration::hours(11), "{relation:?}");
        assert_eq!(blocks[1].end, DAY + Duration::hours(12), "{relation:?}");
        assert!(
            plan.unscheduled
                .iter()
                .all(|work| work.item_id != successor.id),
            "{relation:?}"
        );
    }
}

#[test]
fn soft_finish_dependency_penalty_is_assessed_only_on_the_aggregate_finish() {
    let predecessor = calendar_event(
        51_003,
        "Fixed predecessor",
        DAY + Duration::hours(10),
        DAY + Duration::hours(11),
    );
    let mut successor = item(51_004, "Split dependent work", 120);
    successor.split_policy = SplitPolicy::Splittable {
        minimum_session: Minutes(60),
        maximum_session: Minutes(60),
        maximum_sessions: 2,
        minimum_gap: Minutes::ZERO,
        maximum_days: Some(1),
    };
    successor.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToFinish,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Soft { weight: 7 },
    });
    let mut input = request(vec![predecessor, successor.clone()]);
    input.availability = vec![availability(8, 12)];

    let plan = Scheduler.plan(&input).unwrap();
    let blocks = planned(&plan, &successor);

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].end, DAY + Duration::hours(9));
    assert_eq!(blocks[1].end, DAY + Duration::hours(12));
    assert_eq!(plan.score.soft_penalty, 0);
    assert!(
        plan.violations
            .iter()
            .all(|violation| violation.kind != ViolationKind::Dependency)
    );
}

#[test]
fn higher_priority_successor_does_not_retain_a_provisional_soft_dependency_penalty() {
    let mut predecessor = item(51_005, "Lower-priority predecessor", 60);
    predecessor.priority = Priority {
        importance: 1,
        urgency: 1,
    };
    let mut successor = item(51_006, "Higher-priority successor", 60);
    successor.priority = Priority {
        importance: 10,
        urgency: 10,
    };
    successor.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(9)));
    successor.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Soft { weight: 7 },
    });
    let mut input = request(vec![successor.clone(), predecessor.clone()]);
    input.availability = vec![availability(8, 10)];

    let plan = Scheduler.plan(&input).unwrap();

    assert_eq!(
        planned(&plan, &predecessor)[0].start,
        DAY + Duration::hours(8)
    );
    assert_eq!(
        planned(&plan, &predecessor)[0].end,
        DAY + Duration::hours(9)
    );
    assert_eq!(
        planned(&plan, &successor)[0].start,
        DAY + Duration::hours(9)
    );
    assert_eq!(plan.score.soft_penalty, 0);
    assert!(
        plan.violations
            .iter()
            .all(|violation| violation.kind != ViolationKind::Dependency)
    );
    assert!(
        planned(&plan, &successor)[0]
            .explanations
            .iter()
            .any(|value| value.code == ExplanationCode::Dependency)
    );
}

#[test]
fn soft_dependency_cycles_remain_schedulable_and_use_final_split_boundaries() {
    for (relation, expected_penalty) in [
        (DependencyRelation::FinishToStart, 540),
        (DependencyRelation::StartToStart, 360),
        (DependencyRelation::FinishToFinish, 180),
        (DependencyRelation::StartToFinish, 0),
    ] {
        let mut split_successor = item(51_007, "First soft-cycle member", 120);
        split_successor.split_policy = SplitPolicy::Splittable {
            minimum_session: Minutes(60),
            maximum_session: Minutes(60),
            maximum_sessions: 2,
            minimum_gap: Minutes::ZERO,
            maximum_days: Some(1),
        };
        let mut other = item(51_008, "Second soft-cycle member", 60);
        split_successor.constraints.dependencies.push(Dependency {
            item_id: other.id,
            relation,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Soft { weight: 3 },
        });
        other.constraints.dependencies.push(Dependency {
            item_id: split_successor.id,
            relation,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Soft { weight: 3 },
        });
        let mut input = request(vec![split_successor.clone(), other.clone()]);
        input.availability = vec![availability(8, 11)];

        let plan = Scheduler.plan(&input).unwrap();

        assert!(plan.unscheduled.is_empty(), "{relation:?}");
        let split_blocks = planned(&plan, &split_successor);
        assert_eq!(split_blocks.len(), 2, "{relation:?}");
        assert_eq!(
            split_blocks[0].start,
            DAY + Duration::hours(8),
            "{relation:?}"
        );
        assert_eq!(
            split_blocks[1].end,
            DAY + Duration::hours(10),
            "{relation:?}"
        );
        assert_eq!(
            planned(&plan, &other)[0].start,
            DAY + Duration::hours(10),
            "{relation:?}"
        );
        assert_eq!(plan.score.soft_penalty, expected_penalty, "{relation:?}");
        let dependency_violations = plan
            .violations
            .iter()
            .filter(|violation| violation.kind == ViolationKind::Dependency)
            .collect::<Vec<_>>();
        assert_eq!(
            dependency_violations.len(),
            usize::from(expected_penalty > 0),
            "{relation:?}"
        );
        if let Some(violation) = dependency_violations.first() {
            assert_eq!(violation.item_ids, vec![split_successor.id], "{relation:?}");
            assert_eq!(violation.penalty, expected_penalty, "{relation:?}");
        }
    }
}

#[test]
fn retained_split_pins_surface_hard_dependency_conflicts_for_every_relation() {
    for relation in [
        DependencyRelation::FinishToStart,
        DependencyRelation::StartToStart,
        DependencyRelation::FinishToFinish,
        DependencyRelation::StartToFinish,
    ] {
        let mut predecessor = item(51_009, "Predecessor after retained work", 60);
        predecessor.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(10)));
        let mut successor = item(51_010, "Retained split successor", 120);
        successor.split_policy = SplitPolicy::Splittable {
            minimum_session: Minutes(60),
            maximum_session: Minutes(60),
            maximum_sessions: 2,
            minimum_gap: Minutes::ZERO,
            maximum_days: Some(1),
        };
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: Minutes(15),
            strength: ConstraintStrength::Hard,
        });
        let mut input = request(vec![successor.clone(), predecessor.clone()]);
        input.availability = vec![availability(8, 11)];
        input.previous_assignments = vec![PreviousAssignment {
            item_id: successor.id,
            occurrence_id: None,
            blocks: vec![
                PreviousBlock {
                    start: DAY + Duration::hours(8),
                    end: DAY + Duration::hours(9),
                    session_index: 0,
                },
                PreviousBlock {
                    start: DAY + Duration::hours(9),
                    end: DAY + Duration::hours(10),
                    session_index: 1,
                },
            ],
            pinned: true,
            manual_placement_id: None,
        }];

        let plan = Scheduler.plan(&input).unwrap();

        assert!(plan.unscheduled.is_empty(), "{relation:?}");
        assert_eq!(
            plan.blocks_for(successor.id)
                .filter(|block| block.kind == ScheduleBlockKind::Pinned)
                .count(),
            2,
            "{relation:?}"
        );
        let violations = plan
            .violations
            .iter()
            .filter(|violation| violation.kind == ViolationKind::Dependency)
            .collect::<Vec<_>>();
        assert_eq!(violations.len(), 1, "{relation:?}");
        assert_eq!(
            violations[0].severity,
            ViolationSeverity::Error,
            "{relation:?}"
        );
        assert_eq!(violations[0].penalty, 0, "{relation:?}");
        assert!(
            violations[0].item_ids.contains(&predecessor.id),
            "{relation:?}"
        );
        assert!(
            violations[0].item_ids.contains(&successor.id),
            "{relation:?}"
        );
    }
}

#[test]
fn authoritative_execution_reservation_surfaces_a_hard_start_dependency_conflict() {
    let mut predecessor = item(51_011, "Execution predecessor", 60);
    predecessor.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(9)));
    let mut successor = item(51_012, "Reserved execution successor", 60);
    successor.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::StartToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });
    let mut input = request(vec![successor.clone(), predecessor.clone()]);
    input.availability = vec![availability(9, 10)];

    let plan = Scheduler
        .plan_with_execution(
            &input,
            &execution_context(vec![in_flight_defer_work(successor.id, 60)]),
        )
        .unwrap();

    let violation = plan
        .violations
        .iter()
        .find(|violation| violation.kind == ViolationKind::Dependency)
        .expect("the retained execution block must expose its hard conflict");
    assert_eq!(violation.severity, ViolationSeverity::Error);
    assert_eq!(violation.penalty, 0);
    assert!(violation.item_ids.contains(&predecessor.id));
    assert!(violation.item_ids.contains(&successor.id));
}

#[test]
fn deferred_replacement_checks_a_new_aggregate_start_for_hard_fs_and_ss() {
    for relation in [
        DependencyRelation::FinishToStart,
        DependencyRelation::StartToStart,
    ] {
        let mut predecessor = item(51_013, "Predecessor before deferred work", 60);
        predecessor.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(10)));
        let mut successor = item(51_014, "Partly deferred successor", 120);
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        });
        let mut input = request(vec![successor.clone(), predecessor.clone()]);
        input.availability = vec![availability(8, 13)];
        let execution = execution_context(vec![in_flight_defer_work(successor.id, 60)]);
        let mut candidate = defer_candidate(successor.id);
        candidate.move_start = DAY + Duration::hours(12);
        candidate.move_end = candidate.move_start + Duration::minutes(49);

        let result = Scheduler
            .assess_defer_candidate(&input, &execution, &candidate)
            .unwrap();

        let planned_blocks = planned(&result.plan, &successor);
        let [planned_block] = planned_blocks.as_slice() else {
            panic!("the residual work must use one planned block: {relation:?}");
        };
        assert_eq!(
            planned_block.start,
            DAY + Duration::hours(11),
            "{relation:?}"
        );
        assert_eq!(planned_block.end, candidate.move_start, "{relation:?}");
        assert!(
            planned_block.start < candidate.move_start,
            "the planned block must become the aggregate first block: {relation:?}"
        );
        assert!(
            result
                .plan
                .violations
                .iter()
                .all(|violation| violation.kind != ViolationKind::Dependency),
            "{relation:?}"
        );
        assert!(
            result
                .assessment
                .violations
                .iter()
                .all(|violation| violation.code != ManualPlacementViolationCode::Dependency),
            "{relation:?}"
        );
    }
}

#[test]
fn deferred_item_does_not_hide_a_nonmanual_retained_start_conflict() {
    for relation in [
        DependencyRelation::FinishToStart,
        DependencyRelation::StartToStart,
    ] {
        let mut predecessor = item(51_017, "Predecessor after an ordinary pin", 60);
        predecessor.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(10)));
        let mut successor = item(51_018, "Deferred item with an ordinary pin", 180);
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        });
        let mut input = request(vec![successor.clone(), predecessor.clone()]);
        input.availability = vec![availability(8, 14)];
        input.previous_assignments = vec![PreviousAssignment {
            item_id: successor.id,
            occurrence_id: None,
            blocks: vec![PreviousBlock {
                start: DAY + Duration::hours(8),
                end: DAY + Duration::hours(9),
                session_index: 2,
            }],
            pinned: true,
            manual_placement_id: None,
        }];
        let execution = execution_context(vec![in_flight_defer_work(successor.id, 60)]);
        let mut candidate = defer_candidate(successor.id);
        candidate.replacement_session_index = 3;
        candidate.move_start = DAY + Duration::hours(12);
        candidate.move_end = candidate.move_start + Duration::minutes(49);

        let result = Scheduler
            .assess_defer_candidate(&input, &execution, &candidate)
            .unwrap();

        let dependency_violations = result
            .plan
            .violations
            .iter()
            .filter(|violation| violation.kind == ViolationKind::Dependency)
            .collect::<Vec<_>>();
        assert_eq!(dependency_violations.len(), 1, "{relation:?}");
        assert_eq!(
            dependency_violations[0].severity,
            ViolationSeverity::Error,
            "{relation:?}"
        );
        assert!(
            dependency_violations[0].item_ids.contains(&predecessor.id),
            "{relation:?}"
        );
        assert!(
            dependency_violations[0].item_ids.contains(&successor.id),
            "{relation:?}"
        );
        assert!(
            result
                .assessment
                .violations
                .iter()
                .all(|violation| violation.code != ManualPlacementViolationCode::Dependency),
            "the non-boundary defer target must not duplicate the plan-level conflict: {relation:?}"
        );
    }
}

#[test]
fn deferred_replacement_uses_its_retained_aggregate_finish_for_hard_ff_and_sf() {
    for relation in [
        DependencyRelation::FinishToFinish,
        DependencyRelation::StartToFinish,
    ] {
        let mut predecessor = item(51_015, "Predecessor before retained finish", 60);
        predecessor.constraints.earliest_start = Some(Qualified::hard(DAY + Duration::hours(10)));
        let mut successor = item(51_016, "Deferred final boundary", 120);
        successor.constraints.dependencies.push(Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        });
        let mut input = request(vec![successor.clone(), predecessor.clone()]);
        input.availability = vec![availability(8, 13)];
        let execution = execution_context(vec![in_flight_defer_work(successor.id, 60)]);
        let mut candidate = defer_candidate(successor.id);
        candidate.move_start = DAY + Duration::hours(12);
        candidate.move_end = candidate.move_start + Duration::minutes(49);

        let result = Scheduler
            .assess_defer_candidate(&input, &execution, &candidate)
            .unwrap();

        let planned_blocks = planned(&result.plan, &successor);
        let [planned_block] = planned_blocks.as_slice() else {
            panic!("the residual work must use one planned block: {relation:?}");
        };
        assert_eq!(
            planned_block.start,
            DAY + Duration::hours(8),
            "the later retained block already establishes the aggregate finish: {relation:?}"
        );
        assert!(planned_block.end < candidate.move_start, "{relation:?}");
        assert!(
            result
                .plan
                .violations
                .iter()
                .all(|violation| violation.kind != ViolationKind::Dependency),
            "{relation:?}"
        );
        assert!(
            result
                .assessment
                .violations
                .iter()
                .all(|violation| violation.code != ManualPlacementViolationCode::Dependency),
            "{relation:?}"
        );
    }
}

#[test]
fn out_of_horizon_calendar_event_dependency_uses_its_authoritative_interval() {
    let event = |value: u128, start: OffsetDateTime, end: OffsetDateTime| WorkItem {
        id: id(value),
        is_sensitive: false,
        revision: 1,
        title: "Out-of-horizon predecessor".to_owned(),
        kind: ItemKind::CalendarEvent(CalendarEventSpec {
            start,
            end,
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
    let dependent = |value: u128, predecessor: ItemId| {
        let mut item = item(value, "Event-dependent work", 30);
        item.constraints.dependencies.push(Dependency {
            item_id: predecessor,
            relation: DependencyRelation::FinishToStart,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        });
        item
    };

    let past_event = event(52, DAY - Duration::hours(2), DAY - Duration::hours(1));
    let past_dependent = dependent(53, past_event.id);
    let past_plan = Scheduler
        .plan(&request(vec![past_event.clone(), past_dependent.clone()]))
        .unwrap();
    assert_eq!(planned(&past_plan, &past_dependent).len(), 1);

    let mut manual_past = request(vec![past_event, past_dependent.clone()]);
    manual_past.previous_assignments = vec![PreviousAssignment {
        item_id: past_dependent.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(8),
            end: DAY + Duration::hours(8) + Duration::minutes(30),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(Uuid::from_u128(54)),
    }];
    let manual_past_plan = Scheduler.plan(&manual_past).unwrap();
    assert!(
        manual_past_plan.manual_placement_assessments[0]
            .violations
            .iter()
            .all(|violation| violation.code != ManualPlacementViolationCode::Dependency)
    );

    let future_event = event(
        55,
        DAY + Duration::days(3),
        DAY + Duration::days(3) + Duration::hours(1),
    );
    let future_dependent = dependent(56, future_event.id);
    let future_plan = Scheduler
        .plan(&request(vec![
            future_event.clone(),
            future_dependent.clone(),
        ]))
        .unwrap();
    assert!(planned(&future_plan, &future_dependent).is_empty());

    let mut manual_future = request(vec![future_event, future_dependent.clone()]);
    manual_future.previous_assignments = vec![PreviousAssignment {
        item_id: future_dependent.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(8),
            end: DAY + Duration::hours(8) + Duration::minutes(30),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(Uuid::from_u128(57)),
    }];
    let manual_future_plan = Scheduler.plan(&manual_future).unwrap();
    assert!(
        manual_future_plan.manual_placement_assessments[0]
            .violations
            .iter()
            .any(|violation| violation.code == ManualPlacementViolationCode::Dependency)
    );
}

#[test]
fn planning_boundary_rejects_invalid_dependency_edges() {
    let mut self_referencing = item(44, "Self reference", 20);
    self_referencing.constraints.dependencies.push(Dependency {
        item_id: self_referencing.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });
    assert!(matches!(
        Scheduler.plan(&request(vec![self_referencing])),
        Err(ScheduleError::InvalidItem { message, .. })
            if message == "dependency cannot reference its owning item"
    ));

    let predecessor = item(45, "Predecessor", 20);
    let mut excessive_lag = item(46, "Excessive lag", 20);
    excessive_lag.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::StartToFinish,
        minimum_lag: Minutes(MAX_DEPENDENCY_LAG_MINUTES + 1),
        strength: ConstraintStrength::Hard,
    });
    assert!(matches!(
        Scheduler.plan(&request(vec![predecessor.clone(), excessive_lag])),
        Err(ScheduleError::InvalidItem { message, .. })
            if message.contains("dependency lag must be at most")
    ));

    let mut excessive_weight = item(47, "Excessive weight", 20);
    excessive_weight.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Soft {
            weight: MAX_DEPENDENCY_WEIGHT + 1,
        },
    });
    assert!(matches!(
        Scheduler.plan(&request(vec![predecessor, excessive_weight])),
        Err(ScheduleError::InvalidItem { message, .. })
            if message.contains("dependency soft weight must be at most")
    ));
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
        missed_policy: HabitMissedPolicy::Ask,
        minimum_spacing: Minutes::ZERO,
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
fn blocked_work_rejects_manual_and_ordinary_pinned_assignments() {
    let mut blocked = item(860, "Blocked pinned work", 60);
    blocked.status = WorkStatus::Blocked;

    for manual_placement_id in [None, Some(Uuid::from_u128(861))] {
        let mut input = request(vec![blocked.clone()]);
        input.previous_assignments = vec![PreviousAssignment {
            item_id: blocked.id,
            occurrence_id: None,
            blocks: vec![PreviousBlock {
                start: DAY + Duration::hours(10),
                end: DAY + Duration::hours(11),
                session_index: 0,
            }],
            pinned: true,
            manual_placement_id,
        }];

        assert!(matches!(
            Scheduler.plan(&input),
            Err(ScheduleError::InvalidItem { item_id, message })
                if item_id == blocked.id && message.contains("executable work")
        ));
    }
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
fn manual_partial_predecessor_readiness_uses_the_relation_predecessor_boundary() {
    for (relation, reports_dependency_violation) in [
        (DependencyRelation::FinishToStart, true),
        (DependencyRelation::StartToStart, false),
        (DependencyRelation::FinishToFinish, true),
        (DependencyRelation::StartToFinish, false),
    ] {
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
            relation,
            minimum_lag: Minutes::ZERO,
            strength: ConstraintStrength::Hard,
        }];
        let successor_id = successor.id;
        let placement_id = Uuid::from_u128(93);
        let mut input = request(vec![predecessor.clone(), successor]);
        input.availability = vec![availability(8, 9)];
        input.previous_assignments = vec![PreviousAssignment {
            item_id: successor_id,
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
        let dependency_violation = assessment
            .violations
            .iter()
            .find(|violation| violation.code == ManualPlacementViolationCode::Dependency);
        assert_eq!(
            dependency_violation.is_some(),
            reports_dependency_violation,
            "{relation:?}"
        );
        if let Some(violation) = dependency_violation {
            assert!(!violation.conflicting_blocks.is_empty(), "{relation:?}");
        }
    }
}

#[test]
fn manual_finish_relations_assess_the_aggregate_successor_finish() {
    for (relation, lag) in [
        (DependencyRelation::FinishToFinish, Minutes::ZERO),
        (DependencyRelation::StartToFinish, Minutes(60)),
    ] {
        let predecessor = calendar_event(
            93_001,
            "Fixed predecessor",
            DAY + Duration::hours(10),
            DAY + Duration::hours(11),
        );
        let mut successor = item(93_002, "Manually split successor", 120);
        successor.split_policy = SplitPolicy::Splittable {
            minimum_session: Minutes(60),
            maximum_session: Minutes(60),
            maximum_sessions: 2,
            minimum_gap: Minutes::ZERO,
            maximum_days: Some(1),
        };
        successor.constraints.dependencies = vec![Dependency {
            item_id: predecessor.id,
            relation,
            minimum_lag: lag,
            strength: ConstraintStrength::Hard,
        }];
        let placement_id = Uuid::from_u128(93_003);
        let mut input = request(vec![predecessor, successor.clone()]);
        input.availability = vec![availability(8, 12)];
        input.previous_assignments = vec![PreviousAssignment {
            item_id: successor.id,
            occurrence_id: None,
            blocks: vec![
                PreviousBlock {
                    start: DAY + Duration::hours(8),
                    end: DAY + Duration::hours(9),
                    session_index: 0,
                },
                PreviousBlock {
                    start: DAY + Duration::hours(11),
                    end: DAY + Duration::hours(12),
                    session_index: 1,
                },
            ],
            pinned: true,
            manual_placement_id: Some(placement_id),
        }];

        let plan = Scheduler.plan(&input).unwrap();
        let assessment = plan
            .manual_placement_assessments
            .iter()
            .find(|assessment| assessment.placement_id == placement_id)
            .expect("manual placement assessment");

        assert!(
            assessment
                .violations
                .iter()
                .all(|violation| violation.code != ManualPlacementViolationCode::Dependency),
            "{relation:?}"
        );
    }
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
fn defer_candidate_rounds_credit_once_and_replaces_only_the_source_session() {
    let mut task = item(30_001, "Split defer source", 120);
    task.split_policy = SplitPolicy::Splittable {
        minimum_session: Minutes(15),
        maximum_session: Minutes(60),
        maximum_sessions: 4,
        minimum_gap: Minutes::ZERO,
        maximum_days: Some(2),
    };
    let mut input = request(vec![task.clone()]);
    input.previous_assignments.push(PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![PreviousBlock {
            start: DAY + Duration::hours(12),
            end: DAY + Duration::hours(13),
            session_index: 2,
        }],
        pinned: true,
        manual_placement_id: None,
    });
    let execution = execution_context(vec![in_flight_defer_work(task.id, 60)]);
    let mut candidate = defer_candidate(task.id);
    candidate.replacement_session_index = 3;

    let stale_replacement_index = defer_candidate(task.id);
    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &execution, &stale_replacement_index),
        Err(ScheduleError::InvalidDeferCandidate { placement_id, message })
            if placement_id == candidate.placement_id && message.contains("next fresh index 3")
    ));

    let first = Scheduler
        .assess_defer_candidate(&input, &execution, &candidate)
        .unwrap();
    let second = Scheduler
        .assess_defer_candidate(&input, &execution, &candidate)
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.assessment.placement_id, candidate.placement_id);
    assert!(first.assessment.violations.is_empty());

    let replacement = first
        .plan
        .blocks_for(task.id)
        .find(|block| block.session_index == candidate.replacement_session_index)
        .expect("exact deferred replacement");
    assert_eq!(replacement.start, candidate.move_start);
    assert_eq!(replacement.end, candidate.move_end);
    assert_eq!((replacement.end - replacement.start).whole_minutes(), 49);
    let remaining_session = first
        .plan
        .blocks_for(task.id)
        .find(|block| block.session_index == 2)
        .expect("the unrelated retained split session remains exact");
    assert_eq!(remaining_session.kind, ScheduleBlockKind::Pinned);
    assert_eq!(remaining_session.start, DAY + Duration::hours(12));
    assert_eq!(remaining_session.end, DAY + Duration::hours(13));
    assert_eq!(
        (remaining_session.end - remaining_session.start).whole_minutes(),
        60
    );
    assert!(planned(&first.plan, &task).is_empty());
    assert!(first.plan.unscheduled.is_empty());

    // 601 seconds normalizes once to 11 credited minutes. The exact source
    // remainder is therefore 49 minutes, never the legacy 2,999 seconds.
    let mut exact_second_remainder = candidate.clone();
    exact_second_remainder.move_end = exact_second_remainder.move_start + Duration::seconds(2_999);
    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &execution, &exact_second_remainder),
        Err(ScheduleError::InvalidDeferCandidate { placement_id, message })
            if placement_id == candidate.placement_id
                && message.contains("exactly 49 whole minutes")
    ));

    let mut encoded = serde_json::to_value(&candidate).unwrap();
    encoded
        .as_object_mut()
        .unwrap()
        .insert("untrusted_extra".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<DeferCandidateAssessmentInput>(encoded).is_err());
}

#[test]
fn defer_candidate_rejects_a_blocked_source_item() {
    let mut task = item(30_003, "Blocked defer source", 60);
    task.status = WorkStatus::Blocked;
    let input = request(vec![task.clone()]);
    let execution = execution_context(vec![in_flight_defer_work(task.id, 60)]);
    let candidate = defer_candidate(task.id);

    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &execution, &candidate),
        Err(ScheduleError::InvalidDeferCandidate { placement_id, message })
            if placement_id == candidate.placement_id
                && message.contains("executable work")
    ));
}

#[test]
fn defer_candidate_surfaces_content_free_immutable_overlap_evidence() {
    const PRIVATE_CONFLICT_TITLE: &str = "SYNTHETIC-DEFER-CONFLICT-TITLE-CANARY";
    let task = item(30_003, "Active source title", 60);
    let mut input = request(vec![task.clone()]);
    input.fixed_blocks.push(FixedBlock {
        id: Uuid::from_u128(30_004),
        is_sensitive: true,
        title: PRIVATE_CONFLICT_TITLE.to_owned(),
        start: DAY + Duration::hours(10),
        end: DAY + Duration::hours(11),
        source: FixedBlockSource::ProtectedTime,
    });
    let execution = execution_context(vec![in_flight_defer_work(task.id, 60)]);
    let mut candidate = defer_candidate(task.id);
    candidate.credited_seconds_after_source = 30 * 60;
    candidate.move_end = candidate.move_start + Duration::minutes(30);

    let result = Scheduler
        .assess_defer_candidate(&input, &execution, &candidate)
        .unwrap();
    let overlap = result
        .assessment
        .violations
        .iter()
        .find(|violation| violation.code == ManualPlacementViolationCode::ImmutableOverlap)
        .expect("immutable overlap evidence");
    assert_eq!(
        overlap.conflicting_block_ids,
        vec![input.fixed_blocks[0].id]
    );
    assert_eq!(overlap.conflicting_blocks.len(), 1);
    assert_eq!(
        overlap.conflicting_blocks[0].kind,
        ScheduleBlockKind::ExternalFixed
    );
    let encoded = serde_json::to_string(&result.assessment).unwrap();
    assert!(!encoded.contains(PRIVATE_CONFLICT_TITLE));
    assert!(!encoded.contains(&task.title));
    assert!(result.plan.violations.iter().any(|violation| {
        violation.kind == ViolationKind::PinnedConflict
            && violation.start == Some(candidate.move_start)
            && violation.end == Some(candidate.move_end)
    }));
    assert!(!result.plan.blocks_for(task.id).any(|block| {
        block.session_index == candidate.source_session_index
            && block.start == DAY + Duration::hours(8)
    }));
}

#[test]
fn defer_candidate_rejects_wrong_source_fresh_index_and_occurrence() {
    let task = item(30_005, "Strict defer identity", 60);
    let input = request(vec![task.clone()]);
    let work = in_flight_defer_work(task.id, 60);
    let execution = execution_context(vec![work.clone()]);
    let candidate = defer_candidate(task.id);

    let mut wrong_source = candidate.clone();
    wrong_source.source_session_index = 9;
    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &execution, &wrong_source),
        Err(ScheduleError::MissingDeferSourceReservation {
            placement_id,
            source_session_index: 9,
        }) if placement_id == candidate.placement_id
    ));

    let mut wrong_fresh_index = candidate.clone();
    wrong_fresh_index.replacement_session_index = 2;
    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &execution, &wrong_fresh_index),
        Err(ScheduleError::InvalidDeferCandidate { placement_id, message })
            if placement_id == candidate.placement_id && message.contains("next fresh index 1")
    ));

    let mut wrong_occurrence = candidate.clone();
    wrong_occurrence.occurrence_id = Some(OccurrenceId(Uuid::from_u128(30_006)));
    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &execution, &wrong_occurrence),
        Err(ScheduleError::MissingDeferSourceWorkUnit {
            placement_id,
            item_id,
            occurrence_id: Some(_),
        }) if placement_id == candidate.placement_id && item_id == task.id
    ));

    let ambiguous_units = execution_context(vec![work.clone(), work.clone()]);
    assert!(matches!(
        Scheduler.assess_defer_candidate(&input, &ambiguous_units, &candidate),
        Err(ScheduleError::AmbiguousDeferSourceWorkUnit { placement_id, .. })
            if placement_id == candidate.placement_id
    ));

    let mut duplicate_reservations = work;
    duplicate_reservations
        .reservations
        .push(duplicate_reservations.reservations[0].clone());
    assert!(matches!(
        Scheduler.assess_defer_candidate(
            &input,
            &execution_context(vec![duplicate_reservations]),
            &candidate,
        ),
        Err(ScheduleError::AmbiguousDeferSourceReservation { placement_id, .. })
            if placement_id == candidate.placement_id
    ));
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
fn only_exact_execution_indices_remove_caller_blocks_below_the_high_water() {
    let task = item(207, "Do not resurrect old blocks", 60);
    let mut input = request(vec![task.clone()]);
    input.previous_assignments.push(PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![
            PreviousBlock {
                start: DAY + Duration::hours(9),
                end: DAY + Duration::hours(9) + Duration::minutes(30),
                session_index: 1,
            },
            PreviousBlock {
                start: DAY + Duration::hours(10),
                end: DAY + Duration::hours(10) + Duration::minutes(30),
                session_index: 2,
            },
        ],
        pinned: true,
        manual_placement_id: None,
    });
    let execution = execution_context(vec![execution_work(task.id, 0, vec![0, 2])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    let pinned = plan
        .blocks_for(task.id)
        .filter(|block| block.kind == ScheduleBlockKind::Pinned)
        .collect::<Vec<_>>();
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].session_index, 1);
    let planned = planned(&plan, &task);
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].session_index, 3);
    assert_eq!((planned[0].end - planned[0].start).whole_minutes(), 30);
}

#[test]
fn manual_split_keeps_unclaimed_sessions_on_both_sides_of_execution_history() {
    let mut task = item(207_001, "Move the unconsumed split sessions", 90);
    task.split_policy = SplitPolicy::Splittable {
        minimum_session: Minutes(30),
        maximum_session: Minutes(30),
        maximum_sessions: 3,
        minimum_gap: Minutes::ZERO,
        maximum_days: None,
    };
    let mut input = request(vec![task.clone()]);
    input.previous_assignments.push(PreviousAssignment {
        item_id: task.id,
        occurrence_id: None,
        blocks: vec![
            PreviousBlock {
                start: DAY + Duration::hours(9),
                end: DAY + Duration::hours(9) + Duration::minutes(30),
                session_index: 0,
            },
            PreviousBlock {
                start: DAY + Duration::hours(10),
                end: DAY + Duration::hours(10) + Duration::minutes(30),
                session_index: 1,
            },
            PreviousBlock {
                start: DAY + Duration::hours(11),
                end: DAY + Duration::hours(11) + Duration::minutes(30),
                session_index: 2,
            },
        ],
        pinned: true,
        manual_placement_id: Some(Uuid::from_u128(207_002)),
    });
    let execution = execution_context(vec![execution_work(task.id, 1_800, vec![1])]);

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    assert_eq!(
        plan.blocks_for(task.id)
            .map(|block| block.session_index)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert!(planned(&plan, &task).is_empty());
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
