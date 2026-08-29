use std::collections::BTreeSet;

use dayweave_core::*;
use time::{Duration, OffsetDateTime, macros::datetime};
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
fn minimum_spacing_is_enforced_between_occurrence_starts() {
    let item = recurring_item(13, Recurrence::Daily { times_per_day: 3 });
    let mut input = request(item.clone(), START, START + Duration::days(1));
    input.availability = all_day_availability(START, 1);
    input
        .recurrence_context
        .minimum_spacing
        .insert(item.id, Minutes(600));
    let plan = Scheduler.plan(&input).unwrap();
    let blocks: Vec<_> = plan.blocks_for(item.id).collect();
    assert_eq!(blocks.len(), 3);
    assert!((blocks[1].start - blocks[0].start) >= Duration::hours(10));
    assert!((blocks[2].start - blocks[1].start) >= Duration::hours(10));
}

#[test]
fn old_json_without_recurrence_fields_remains_readable() {
    let item = recurring_item(14, Recurrence::Daily { times_per_day: 1 });
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

    let old_rule = r#"{"type":"daily","times_per_day":2}"#;
    assert_eq!(
        serde_json::from_str::<Recurrence>(old_rule).unwrap(),
        Recurrence::Daily { times_per_day: 2 }
    );
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
