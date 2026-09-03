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
fn custom_rrule_is_retained_but_not_schedulable() {
    let item = recurring_item(
        813,
        Recurrence::Custom {
            rrule: "FREQ=YEARLY;BYMONTH=9;BYMONTHDAY=1".to_owned(),
        },
    );
    assert_eq!(
        expand_occurrences(&request(item.clone(), START, START + Duration::days(1))).unwrap_err(),
        RecurrenceError::InvalidRule {
            item_id: item.id,
            message: "custom RRULE recurrence is retained for read compatibility but is not schedulable until bounded RFC 5545 expansion is available".to_owned(),
        }
    );
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
    let days = (1..=31)
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
        days,
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
