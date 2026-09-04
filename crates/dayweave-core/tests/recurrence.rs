use std::collections::BTreeSet;

use dayweave_core::*;
use serde_json::json;
use time::{Duration, OffsetDateTime, Time, UtcOffset, macros::datetime};
use uuid::Uuid;

const START: OffsetDateTime = datetime!(2026-09-01 0:00 UTC);

fn id(value: u128) -> ItemId {
    ItemId::from_uuid(Uuid::from_u128(value))
}

fn recurring_item(value: u128, recurrence: Recurrence) -> WorkItem {
    WorkItem {
        id: id(value),
        is_sensitive: false,
        revision: 1,
        title: format!("Recurring {value}"),
        kind: ItemKind::RecurringTask(RecurringTaskSpec { recurrence }),
        status: WorkStatus::NotStarted,
        parent_id: None,
        sibling_order: None,
        has_own_effort: false,
        goal_ids: BTreeSet::new(),
        priority: Priority {
            importance: 5,
            urgency: 5,
        },
        duration: Some(DurationEstimate::exact(30)),
        constraints: SchedulingConstraints::default(),
        split_policy: SplitPolicy::Indivisible,
        energy: None,
        tags: BTreeSet::new(),
        created_at: START,
        updated_at: START,
    }
}

fn habit_item(value: u128, recurrence: Recurrence) -> WorkItem {
    let mut item = recurring_item(value, recurrence.clone());
    item.kind = ItemKind::Habit(HabitSpec {
        recurrence,
        target: None,
        preserves_streak_when_paused: true,
        missed_policy: HabitMissedPolicy::Ask,
        minimum_spacing: Minutes::ZERO,
    });
    item
}

fn request(item: WorkItem, start: OffsetDateTime, end: OffsetDateTime) -> PlanRequest {
    PlanRequest {
        as_of: start,
        horizon_start: start,
        horizon_end: end,
        items: vec![item],
        availability: Vec::new(),
        fixed_blocks: Vec::new(),
        previous_assignments: Vec::new(),
        config: SchedulerConfig::default(),
        recurrence_context: RecurrenceContext::default(),
    }
}

fn move_source(item: &WorkItem, occurrence: &Occurrence) -> RecurrenceMoveSource {
    RecurrenceMoveSource {
        item_revision: item.revision,
        identity: occurrence.identity,
        nominal_start: occurrence.nominal_start,
        nominal_end: occurrence.nominal_end,
        local_date: occurrence.local_date,
        ordinal: occurrence.ordinal,
    }
}

fn all_day_availability(start: OffsetDateTime, days: i64) -> Vec<AvailabilityWindow> {
    (0..days)
        .map(|index| AvailabilityWindow {
            start: start + Duration::days(index),
            end: start + Duration::days(index + 1),
            contexts: BTreeSet::new(),
            location: None,
            energy: EnergyLevel::Deep,
        })
        .collect()
}

#[test]
fn daily_occurrences_are_stable_across_overlapping_horizons() {
    let item = recurring_item(1, Recurrence::Daily { times_per_day: 2 });
    let full = request(item.clone(), START, START + Duration::days(4));
    let narrowed = request(item, START + Duration::days(1), START + Duration::days(4));

    let full_occurrences = expand_occurrences(&full).unwrap();
    let narrowed_occurrences = expand_occurrences(&narrowed).unwrap();
    assert_eq!(full_occurrences.len(), 8);
    assert_eq!(narrowed_occurrences.len(), 6);
    let retained: BTreeSet<_> = full_occurrences
        .iter()
        .filter(|value| value.window_start >= narrowed.horizon_start)
        .map(|value| value.id)
        .collect();
    assert_eq!(
        retained,
        narrowed_occurrences.iter().map(|value| value.id).collect()
    );
    assert_eq!(
        full_occurrences
            .iter()
            .map(|value| value.id)
            .collect::<BTreeSet<_>>()
            .len(),
        full_occurrences.len()
    );
}

#[test]
fn weekly_and_monthly_rules_distribute_over_calendar_buckets() {
    let weekly_item = recurring_item(
        2,
        Recurrence::Weekly {
            times_per_week: 3,
            weekdays: BTreeSet::from([DayOfWeek::Monday, DayOfWeek::Wednesday, DayOfWeek::Friday]),
        },
    );
    let monday = datetime!(2026-08-31 0:00 UTC);
    let weekly =
        expand_occurrences(&request(weekly_item, monday, monday + Duration::days(7))).unwrap();
    assert_eq!(weekly.len(), 3);
    assert_eq!(
        weekly
            .iter()
            .map(|value| value.local_date.unwrap().weekday())
            .collect::<Vec<_>>(),
        vec![
            time::Weekday::Monday,
            time::Weekday::Wednesday,
            time::Weekday::Friday,
        ]
    );

    let monthly_item = recurring_item(3, Recurrence::Monthly { times_per_month: 4 });
    let october = datetime!(2026-10-01 0:00 UTC);
    let monthly = expand_occurrences(&request(
        monthly_item,
        october,
        datetime!(2026-11-01 0:00 UTC),
    ))
    .unwrap();
    assert_eq!(monthly.len(), 4);
    assert_eq!(
        monthly
            .iter()
            .map(|value| value.local_date.unwrap().day())
            .collect::<Vec<_>>(),
        vec![1, 8, 16, 24]
    );
}

#[test]
fn rolling_interval_keeps_the_in_progress_cycle_and_stable_id() {
    let mut item = recurring_item(
        4,
        Recurrence::EveryInterval {
            interval: Minutes(120),
        },
    );
    item.created_at = START + Duration::hours(6);
    let broad = expand_occurrences(&request(
        item.clone(),
        START + Duration::hours(8),
        START + Duration::hours(14),
    ))
    .unwrap();
    let narrow = expand_occurrences(&request(
        item,
        START + Duration::hours(9) + Duration::minutes(30),
        START + Duration::hours(14),
    ))
    .unwrap();

    assert_eq!(broad.len(), 3);
    assert_eq!(narrow.len(), 3);
    assert_eq!(broad[0].id, narrow[0].id);
    assert_eq!(
        narrow[0].window_start,
        START + Duration::hours(9) + Duration::minutes(30)
    );
    assert_eq!(narrow[0].window_end, START + Duration::hours(10));
}

#[test]
fn after_completion_generates_only_the_next_due_occurrence() {
    let item = recurring_item(
        5,
        Recurrence::AfterCompletion {
            interval: Minutes(180),
        },
    );
    let mut input = request(item.clone(), START, START + Duration::days(1));
    input
        .recurrence_context
        .completion_anchors
        .insert(item.id, START + Duration::hours(9));
    let values = expand_occurrences(&input).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].window_start, START + Duration::hours(12));
    assert_eq!(values[0].nominal_start, START + Duration::hours(12));
    assert_eq!(values[0].nominal_end, START + Duration::hours(15));

    let mut clipped = input.clone();
    clipped.horizon_start = START + Duration::hours(13);
    clipped.horizon_end = START + Duration::hours(16);
    let clipped_value = expand_occurrences(&clipped).unwrap()[0];
    assert_eq!(clipped_value.id, values[0].id);
    assert_eq!(clipped_value.nominal_start, values[0].nominal_start);
    assert_eq!(clipped_value.nominal_end, values[0].nominal_end);
    assert_eq!(clipped_value.window_start, clipped.horizon_start);
    assert_eq!(clipped_value.window_end, values[0].nominal_end);

    input.horizon_end = START + Duration::hours(11);
    assert!(expand_occurrences(&input).unwrap().is_empty());
    let plan = Scheduler.plan(&input).unwrap();
    assert!(
        plan.blocks.is_empty(),
        "an out-of-range series is not scheduled once"
    );
}

#[test]
fn calendar_and_rolling_frequency_have_distinct_anchor_semantics() {
    let calendar_item = recurring_item(
        6,
        Recurrence::Frequency {
            target: 2,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Calendar,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: None,
        },
    );
    let rolling_item = recurring_item(
        7,
        Recurrence::Frequency {
            target: 2,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Rolling,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: Some(START + Duration::hours(6)),
        },
    );
    let end = START + Duration::days(2);
    let calendar = expand_occurrences(&request(calendar_item, START, end)).unwrap();
    let rolling = expand_occurrences(&request(rolling_item, START, end)).unwrap();

    assert_eq!(calendar.len(), 4);
    assert_eq!(calendar[0].window_start, START);
    assert_eq!(calendar[1].window_start, START + Duration::hours(12));
    assert_eq!(rolling[0].window_start, START + Duration::hours(6));
    assert_eq!(rolling[1].window_start, START + Duration::hours(18));
}

#[test]
fn rolling_frequency_distributes_remainders_without_overproducing() {
    let item = recurring_item(
        700,
        Recurrence::Frequency {
            target: 7,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Rolling,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: Some(START),
        },
    );
    let broad =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(2))).unwrap();
    assert_eq!(broad.len(), 14);
    assert_eq!(
        broad[..7]
            .iter()
            .map(|occurrence| (occurrence.nominal_start - START).whole_minutes())
            .collect::<Vec<_>>(),
        vec![0, 205, 411, 617, 822, 1_028, 1_234]
    );
    assert_eq!(broad[7].nominal_start, START + Duration::minutes(1_440));
    assert!(broad.windows(2).all(|pair| matches!(
        (pair[1].nominal_start - pair[0].nominal_start).whole_minutes(),
        205 | 206
    )));

    for shifted_start in [1, 206, 700] {
        let start = START + Duration::minutes(shifted_start);
        let end = start + Duration::days(1);
        let shifted = expand_occurrences(&request(item.clone(), start, end)).unwrap();
        assert_eq!(
            shifted
                .iter()
                .filter(|occurrence| {
                    occurrence.nominal_start >= start && occurrence.nominal_start < end
                })
                .count(),
            7
        );
        for occurrence in &shifted {
            let matching = broad
                .iter()
                .find(|candidate| candidate.id == occurrence.id)
                .expect("two-day expansion covers each shifted occurrence");
            assert_eq!(matching.nominal_start, occurrence.nominal_start);
            assert_eq!(matching.nominal_end, occurrence.nominal_end);
        }
    }
}

#[test]
fn rolling_identity_binds_its_anchor_and_window_frequency() {
    let occurrence = |target, anchor| {
        let item = recurring_item(
            704,
            Recurrence::Frequency {
                target,
                period: RecurrencePeriod::Day,
                semantics: RecurrenceSemantics::Rolling,
                weekdays: BTreeSet::new(),
                minimum_spacing: Minutes::ZERO,
                anchor: Some(anchor),
            },
        );
        expand_occurrences(&request(item, START, START + Duration::days(1))).unwrap()[0]
    };
    let baseline = occurrence(7, START);
    assert_ne!(baseline.id, occurrence(8, START).id);
    assert_ne!(baseline.id, occurrence(7, START + Duration::minutes(1)).id);
}

#[test]
fn rolling_frequency_supports_one_per_minute_and_rederives_move_gaps() {
    let item = recurring_item(
        701,
        Recurrence::Frequency {
            target: 1_440,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Rolling,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: Some(START + Duration::hours(3)),
        },
    );
    let minute_values = expand_occurrences(&request(
        item,
        START + Duration::hours(3),
        START + Duration::hours(4),
    ))
    .unwrap();
    assert_eq!(minute_values.len(), 60);
    assert!(
        minute_values
            .iter()
            .all(|value| value.nominal_end - value.nominal_start == Duration::minutes(1))
    );

    let item = recurring_item(
        702,
        Recurrence::Frequency {
            target: 7,
            period: RecurrencePeriod::Day,
            semantics: RecurrenceSemantics::Rolling,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: Some(START),
        },
    );
    let mut input = request(item.clone(), START, START + Duration::days(1));
    let baseline = expand_occurrences(&input).unwrap();
    let source = baseline[1];
    let moved_start = START + Duration::hours(20);
    input
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: item.id,
            selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
            action: RecurrenceExceptionAction::Move {
                start: moved_start,
                end: moved_start + Duration::minutes(30),
                source: move_source(&item, &source),
            },
        });
    assert!(expand_occurrences(&input).is_ok());
    let RecurrenceExceptionAction::Move { source, .. } =
        &mut input.recurrence_context.exceptions[0].action
    else {
        unreachable!();
    };
    source.nominal_end += Duration::minutes(1);
    assert_eq!(
        expand_occurrences(&input),
        Err(RecurrenceError::InvalidMoveSource(item.id))
    );
}

#[test]
fn rolling_week_frequency_preserves_remainders_and_arbitrary_window_counts() {
    let anchor = START + Duration::minutes(37);
    let item = recurring_item(
        703,
        Recurrence::Frequency {
            target: 1_000,
            period: RecurrencePeriod::Week,
            semantics: RecurrenceSemantics::Rolling,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: Some(anchor),
        },
    );
    let broad =
        expand_occurrences(&request(item.clone(), anchor, anchor + Duration::weeks(2))).unwrap();
    assert_eq!(broad.len(), 2_000);
    assert!(broad.windows(2).all(|pair| matches!(
        (pair[1].nominal_start - pair[0].nominal_start).whole_minutes(),
        10 | 11
    )));

    for shift in [1, 11, 5_039] {
        let start = anchor + Duration::minutes(shift);
        let end = start + Duration::weeks(1);
        let shifted = expand_occurrences(&request(item.clone(), start, end)).unwrap();
        assert_eq!(
            shifted
                .iter()
                .filter(|occurrence| {
                    occurrence.nominal_start >= start && occurrence.nominal_start < end
                })
                .count(),
            1_000,
        );
        for occurrence in shifted {
            let same = broad
                .iter()
                .find(|candidate| candidate.id == occurrence.id)
                .expect("the broad horizon covers every shifted occurrence");
            assert_eq!(same.nominal_start, occurrence.nominal_start);
            assert_eq!(same.nominal_end, occurrence.nominal_end);
        }
    }
}

#[test]
fn partial_progress_materializes_only_bounded_remaining_work() {
    for (case, basis_points, expected_remaining) in
        [(0_u128, 1_u16, 30_u32), (1, 5_000, 15), (2, 9_999, 1)]
    {
        let item = habit_item(720 + case, Recurrence::Daily { times_per_day: 1 });
        let mut input = request(item.clone(), START, START + Duration::days(1));
        input.availability = all_day_availability(START, 1);
        let occurrence = expand_occurrences(&input).unwrap()[0];
        input.recurrence_context.partial_progress.insert(
            occurrence.id,
            RecurrencePartialProgress {
                progress_basis_points: basis_points,
                expected_duration_minutes: Minutes(30),
                remaining_duration_minutes: None,
            },
        );
        let plan = Scheduler.plan(&input).unwrap();
        let scheduled = plan
            .blocks_for(item.id)
            .map(|block| u32::try_from((block.end - block.start).whole_minutes()).unwrap())
            .sum::<u32>();
        assert_eq!(scheduled, expected_remaining, "{basis_points} bp");
    }

    let item = habit_item(730, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(item.clone(), START, START + Duration::days(1));
    input.availability = all_day_availability(START, 1);
    let occurrence = expand_occurrences(&input).unwrap()[0];
    input.recurrence_context.partial_progress.insert(
        occurrence.id,
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: Some(Minutes(7)),
        },
    );
    let plan = Scheduler.plan(&input).unwrap();
    assert_eq!(
        plan.blocks_for(item.id)
            .map(|block| (block.end - block.start).whole_minutes())
            .sum::<i64>(),
        7
    );
}

#[test]
fn partial_progress_rejects_terminal_unknown_and_invalid_evidence() {
    let item = habit_item(731, Recurrence::Daily { times_per_day: 1 });
    let base = request(item.clone(), START, START + Duration::days(1));
    let occurrence = expand_occurrences(&base).unwrap()[0];
    for progress in [
        RecurrencePartialProgress {
            progress_basis_points: 0,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: None,
        },
        RecurrencePartialProgress {
            progress_basis_points: 10_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: None,
        },
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: Some(Minutes::ZERO),
        },
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: Some(Minutes(31)),
        },
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(MAX_DEPENDENCY_LAG_MINUTES + 1),
            remaining_duration_minutes: None,
        },
    ] {
        let mut invalid = base.clone();
        invalid
            .recurrence_context
            .partial_progress
            .insert(occurrence.id, progress);
        assert_eq!(
            expand_occurrences(&invalid),
            Err(RecurrenceError::InvalidPartialProgress(occurrence.id))
        );
    }

    let unknown = OccurrenceId(Uuid::from_u128(999_999));
    let mut invalid = base.clone();
    invalid.recurrence_context.partial_progress.insert(
        unknown,
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: None,
        },
    );
    assert_eq!(
        expand_occurrences(&invalid),
        Err(RecurrenceError::InvalidPartialProgress(unknown))
    );

    let mut terminal = base;
    terminal
        .recurrence_context
        .completed_occurrence_ids
        .insert(occurrence.id);
    terminal.recurrence_context.partial_progress.insert(
        occurrence.id,
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: None,
        },
    );
    assert_eq!(
        expand_occurrences(&terminal),
        Err(RecurrenceError::InvalidPartialProgress(occurrence.id))
    );

    let non_habit = recurring_item(733, Recurrence::Daily { times_per_day: 1 });
    let mut invalid_owner = request(non_habit, START, START + Duration::days(1));
    let non_habit_occurrence = expand_occurrences(&invalid_owner).unwrap()[0];
    invalid_owner.recurrence_context.partial_progress.insert(
        non_habit_occurrence.id,
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: None,
        },
    );
    assert_eq!(
        expand_occurrences(&invalid_owner),
        Err(RecurrenceError::InvalidPartialProgress(
            non_habit_occurrence.id
        )),
    );
}

#[test]
fn partial_progress_binds_to_a_restored_move_before_final_state_validation() {
    let item = habit_item(735, Recurrence::Daily { times_per_day: 1 });
    let source =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(1))).unwrap()[0];
    let horizon_start = START + Duration::days(1);
    let moved_start = horizon_start + Duration::hours(9);
    let mut destination = request(
        item.clone(),
        horizon_start,
        horizon_start + Duration::days(1),
    );
    destination.availability = all_day_availability(horizon_start, 1);
    destination
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: item.id,
            selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
            action: RecurrenceExceptionAction::Move {
                start: moved_start,
                end: moved_start + Duration::hours(1),
                source: move_source(&item, &source),
            },
        });
    destination.recurrence_context.partial_progress.insert(
        source.id,
        RecurrencePartialProgress {
            progress_basis_points: 5_000,
            expected_duration_minutes: Minutes(30),
            remaining_duration_minutes: None,
        },
    );
    let plan = Scheduler.plan(&destination).unwrap();
    assert_eq!(
        plan.blocks
            .iter()
            .filter(|block| block.occurrence_id == Some(source.id))
            .map(|block| (block.end - block.start).whole_minutes())
            .sum::<i64>(),
        15,
    );

    destination.recurrence_context.pauses.push(RecurrencePause {
        item_id: item.id,
        start: moved_start,
        end: moved_start + Duration::hours(1),
    });
    let occurrences = expand_occurrences(&destination).unwrap();
    assert_eq!(
        occurrences
            .iter()
            .find(|occurrence| occurrence.id == source.id)
            .unwrap()
            .state,
        OccurrenceState::Paused,
    );
    assert!(
        Scheduler
            .plan(&destination)
            .unwrap()
            .blocks
            .iter()
            .all(|block| block.occurrence_id != Some(source.id))
    );
}

#[test]
fn habit_quantity_target_never_falls_back_to_a_duration() {
    let mut item = habit_item(732, Recurrence::Daily { times_per_day: 1 });
    let ItemKind::Habit(spec) = &mut item.kind else {
        unreachable!();
    };
    spec.target = Some(QuantityTarget {
        amount: 10_000,
        unit: "steps".to_owned(),
    });
    item.duration = None;
    let mut input = request(item.clone(), START, START + Duration::days(1));
    input.availability = all_day_availability(START, 1);
    let plan = Scheduler.plan(&input).unwrap();
    assert!(plan.blocks_for(item.id).next().is_none());
    assert!(plan.unscheduled.iter().any(|work| {
        work.item_id == item.id && work.reason == UnscheduledReason::MissingDuration
    }));
}

#[test]
fn completed_paused_skipped_and_moved_occurrences_integrate_with_planning() {
    let item = recurring_item(8, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(item.clone(), START, START + Duration::days(4));
    input.availability = all_day_availability(START, 4);
    let baseline = expand_occurrences(&input).unwrap();

    input
        .recurrence_context
        .completed_occurrence_ids
        .insert(baseline[0].id);
    input.recurrence_context.pauses.push(RecurrencePause {
        item_id: item.id,
        start: START + Duration::days(1),
        end: START + Duration::days(2),
    });
    input
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: item.id,
            selector: RecurrenceExceptionSelector::LocalDate {
                date: baseline[2].local_date.unwrap(),
            },
            action: RecurrenceExceptionAction::Skip,
        });
    let moved_start = START + Duration::days(3) + Duration::hours(15);
    input
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: item.id,
            selector: RecurrenceExceptionSelector::Occurrence { id: baseline[3].id },
            action: RecurrenceExceptionAction::Move {
                start: moved_start,
                end: moved_start + Duration::hours(1),
                source: move_source(&item, &baseline[3]),
            },
        });

    let expanded = expand_occurrences(&input).unwrap();
    assert_eq!(
        expanded.iter().map(|value| value.state).collect::<Vec<_>>(),
        vec![
            OccurrenceState::Completed,
            OccurrenceState::Paused,
            OccurrenceState::Skipped,
            OccurrenceState::Generated,
        ]
    );
    let plan = Scheduler.plan(&input).unwrap();
    let blocks: Vec<_> = plan.blocks_for(item.id).collect();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].start, moved_start);
    assert_eq!(blocks[0].occurrence_id, Some(baseline[3].id));
    assert_eq!(plan.occurrences, expanded);
}

#[test]
fn carry_and_reduce_missed_decisions_materialize_as_exact_scheduler_actions() {
    let item = habit_item(734, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(item.clone(), START, START + Duration::days(3));
    let baseline = expand_occurrences(&input).unwrap();
    let missed = baseline[0];
    let as_of = missed.window_end;
    let ItemKind::Habit(mut carry_spec) = item.kind.clone() else {
        unreachable!();
    };
    carry_spec.missed_policy = HabitMissedPolicy::Carry;

    let carry = decide_configured_habit_missed_behavior(
        &carry_spec,
        as_of,
        missed.window_start,
        missed.window_end,
        &HabitOccurrenceValue::pending(),
        &[],
    )
    .unwrap();
    input.recurrence_context.exceptions =
        materialize_habit_missed_scheduling_decision(item.revision, &missed, &carry, &baseline)
            .unwrap();
    let carried = expand_occurrences(&input)
        .unwrap()
        .into_iter()
        .find(|occurrence| occurrence.id == missed.id)
        .unwrap();
    assert_eq!(carried.window_start, as_of);
    assert_eq!(carried.window_end, as_of + Duration::days(1));

    let reduce = HabitMissedDecision::ReduceFrequency {
        skip_next_occurrences: 1,
    };
    input.recurrence_context.exceptions =
        materialize_habit_missed_scheduling_decision(item.revision, &missed, &reduce, &baseline)
            .unwrap();
    let reduced = expand_occurrences(&input).unwrap();
    assert_eq!(
        reduced
            .iter()
            .find(|occurrence| occurrence.id == baseline[1].id)
            .unwrap()
            .state,
        OccurrenceState::Skipped,
    );
}

#[test]
fn occurrence_id_move_survives_from_its_nominal_day_to_its_destination_day() {
    let item = recurring_item(800, Recurrence::Daily { times_per_day: 1 });
    let source_start = START;
    let destination_start = START + Duration::days(1);
    let destination_end = destination_start + Duration::days(1);
    let source_occurrence =
        expand_occurrences(&request(item.clone(), source_start, destination_start))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
    let moved_start = destination_start + Duration::hours(9);
    let exception = RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence {
            id: source_occurrence.id,
        },
        action: RecurrenceExceptionAction::Move {
            start: moved_start,
            end: moved_start + Duration::hours(1),
            source: move_source(&item, &source_occurrence),
        },
    };

    let mut source_request = request(item.clone(), source_start, destination_start);
    source_request.availability = all_day_availability(source_start, 1);
    source_request
        .recurrence_context
        .exceptions
        .push(exception.clone());
    let source_expansion = expand_occurrences(&source_request).unwrap();
    assert_eq!(source_expansion.len(), 1);
    assert_eq!(source_expansion[0].id, source_occurrence.id);
    assert_eq!(source_expansion[0].state, OccurrenceState::Paused);
    assert!(Scheduler.plan(&source_request).unwrap().blocks.is_empty());

    let mut destination_request = request(item.clone(), destination_start, destination_end);
    destination_request.availability = all_day_availability(destination_start, 1);
    destination_request
        .recurrence_context
        .exceptions
        .push(exception.clone());
    let destination_expansion = expand_occurrences(&destination_request).unwrap();
    assert_eq!(destination_expansion.len(), 2);
    let restored = destination_expansion
        .iter()
        .find(|occurrence| occurrence.id == source_occurrence.id)
        .unwrap();
    assert_eq!(restored.window_start, moved_start);
    assert_eq!(restored.window_end, moved_start + Duration::hours(1));
    assert_eq!(restored.state, OccurrenceState::Generated);
    assert_eq!(restored.nominal_start, source_occurrence.nominal_start);
    assert_eq!(restored.nominal_end, source_occurrence.nominal_end);
    assert_eq!(restored.local_date, source_occurrence.local_date);
    assert_eq!(restored.ordinal, source_occurrence.ordinal);
    let destination_plan = Scheduler.plan(&destination_request).unwrap();
    let moved_block = destination_plan
        .blocks_for(item.id)
        .find(|block| block.occurrence_id == Some(source_occurrence.id))
        .unwrap();
    assert_eq!(moved_block.start, moved_start);

    let after_start = destination_end;
    let mut after_request = request(item, after_start, after_start + Duration::days(1));
    after_request.availability = all_day_availability(after_start, 1);
    after_request.recurrence_context.exceptions.push(exception);
    let after_expansion = expand_occurrences(&after_request).unwrap();
    assert_eq!(after_expansion.len(), 1);
    assert_ne!(after_expansion[0].id, source_occurrence.id);

    let mut wide_request = request(
        recurring_item(800, Recurrence::Daily { times_per_day: 1 }),
        source_start,
        destination_end,
    );
    wide_request.recurrence_context.exceptions =
        source_request.recurrence_context.exceptions.clone();
    let wide_occurrence = expand_occurrences(&wide_request)
        .unwrap()
        .into_iter()
        .find(|occurrence| occurrence.id == source_occurrence.id)
        .unwrap();
    assert_eq!(
        (
            wide_occurrence.nominal_start,
            wide_occurrence.nominal_end,
            wide_occurrence.local_date,
            wide_occurrence.ordinal,
            wide_occurrence.window_start,
            wide_occurrence.window_end,
        ),
        (
            restored.nominal_start,
            restored.nominal_end,
            restored.local_date,
            restored.ordinal,
            restored.window_start,
            restored.window_end,
        ),
    );
}

#[test]
fn a_move_cannot_bypass_a_pause_covering_its_destination() {
    let item = habit_item(801, Recurrence::Daily { times_per_day: 1 });
    let source_request = request(item.clone(), START, START + Duration::days(1));
    let source = expand_occurrences(&source_request).unwrap()[0];
    let moved_start = START + Duration::days(1) + Duration::hours(9);
    let mut destination = request(
        item.clone(),
        START + Duration::days(1),
        START + Duration::days(2),
    );
    destination
        .recurrence_context
        .exceptions
        .push(RecurrenceException {
            item_id: item.id,
            selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
            action: RecurrenceExceptionAction::Move {
                start: moved_start,
                end: moved_start + Duration::hours(1),
                source: move_source(&item, &source),
            },
        });
    destination.recurrence_context.pauses.push(RecurrencePause {
        item_id: item.id,
        start: moved_start,
        end: moved_start + Duration::hours(1),
    });
    let moved = expand_occurrences(&destination)
        .unwrap()
        .into_iter()
        .find(|occurrence| occurrence.id == source.id)
        .unwrap();
    assert_eq!(moved.state, OccurrenceState::Paused);
    assert!(
        Scheduler
            .plan(&destination)
            .unwrap()
            .blocks_for(item.id)
            .all(|block| block.occurrence_id != Some(source.id))
    );
}

#[test]
fn cross_horizon_move_source_and_boundaries_fail_closed() {
    let item = recurring_item(803, Recurrence::Daily { times_per_day: 1 });
    let source_request = request(item.clone(), START, START + Duration::days(1));
    let source = expand_occurrences(&source_request).unwrap()[0];
    let target_start = START + Duration::days(1) + Duration::hours(23) + Duration::minutes(30);
    let mut crossing = source_request.clone();
    crossing.horizon_start = START + Duration::days(1);
    crossing.horizon_end = START + Duration::days(2);
    crossing.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: target_start,
            end: target_start + Duration::hours(1),
            source: move_source(&item, &source),
        },
    }];
    assert_eq!(
        serde_json::to_value(crossing.recurrence_context.exceptions[0].action).unwrap(),
        json!({
            "type": "move",
            "start": "2026-09-02T23:30:00Z",
            "end": "2026-09-03T00:30:00Z",
            "source": {
                "item_revision": 1,
                "identity": {
                    "type": "calendar_day",
                    "date": "2026-09-01",
                    "bucket_ordinal": 0
                },
                "nominal_start": "2026-09-01T00:00:00Z",
                "nominal_end": "2026-09-02T00:00:00Z",
                "local_date": "2026-09-01",
                "ordinal": 0
            }
        }),
    );
    assert_eq!(
        expand_occurrences(&crossing).unwrap_err(),
        RecurrenceError::MoveCrossesHorizon(item.id),
    );

    assert!(
        serde_json::from_value::<RecurrenceExceptionAction>(json!({
            "type": "move",
            "start": "2026-09-02T09:00:00Z",
            "end": "2026-09-02T10:00:00Z"
        }))
        .is_err(),
        "move source is mandatory at the wire boundary"
    );

    let mut stale_source = crossing;
    stale_source.horizon_end = START + Duration::days(3);
    if let RecurrenceExceptionAction::Move { source, .. } =
        &mut stale_source.recurrence_context.exceptions[0].action
    {
        source.item_revision += 1;
    }
    assert_eq!(
        expand_occurrences(&stale_source).unwrap_err(),
        RecurrenceError::InvalidMoveSource(item.id),
    );
}

#[test]
fn move_identity_rejects_fabricated_ids_and_tampered_ordinals() {
    let item = recurring_item(807, Recurrence::Daily { times_per_day: 1 });
    let source =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(1))).unwrap()[0];
    let moved_start = START + Duration::days(1) + Duration::hours(9);
    let make_request = |id, source_envelope| {
        let mut input = request(
            item.clone(),
            START + Duration::days(1),
            START + Duration::days(2),
        );
        input.recurrence_context.exceptions = vec![RecurrenceException {
            item_id: item.id,
            selector: RecurrenceExceptionSelector::Occurrence { id },
            action: RecurrenceExceptionAction::Move {
                start: moved_start,
                end: moved_start + Duration::hours(1),
                source: source_envelope,
            },
        }];
        input
    };

    let fabricated = OccurrenceId(Uuid::new_v5(&item.id.0, b"daily:plausible-but-not-issued"));
    assert_eq!(
        expand_occurrences(&make_request(fabricated, move_source(&item, &source))).unwrap_err(),
        RecurrenceError::InvalidMoveSource(item.id),
    );

    let mut tampered = move_source(&item, &source);
    tampered.ordinal = u32::MAX;
    assert_eq!(
        expand_occurrences(&make_request(source.id, tampered)).unwrap_err(),
        RecurrenceError::InvalidMoveSource(item.id),
    );
}

#[test]
fn only_occurrence_selectors_can_move_recurring_work() {
    let item = recurring_item(808, Recurrence::Daily { times_per_day: 1 });
    let source =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(1))).unwrap()[0];
    for selector in [
        RecurrenceExceptionSelector::LocalDate {
            date: source.local_date.unwrap(),
        },
        RecurrenceExceptionSelector::NominalStart {
            at: source.nominal_start,
        },
    ] {
        let mut input = request(item.clone(), START, START + Duration::days(2));
        input.recurrence_context.exceptions = vec![RecurrenceException {
            item_id: item.id,
            selector,
            action: RecurrenceExceptionAction::Move {
                start: START + Duration::days(1) + Duration::hours(9),
                end: START + Duration::days(1) + Duration::hours(10),
                source: move_source(&item, &source),
            },
        }];
        assert_eq!(
            expand_occurrences(&input).unwrap_err(),
            RecurrenceError::InvalidMoveSource(item.id),
        );
    }
}

#[test]
fn weekly_and_monthly_identity_is_stable_for_partial_buckets() {
    let monday = datetime!(2026-08-31 0:00 UTC);
    let weekly_item = recurring_item(
        809,
        Recurrence::Weekly {
            times_per_week: 3,
            weekdays: BTreeSet::from([DayOfWeek::Monday, DayOfWeek::Wednesday, DayOfWeek::Friday]),
        },
    );
    let full_week = expand_occurrences(&request(
        weekly_item.clone(),
        monday,
        monday + Duration::days(7),
    ))
    .unwrap();
    let wednesday = monday + Duration::days(2);
    let partial_week = expand_occurrences(&request(
        weekly_item,
        wednesday,
        wednesday + Duration::days(1),
    ))
    .unwrap();
    assert_eq!(partial_week.len(), 1);
    assert_eq!(partial_week[0].id, full_week[1].id);
    assert_eq!(partial_week[0].identity, full_week[1].identity);

    let october = datetime!(2026-10-01 0:00 UTC);
    let monthly_item = recurring_item(810, Recurrence::Monthly { times_per_month: 4 });
    let full_month = expand_occurrences(&request(
        monthly_item.clone(),
        october,
        datetime!(2026-11-01 0:00 UTC),
    ))
    .unwrap();
    let sixteenth = datetime!(2026-10-16 0:00 UTC);
    let partial_month = expand_occurrences(&request(
        monthly_item,
        sixteenth,
        sixteenth + Duration::days(1),
    ))
    .unwrap();
    assert_eq!(partial_month.len(), 1);
    assert_eq!(partial_month[0].id, full_month[2].id);
    assert_eq!(partial_month[0].identity, full_month[2].identity);
}

#[test]
fn out_of_horizon_calendar_bucket_never_uses_a_fabricated_dst_offset() {
    let item = recurring_item(
        816,
        Recurrence::Weekly {
            times_per_week: 1,
            weekdays: BTreeSet::from([DayOfWeek::Monday]),
        },
    );
    let start = datetime!(2026-10-25 0:00 +02:00);
    let day_end = datetime!(2026-10-26 0:00 +01:00);
    let mut input = request(item, start, day_end - Duration::minutes(30));
    input.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Sunday,
        days: vec![ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 10 - 25),
            start,
            end: day_end,
        }],
    };
    assert!(expand_occurrences(&input).unwrap().is_empty());
}

#[test]
fn after_completion_move_accepts_a_new_horizon_end() {
    let item = recurring_item(
        811,
        Recurrence::AfterCompletion {
            interval: Minutes(60),
        },
    );
    let mut source_request = request(item.clone(), START, START + Duration::days(1));
    source_request
        .recurrence_context
        .completion_anchors
        .insert(item.id, START);
    let source = expand_occurrences(&source_request).unwrap()[0];
    let moved_start = START + Duration::days(2) + Duration::hours(9);
    let exception = RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: moved_start,
            end: moved_start + Duration::hours(1),
            source: move_source(&item, &source),
        },
    };
    let mut destination = request(
        item.clone(),
        START + Duration::days(2),
        START + Duration::days(3),
    );
    destination
        .recurrence_context
        .completion_anchors
        .insert(item.id, START);
    destination.recurrence_context.exceptions = vec![exception];
    let restored = expand_occurrences(&destination).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, source.id);
    assert_eq!(restored[0].window_start, moved_start);
    assert_eq!(restored[0].nominal_end, source.nominal_end);
}

#[test]
fn custom_rrule_ids_are_stable_across_horizons_and_equivalent_parsing() {
    let rule = "FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=12";
    let item = recurring_item(
        813,
        Recurrence::Custom {
            rrule: rule.to_owned(),
        },
    );
    let broad =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(15))).unwrap();
    let narrow_start = START + Duration::days(6);
    let narrow = expand_occurrences(&request(
        item.clone(),
        narrow_start,
        START + Duration::days(15),
    ))
    .unwrap();
    assert_eq!(
        broad
            .iter()
            .filter(|occurrence| occurrence.nominal_start >= narrow_start)
            .map(|occurrence| occurrence.id)
            .collect::<Vec<_>>(),
        narrow
            .iter()
            .map(|occurrence| occurrence.id)
            .collect::<Vec<_>>()
    );
    assert!(broad.iter().enumerate().all(|(expected, occurrence)| {
        matches!(
            occurrence.identity,
            RecurrenceOccurrenceIdentity::CustomRule { sequence, .. }
                if usize::try_from(sequence) == Ok(expected)
        )
    }));

    let equivalent = recurring_item(
        813,
        Recurrence::Custom {
            rrule: "rrule:count=12;byday=fr,we,mo;interval=1;freq=weekly".to_owned(),
        },
    );
    let reparsed =
        expand_occurrences(&request(equivalent, START, START + Duration::days(15))).unwrap();
    assert_eq!(
        broad.iter().map(|value| value.id).collect::<Vec<_>>(),
        reparsed.iter().map(|value| value.id).collect::<Vec<_>>()
    );

    let semantically_different = recurring_item(
        813,
        Recurrence::Custom {
            rrule: "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR;COUNT=12".to_owned(),
        },
    );
    let different = expand_occurrences(&request(
        semantically_different,
        START,
        START + Duration::days(15),
    ))
    .unwrap();
    assert_ne!(broad[0].id, different[0].id);
}

#[test]
fn custom_rrule_uses_resolved_dst_boundaries() {
    let mut item = recurring_item(
        814,
        Recurrence::Custom {
            rrule: "FREQ=DAILY;COUNT=4".to_owned(),
        },
    );
    item.created_at = datetime!(2026-10-24 0:00 +02:00);
    item.updated_at = item.created_at;
    let days = vec![
        ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 10 - 24),
            start: datetime!(2026-10-24 0:00 +02:00),
            end: datetime!(2026-10-25 0:00 +02:00),
        },
        ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 10 - 25),
            start: datetime!(2026-10-25 0:00 +02:00),
            end: datetime!(2026-10-26 0:00 +01:00),
        },
        ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 10 - 26),
            start: datetime!(2026-10-26 0:00 +01:00),
            end: datetime!(2026-10-27 0:00 +01:00),
        },
    ];
    let mut broad_request = request(
        item.clone(),
        datetime!(2026-10-24 0:00 +02:00),
        datetime!(2026-10-27 0:00 +01:00),
    );
    broad_request.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Paris".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: days.clone(),
    };
    let broad = expand_occurrences(&broad_request).unwrap();
    assert_eq!(broad.len(), 3);
    let fall_back = broad
        .iter()
        .find(|occurrence| occurrence.local_date == Some(time::macros::date!(2026 - 10 - 25)))
        .unwrap();
    assert_eq!(
        fall_back.nominal_end - fall_back.nominal_start,
        Duration::hours(25)
    );

    let mut narrow_request = request(
        item,
        datetime!(2026-10-25 0:00 +02:00),
        datetime!(2026-10-27 0:00 +01:00),
    );
    narrow_request.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Paris".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: days.into_iter().skip(1).collect(),
    };
    let narrow = expand_occurrences(&narrow_request).unwrap();
    assert_eq!(narrow[0].id, fall_back.id);
    assert_eq!(narrow[0].nominal_start, fall_back.nominal_start);
    assert_eq!(narrow[0].nominal_end, fall_back.nominal_end);
}

#[test]
fn custom_rrule_move_source_is_rederived_and_tampering_is_rejected() {
    let item = recurring_item(
        815,
        Recurrence::Custom {
            rrule: "FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=30".to_owned(),
        },
    );
    let source =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(7))).unwrap()[0];
    let move_start = START + Duration::days(19) + Duration::hours(9);
    let mut destination = request(
        item.clone(),
        START + Duration::days(19),
        START + Duration::days(20),
    );
    destination.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("UTC".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: (0..20)
            .map(|offset| ZonedDayBoundary {
                local_date: (START + Duration::days(offset)).date(),
                start: START + Duration::days(offset),
                end: START + Duration::days(offset + 1),
            })
            .collect(),
    };
    destination.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: move_start,
            end: move_start + Duration::hours(1),
            source: move_source(&item, &source),
        },
    }];
    let moved = expand_occurrences(&destination).unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].id, source.id);
    assert_eq!(moved[0].window_start, move_start);

    let mut tampered_source = move_source(&item, &source);
    if let RecurrenceOccurrenceIdentity::CustomRule {
        rule_id,
        sequence,
        date,
    } = tampered_source.identity
    {
        tampered_source.identity = RecurrenceOccurrenceIdentity::CustomRule {
            rule_id,
            sequence: sequence + 1,
            date,
        };
        tampered_source.ordinal += 1;
    } else {
        panic!("custom expansion must use a verifiable identity");
    }
    let RecurrenceExceptionAction::Move { source, .. } =
        &mut destination.recurrence_context.exceptions[0].action
    else {
        unreachable!();
    };
    *source = tampered_source;
    assert_eq!(
        expand_occurrences(&destination),
        Err(RecurrenceError::InvalidMoveSource(item.id))
    );
}

#[test]
fn unsupported_or_unbounded_custom_rrules_fail_closed() {
    let cases = [
        (
            "FREQ=YEARLY;BYMONTH=9;BYMONTHDAY=1;COUNT=2",
            "custom RRULE frequency YEARLY is unsupported",
        ),
        (
            "FREQ=DAILY;INTERVAL=2",
            "custom RRULE must define exactly one finite COUNT or UNTIL",
        ),
        (
            "FREQ=MONTHLY;BYDAY=1MO;COUNT=2",
            "custom RRULE does not support ordinal BYDAY entries",
        ),
    ];
    for (rrule, message) in cases {
        let item = recurring_item(
            816,
            Recurrence::Custom {
                rrule: rrule.to_owned(),
            },
        );
        assert_eq!(
            expand_occurrences(&request(item.clone(), START, START + Duration::days(1))),
            Err(RecurrenceError::InvalidRule {
                item_id: item.id,
                message: message.to_owned(),
            })
        );
    }
}

#[test]
fn move_source_preserves_non_utc_rfc3339_offsets() {
    let item = recurring_item(814, Recurrence::Daily { times_per_day: 1 });
    let madrid_start = datetime!(2026-09-01 0:00 +02:00);
    let madrid_end = datetime!(2026-09-02 0:00 +02:00);
    let mut source_request = request(item.clone(), madrid_start, madrid_end);
    source_request.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: vec![ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 09 - 01),
            start: madrid_start,
            end: madrid_end,
        }],
    };
    let source = expand_occurrences(&source_request).unwrap()[0];
    let action = RecurrenceExceptionAction::Move {
        start: datetime!(2026-09-02 9:00 +02:00),
        end: datetime!(2026-09-02 10:00 +02:00),
        source: move_source(&item, &source),
    };
    let encoded = serde_json::to_value(action).unwrap();
    assert_eq!(
        encoded["source"]["nominal_start"],
        json!("2026-09-01T00:00:00+02:00")
    );
    assert_eq!(
        encoded["source"]["nominal_end"],
        json!("2026-09-02T00:00:00+02:00")
    );
}

#[test]
fn off_horizon_calendar_move_rejects_unproved_cross_dst_source_offsets() {
    let item = recurring_item(819, Recurrence::Daily { times_per_day: 1 });
    let source_start = datetime!(2026-09-01 0:00 +02:00);
    let source_end = datetime!(2026-09-02 0:00 +02:00);
    let mut source_request = request(item.clone(), source_start, source_end);
    source_request.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: vec![ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 09 - 01),
            start: source_start,
            end: source_end,
        }],
    };
    let source = expand_occurrences(&source_request).unwrap()[0];
    let destination_start = datetime!(2026-10-30 0:00 +01:00);
    let mut destination = request(
        item.clone(),
        destination_start,
        destination_start + Duration::days(1),
    );
    destination.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: destination_start + Duration::hours(9),
            end: destination_start + Duration::hours(10),
            source: move_source(&item, &source),
        },
    }];
    assert_eq!(
        expand_occurrences(&destination),
        Err(RecurrenceError::InvalidMoveSource(item.id)),
    );
}

#[test]
fn move_accepts_a_calendar_occurrence_spanning_the_fall_dst_day() {
    let item = recurring_item(818, Recurrence::Daily { times_per_day: 1 });
    let start = datetime!(2026-10-25 0:00 +02:00);
    let end = datetime!(2026-10-26 0:00 +01:00);
    let mut input = request(item.clone(), start, end);
    input.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: vec![ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 10 - 25),
            start,
            end,
        }],
    };
    let source = expand_occurrences(&input).unwrap()[0];
    assert_eq!(source.nominal_end, end);
    assert_eq!(source.nominal_end.offset(), end.offset());
    let moved_start = datetime!(2026-10-25 9:00 +01:00);
    input.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: moved_start,
            end: moved_start + Duration::hours(1),
            source: move_source(&item, &source),
        },
    }];
    let moved = expand_occurrences(&input).unwrap();
    assert_eq!(moved[0].window_start, moved_start);
}

#[test]
fn hostile_rolling_identity_arithmetic_fails_without_panicking() {
    let item = recurring_item(
        815,
        Recurrence::EveryInterval {
            interval: Minutes(5_000_000),
        },
    );
    let index = i64::from(i32::MAX);
    let occurrence_id = OccurrenceId(Uuid::new_v5(
        &item.id.0,
        format!("interval:{index}").as_bytes(),
    ));
    let moved_start = START + Duration::hours(9);
    let mut input = request(item.clone(), START, START + Duration::days(1));
    input.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: occurrence_id },
        action: RecurrenceExceptionAction::Move {
            start: moved_start,
            end: moved_start + Duration::hours(1),
            source: RecurrenceMoveSource {
                item_revision: item.revision,
                identity: RecurrenceOccurrenceIdentity::RollingMinutes {
                    index,
                    anchor: item.created_at,
                },
                nominal_start: START,
                nominal_end: START + Duration::hours(1),
                local_date: None,
                ordinal: u32::try_from(index).unwrap(),
            },
        },
    }];
    assert_eq!(
        expand_occurrences(&input).unwrap_err(),
        RecurrenceError::InvalidMoveSource(item.id),
    );
}

#[test]
fn rolling_month_move_keeps_source_metadata_when_horizon_offset_changes() {
    let anchor = datetime!(2026-09-01 0:00 +02:00);
    let item = recurring_item(
        817,
        Recurrence::Frequency {
            target: 1,
            period: RecurrencePeriod::Month,
            semantics: RecurrenceSemantics::Rolling,
            weekdays: BTreeSet::new(),
            minimum_spacing: Minutes::ZERO,
            anchor: Some(anchor),
        },
    );
    let source_horizon_start = datetime!(2026-10-01 0:00 +02:00);
    let source_horizon_end = datetime!(2026-11-01 0:00 +01:00);
    let mut source_request = request(item.clone(), source_horizon_start, source_horizon_end);
    let summer = UtcOffset::from_hms(2, 0, 0).unwrap();
    let winter = UtcOffset::from_hms(1, 0, 0).unwrap();
    let days: Vec<_> = (1..=31)
        .map(|day| {
            let date = time::Date::from_calendar_date(2026, time::Month::October, day).unwrap();
            let next = date.next_day().unwrap();
            let start_offset = if day <= 25 { summer } else { winter };
            let end_offset = if day < 25 { summer } else { winter };
            ZonedDayBoundary {
                local_date: date,
                start: date.with_time(Time::MIDNIGHT).assume_offset(start_offset),
                end: next.with_time(Time::MIDNIGHT).assume_offset(end_offset),
            }
        })
        .collect();
    source_request.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: days.clone(),
    };
    let source = expand_occurrences(&source_request)
        .unwrap()
        .into_iter()
        .find(|occurrence| {
            matches!(
                occurrence.identity,
                RecurrenceOccurrenceIdentity::RollingMonth { cycle: 1, .. }
            )
        })
        .unwrap();
    assert_eq!(source.nominal_start.offset(), summer);
    assert_eq!(source.nominal_end.offset(), winter);
    let moved_start = datetime!(2026-10-30 9:00 +01:00);
    let mut destination = request(
        item.clone(),
        datetime!(2026-10-30 0:00 +01:00),
        datetime!(2026-10-31 0:00 +01:00),
    );
    destination.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days,
    };
    destination.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: moved_start,
            end: moved_start + Duration::hours(1),
            source: move_source(&item, &source),
        },
    }];
    let restored = expand_occurrences(&destination)
        .unwrap()
        .into_iter()
        .find(|occurrence| occurrence.id == source.id)
        .unwrap();
    assert_eq!(restored.nominal_start, source.nominal_start);
    assert_eq!(
        restored.nominal_start.offset(),
        source.nominal_start.offset()
    );
    assert_eq!(restored.nominal_end, source.nominal_end);
    assert_eq!(restored.window_start, moved_start);
}

#[test]
fn inert_skip_survives_when_a_series_is_no_longer_recurring() {
    let mut item = recurring_item(812, Recurrence::Daily { times_per_day: 1 });
    let occurrence =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(1))).unwrap()[0];
    item.kind = ItemKind::Task;
    let mut input = request(item.clone(), START, START + Duration::days(1));
    input.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: occurrence.id },
        action: RecurrenceExceptionAction::Skip,
    }];
    assert!(expand_occurrences(&input).unwrap().is_empty());
}

#[test]
fn moved_occurrence_metadata_keeps_mixed_selectors_horizon_stable() {
    let item = recurring_item(804, Recurrence::Daily { times_per_day: 1 });
    let source =
        expand_occurrences(&request(item.clone(), START, START + Duration::days(1))).unwrap()[0];
    let moved_start = START + Duration::days(1) + Duration::hours(9);
    let move_exception = RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::Occurrence { id: source.id },
        action: RecurrenceExceptionAction::Move {
            start: moved_start,
            end: moved_start + Duration::hours(1),
            source: move_source(&item, &source),
        },
    };
    let skip_by_original_date = RecurrenceException {
        item_id: item.id,
        selector: RecurrenceExceptionSelector::LocalDate {
            date: source.local_date.unwrap(),
        },
        action: RecurrenceExceptionAction::Skip,
    };
    for (start, end) in [
        (START, START + Duration::days(2)),
        (START + Duration::days(1), START + Duration::days(2)),
    ] {
        let mut input = request(item.clone(), start, end);
        input.recurrence_context.exceptions =
            vec![move_exception.clone(), skip_by_original_date.clone()];
        let moved = expand_occurrences(&input)
            .unwrap()
            .into_iter()
            .find(|occurrence| occurrence.id == source.id)
            .unwrap();
        assert_eq!(moved.state, OccurrenceState::Skipped);
        assert_eq!(moved.nominal_start, source.nominal_start);
        assert_eq!(moved.local_date, source.local_date);
    }
}

#[test]
fn cross_series_occurrence_id_collision_is_rejected() {
    let first = recurring_item(805, Recurrence::Daily { times_per_day: 1 });
    let second = recurring_item(806, Recurrence::Daily { times_per_day: 1 });
    let first_occurrence =
        expand_occurrences(&request(first.clone(), START, START + Duration::days(1))).unwrap()[0];
    let second_occurrence =
        expand_occurrences(&request(second.clone(), START, START + Duration::days(1))).unwrap()[0];
    let mut input = request(first, START, START + Duration::days(1));
    input.items.push(second.clone());
    input.recurrence_context.exceptions = vec![RecurrenceException {
        item_id: second.id,
        selector: RecurrenceExceptionSelector::Occurrence {
            id: first_occurrence.id,
        },
        action: RecurrenceExceptionAction::Move {
            start: START + Duration::hours(9),
            end: START + Duration::hours(10),
            source: move_source(&second, &second_occurrence),
        },
    }];
    assert_eq!(
        expand_occurrences(&input).unwrap_err(),
        RecurrenceError::InvalidMoveSource(second.id),
    );
}

#[test]
fn execution_evidence_maps_only_to_its_exact_recurring_occurrence() {
    let item = recurring_item(801, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(item.clone(), START, START + Duration::days(2));
    input.availability = all_day_availability(START, 2);
    let occurrences = expand_occurrences(&input).unwrap();
    assert_eq!(occurrences.len(), 2);
    let execution = ExecutionPlanningContext {
        snapshot_revision: 1,
        work_units: vec![ExecutionWorkUnit {
            item_id: item.id,
            occurrence_id: Some(occurrences[0].id),
            progress_epoch: 1,
            credited_seconds: 0,
            disposition: Some(ExecutionDisposition::Skipped),
            used_session_indices: vec![0],
            reservations: Vec::new(),
        }],
    };

    let plan = Scheduler.plan_with_execution(&input, &execution).unwrap();
    let blocks: Vec<_> = plan.blocks_for(item.id).collect();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].occurrence_id, Some(occurrences[1].id));
    assert!(plan.decisions.iter().any(|decision| {
        decision.occurrence_id == Some(occurrences[0].id)
            && decision.kind == DecisionKind::TerminalItemIgnored
    }));
}

#[test]
fn manual_placement_never_falls_back_from_an_unknown_occurrence_identity() {
    let item = recurring_item(811, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(item.clone(), START, START + Duration::days(2));
    input.availability = all_day_availability(START, 2);
    let unknown = OccurrenceId(Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        b"different recurrence occurrence",
    ));
    let placement_id = Uuid::from_u128(812);
    input.previous_assignments = vec![PreviousAssignment {
        item_id: item.id,
        occurrence_id: Some(unknown),
        blocks: vec![PreviousBlock {
            start: START + Duration::hours(9),
            end: START + Duration::hours(9) + Duration::minutes(30),
            session_index: 0,
        }],
        pinned: true,
        manual_placement_id: Some(placement_id),
    }];

    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidRecurrence(message))
            if message.contains(&placement_id.to_string())
    ));
}

#[test]
fn manual_conflict_facts_remap_nested_recurring_block_identity() {
    let target = recurring_item(821, Recurrence::Daily { times_per_day: 1 });
    let obstacle = recurring_item(822, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(target.clone(), START, START + Duration::days(1));
    input.items.push(obstacle.clone());
    input.availability = all_day_availability(START, 1);
    let occurrences = expand_occurrences(&input).unwrap();
    let target_occurrence = occurrences
        .iter()
        .find(|occurrence| occurrence.series_item_id == target.id)
        .expect("target occurrence");
    let obstacle_occurrence = occurrences
        .iter()
        .find(|occurrence| occurrence.series_item_id == obstacle.id)
        .expect("obstacle occurrence");
    let start = START + Duration::hours(9);
    let end = start + Duration::minutes(30);
    input.previous_assignments = vec![
        PreviousAssignment {
            item_id: target.id,
            occurrence_id: Some(target_occurrence.id),
            blocks: vec![PreviousBlock {
                start,
                end,
                session_index: 0,
            }],
            pinned: true,
            manual_placement_id: Some(Uuid::from_u128(823)),
        },
        PreviousAssignment {
            item_id: obstacle.id,
            occurrence_id: Some(obstacle_occurrence.id),
            blocks: vec![PreviousBlock {
                start,
                end,
                session_index: 0,
            }],
            pinned: true,
            manual_placement_id: None,
        },
    ];

    let plan = Scheduler.plan(&input).unwrap();
    let overlap = plan.manual_placement_assessments[0]
        .violations
        .iter()
        .find(|violation| violation.code == ManualPlacementViolationCode::ImmutableOverlap)
        .expect("recurring pinned overlap");
    let conflict = overlap
        .conflicting_blocks
        .iter()
        .find(|conflict| conflict.item_id == Some(obstacle.id))
        .expect("canonical recurring obstacle identity");
    assert_eq!(conflict.occurrence_id, Some(obstacle_occurrence.id));
    assert!(overlap.conflicting_block_ids.contains(&conflict.block_id));
    assert!(plan.blocks.iter().any(|block| {
        block.id == conflict.block_id
            && block.item_id == Some(obstacle.id)
            && block.occurrence_id == Some(obstacle_occurrence.id)
    }));
}

#[test]
fn overlapping_reservation_for_unmaterialized_occurrence_fails_closed() {
    let item = recurring_item(802, Recurrence::Daily { times_per_day: 1 });
    let input = request(item.clone(), START, START + Duration::days(1));
    let missing_occurrence = OccurrenceId(Uuid::from_u128(999_802));
    let execution = ExecutionPlanningContext {
        snapshot_revision: 1,
        work_units: vec![ExecutionWorkUnit {
            item_id: item.id,
            occurrence_id: Some(missing_occurrence),
            progress_epoch: 1,
            credited_seconds: 0,
            disposition: None,
            used_session_indices: vec![0],
            reservations: vec![ExecutionReservation {
                session_index: 1,
                start: START + Duration::hours(8),
                end: START + Duration::hours(9),
                kind: ExecutionReservationKind::DeferredReplacement {
                    source_session_index: 0,
                },
            }],
        }],
    };

    assert!(matches!(
        Scheduler.plan_with_execution(&input, &execution),
        Err(ScheduleError::InvalidItem { item_id, message })
            if item_id == item.id
                && message.contains(&missing_occurrence.to_string())
                && message.contains("does not map to materialized work")
    ));
}

#[test]
fn explicit_timezone_days_preserve_spring_and_fall_dst_lengths() {
    let item = recurring_item(9, Recurrence::Daily { times_per_day: 2 });
    let spring_start = datetime!(2026-03-28 0:00 +01:00);
    let spring_end = datetime!(2026-03-31 0:00 +02:00);
    let mut spring = request(item.clone(), spring_start, spring_end);
    spring.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: vec![
            ZonedDayBoundary {
                local_date: time::macros::date!(2026 - 03 - 28),
                start: datetime!(2026-03-28 0:00 +01:00),
                end: datetime!(2026-03-29 0:00 +01:00),
            },
            ZonedDayBoundary {
                local_date: time::macros::date!(2026 - 03 - 29),
                start: datetime!(2026-03-29 0:00 +01:00),
                end: datetime!(2026-03-30 0:00 +02:00),
            },
            ZonedDayBoundary {
                local_date: time::macros::date!(2026 - 03 - 30),
                start: datetime!(2026-03-30 0:00 +02:00),
                end: datetime!(2026-03-31 0:00 +02:00),
            },
        ],
    };
    let spring_values = expand_occurrences(&spring).unwrap();
    let transition: Vec<_> = spring_values
        .iter()
        .filter(|value| value.local_date == Some(time::macros::date!(2026 - 03 - 29)))
        .collect();
    assert_eq!(transition.len(), 2);
    assert_eq!(
        (transition[0].window_end - transition[0].window_start)
            + (transition[1].window_end - transition[1].window_start),
        Duration::hours(23)
    );

    let fall_start = datetime!(2026-10-25 0:00 +02:00);
    let fall_end = datetime!(2026-10-26 0:00 +01:00);
    let mut fall = request(item, fall_start, fall_end);
    fall.recurrence_context.calendar = RecurrenceCalendar {
        time_zone_id: Some("Europe/Madrid".to_owned()),
        week_starts_on: DayOfWeek::Monday,
        days: vec![ZonedDayBoundary {
            local_date: time::macros::date!(2026 - 10 - 25),
            start: fall_start,
            end: fall_end,
        }],
    };
    let fall_values = expand_occurrences(&fall).unwrap();
    assert_eq!(fall_values.len(), 2);
    assert_eq!(
        fall_values
            .iter()
            .map(|value| value.window_end - value.window_start)
            .sum::<Duration>(),
        Duration::hours(25)
    );
}

#[test]
fn recurring_routine_clones_the_tree_and_preserves_step_order() {
    let mut routine = recurring_item(10, Recurrence::Daily { times_per_day: 1 });
    routine.kind = ItemKind::Routine(RoutineSpec {
        ordered: true,
        recurrence: Some(Recurrence::Daily { times_per_day: 1 }),
    });
    routine.duration = None;

    let mut first = recurring_item(11, Recurrence::Daily { times_per_day: 1 });
    first.kind = ItemKind::Task;
    first.parent_id = Some(routine.id);
    first.sibling_order = Some(1);
    let mut second = recurring_item(12, Recurrence::Daily { times_per_day: 1 });
    second.kind = ItemKind::Task;
    second.parent_id = Some(routine.id);
    second.sibling_order = Some(2);

    let mut input = request(routine.clone(), START, START + Duration::days(1));
    input.items = vec![routine.clone(), second.clone(), first.clone()];
    input.as_of = START + Duration::hours(7);
    input.availability = vec![AvailabilityWindow {
        start: START + Duration::hours(8),
        end: START + Duration::hours(10),
        contexts: BTreeSet::new(),
        location: None,
        energy: EnergyLevel::Deep,
    }];

    let plan = Scheduler.plan(&input).unwrap();
    let first_block = plan.blocks_for(first.id).next().unwrap();
    let second_block = plan.blocks_for(second.id).next().unwrap();
    assert!(first_block.end <= second_block.start);
    assert_eq!(first_block.occurrence_id, second_block.occurrence_id);
    assert_eq!(plan.occurrences.len(), 1);
    assert_eq!(plan.occurrences[0].series_item_id, routine.id);
}

#[test]
fn dependencies_cannot_cross_a_materialized_recurring_subtree_boundary() {
    let recurring = recurring_item(1010, Recurrence::Daily { times_per_day: 1 });
    let mut external = recurring_item(1011, Recurrence::Daily { times_per_day: 1 });
    external.kind = ItemKind::Task;
    external.constraints.dependencies.push(Dependency {
        item_id: recurring.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });
    let mut input = request(recurring.clone(), START, START + Duration::days(1));
    input.items = vec![recurring.clone(), external.clone()];
    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidRecurrence(message))
            if message.contains(&external.id.to_string())
                && message.contains(&recurring.id.to_string())
                && message.contains("recurring subtree boundary")
    ));

    let mut other_series = recurring_item(1012, Recurrence::Daily { times_per_day: 1 });
    other_series.constraints.dependencies.push(Dependency {
        item_id: recurring.id,
        relation: DependencyRelation::StartToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Soft { weight: 1 },
    });
    input.items = vec![recurring.clone(), other_series.clone()];
    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidRecurrence(message))
            if message.contains(&other_series.id.to_string())
                && message.contains(&recurring.id.to_string())
    ));

    let mut routine = recurring;
    routine.kind = ItemKind::Routine(RoutineSpec {
        ordered: false,
        recurrence: Some(Recurrence::Daily { times_per_day: 1 }),
    });
    routine.duration = None;
    let mut child = recurring_item(1013, Recurrence::Daily { times_per_day: 1 });
    child.kind = ItemKind::Task;
    child.parent_id = Some(routine.id);
    external.constraints.dependencies[0].item_id = child.id;
    input.items = vec![routine, child.clone(), external.clone()];
    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidRecurrence(message))
            if message.contains(&external.id.to_string())
                && message.contains(&child.id.to_string())
    ));
}

#[test]
fn dependencies_inside_a_recurring_subtree_are_rewritten_per_occurrence() {
    let mut routine = recurring_item(1020, Recurrence::Daily { times_per_day: 1 });
    routine.kind = ItemKind::Routine(RoutineSpec {
        ordered: false,
        recurrence: Some(Recurrence::Daily { times_per_day: 1 }),
    });
    routine.duration = None;

    let mut predecessor = recurring_item(1021, Recurrence::Daily { times_per_day: 1 });
    predecessor.kind = ItemKind::Task;
    predecessor.parent_id = Some(routine.id);
    let mut successor = recurring_item(1022, Recurrence::Daily { times_per_day: 1 });
    successor.kind = ItemKind::Task;
    successor.parent_id = Some(routine.id);
    successor.constraints.dependencies.push(Dependency {
        item_id: predecessor.id,
        relation: DependencyRelation::FinishToStart,
        minimum_lag: Minutes::ZERO,
        strength: ConstraintStrength::Hard,
    });

    let mut input = request(routine.clone(), START, START + Duration::days(2));
    input.items = vec![routine, predecessor.clone(), successor.clone()];
    input.as_of = START;
    input.availability = all_day_availability(START, 2);

    let plan = Scheduler.plan(&input).unwrap();
    let predecessor_blocks = plan.blocks_for(predecessor.id).collect::<Vec<_>>();
    let successor_blocks = plan.blocks_for(successor.id).collect::<Vec<_>>();
    assert_eq!(predecessor_blocks.len(), 2);
    assert_eq!(successor_blocks.len(), 2);
    for successor_block in successor_blocks {
        let predecessor_block = predecessor_blocks
            .iter()
            .find(|block| block.occurrence_id == successor_block.occurrence_id)
            .expect("each cloned successor keeps an occurrence-local predecessor");
        assert!(predecessor_block.end <= successor_block.start);
    }
}

#[test]
fn minimum_spacing_is_enforced_between_occurrence_starts() {
    for (id, recurrence) in [
        (13, Recurrence::Daily { times_per_day: 3 }),
        (
            14,
            Recurrence::Frequency {
                target: 3,
                period: RecurrencePeriod::Day,
                semantics: RecurrenceSemantics::Calendar,
                weekdays: BTreeSet::new(),
                minimum_spacing: Minutes::ZERO,
                anchor: None,
            },
        ),
    ] {
        let mut item = habit_item(id, recurrence);
        let ItemKind::Habit(spec) = &mut item.kind else {
            unreachable!();
        };
        spec.minimum_spacing = Minutes(600);
        let mut input = request(item.clone(), START, START + Duration::days(1));
        input.availability = all_day_availability(START, 1);
        let plan = Scheduler.plan(&input).unwrap();
        let blocks: Vec<_> = plan.blocks_for(item.id).collect();
        assert_eq!(blocks.len(), 3);
        assert!((blocks[1].start - blocks[0].start) >= Duration::hours(10));
        assert!((blocks[2].start - blocks[1].start) >= Duration::hours(10));
    }
}

#[test]
fn old_json_without_recurrence_fields_remains_readable() {
    let item = recurring_item(15, Recurrence::Daily { times_per_day: 1 });
    let input = request(item, START, START + Duration::days(1));
    let mut value = serde_json::to_value(&input).unwrap();
    value.as_object_mut().unwrap().remove("recurrence_context");
    value["items"][0]["constraints"]
        .as_object_mut()
        .unwrap()
        .remove("occurrence_window");
    let decoded: PlanRequest = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.recurrence_context, RecurrenceContext::default());
    assert_eq!(decoded.items[0].constraints.occurrence_window, None);
    assert!(
        serde_json::to_value(RecurrenceContext::default())
            .unwrap()
            .get("partial_progress")
            .is_none()
    );

    let old_rule = r#"{"type":"daily","times_per_day":2}"#;
    assert_eq!(
        serde_json::from_str::<Recurrence>(old_rule).unwrap(),
        Recurrence::Daily { times_per_day: 2 }
    );
    let old_habit = r#"{
        "recurrence":{"type":"daily","times_per_day":1},
        "target":null,
        "preserves_streak_when_paused":true
    }"#;
    let decoded: HabitSpec = serde_json::from_str(old_habit).unwrap();
    assert_eq!(decoded.missed_policy, HabitMissedPolicy::Ask);
    assert_eq!(decoded.minimum_spacing, Minutes::ZERO);
}

#[test]
fn property_style_daily_counts_are_deterministic_and_bounded() {
    for days in 1_i64..=14 {
        for count in 1_u16..=5 {
            let item = recurring_item(
                100 + u128::from(count),
                Recurrence::Daily {
                    times_per_day: count,
                },
            );
            let input = request(item, START, START + Duration::days(days));
            let first = expand_occurrences(&input).unwrap();
            let second = expand_occurrences(&input).unwrap();
            assert_eq!(first, second);
            assert_eq!(
                first.len(),
                usize::try_from(days).unwrap() * usize::from(count)
            );
            let ids: BTreeSet<_> = first.iter().map(|value| value.id).collect();
            assert_eq!(ids.len(), first.len());
            assert!(first.iter().all(|value| {
                input.horizon_start <= value.window_start
                    && value.window_start < value.window_end
                    && value.window_end <= input.horizon_end
            }));
        }
    }
}

#[test]
fn malformed_explicit_timezone_calendar_is_rejected() {
    let item = recurring_item(15, Recurrence::Daily { times_per_day: 1 });
    let mut input = request(item, START, START + Duration::days(2));
    input.recurrence_context.calendar.days = vec![ZonedDayBoundary {
        local_date: time::macros::date!(2026 - 09 - 01),
        start: START,
        end: START + Duration::days(1),
    }];
    assert_eq!(
        expand_occurrences(&input).unwrap_err(),
        RecurrenceError::IncompleteZonedCalendar
    );
    assert!(matches!(
        Scheduler.plan(&input),
        Err(ScheduleError::InvalidRecurrence(_))
    ));
}

#[test]
fn extreme_year_recurrence_offsets_return_an_error_without_panicking() {
    let horizon_end = datetime!(9999-12-31 23:59:59 UTC);
    let horizon_start = horizon_end
        .checked_sub(Duration::hours(2))
        .expect("bounded extreme horizon");
    let mut item = recurring_item(
        900,
        Recurrence::EveryInterval {
            interval: Minutes(u32::MAX),
        },
    );
    item.created_at = horizon_start;
    item.updated_at = horizon_start;
    let mut input = request(item, horizon_start, horizon_end);
    input.recurrence_context.calendar.days = vec![ZonedDayBoundary {
        local_date: horizon_start.date(),
        start: horizon_start,
        end: horizon_end,
    }];

    let outcome = std::panic::catch_unwind(|| expand_occurrences(&input));
    assert!(outcome.is_ok(), "extreme recurrence input must not unwind");
    assert_eq!(
        outcome.expect("catch result").unwrap_err(),
        RecurrenceError::DateOutOfRange
    );
}
