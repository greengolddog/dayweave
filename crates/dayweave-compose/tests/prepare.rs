use std::collections::BTreeSet;

use chrono::{TimeZone as _, Utc};
use dayweave_compose::{
    AvailabilityInput, CanonicalItem, CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy,
    ComposeScheduleRequest, EnergyInput, FixedBlockInput, FixedBlockSourceInput,
    PrepareScheduleError, PreviousAssignmentInput, PreviousBlockInput, SchedulerConfigInput,
    prepare_canonical_schedule, validate_schedule_request,
};
use dayweave_core::{
    ConstraintStrength, EnergyLevel, FixedBlockSource, ItemId, ItemKind, Minutes, Recurrence,
    SplitPolicy, WorkStatus,
};
use serde_json::json;
use uuid::Uuid;

fn canonical_item(value: u128) -> CanonicalItem {
    CanonicalItem {
        id: Uuid::from_u128(value),
        is_sensitive: false,
        kind: CanonicalItemKind::Task,
        status: CanonicalItemStatus::Planned,
        title: format!("Canonical item {value}"),
        notes: None,
        timezone_name: "Europe/Madrid".into(),
        duration_seconds: Some(3_600),
        deadline_at: Some(Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap()),
        earliest_start_at: None,
        recurrence: None,
        flexible_constraints: json!({}),
        split_policy: CanonicalSplitPolicy::Indivisible,
        importance: 80,
        urgency: 60,
        parent_id: None,
        sibling_order: 0,
        is_executable: true,
        revision: 3,
        created_at: Utc.with_ymd_and_hms(2026, 8, 29, 8, 0, 0).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 8, 29, 8, 0, 0).unwrap(),
        completed_at: None,
        deleted_at: None,
    }
}

fn preview_request() -> ComposeScheduleRequest {
    ComposeScheduleRequest {
        as_of: Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap(),
        horizon_start: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
        horizon_end: Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).unwrap(),
        timezone_name: "Europe/Madrid".into(),
        availability: vec![AvailabilityInput {
            start: Utc.with_ymd_and_hms(2026, 9, 1, 7, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 9, 1, 16, 0, 0).unwrap(),
            contexts: BTreeSet::from(["desk".into()]),
            location: Some("home".into()),
            energy: EnergyInput::Deep,
        }],
        fixed_blocks: Vec::new(),
        previous_assignments: Vec::new(),
        config: SchedulerConfigInput::default(),
        recurrence_context: dayweave_core::RecurrenceContext::default(),
    }
}

#[test]
fn maps_canonical_fields_without_running_the_scheduler() {
    let mut item = canonical_item(1);
    item.duration_seconds = Some(121);
    item.importance = 81;
    item.urgency = 1;
    item.earliest_start_at = Some(Utc.with_ymd_and_hms(2026, 9, 1, 8, 0, 0).unwrap());
    item.split_policy = CanonicalSplitPolicy::Splittable {
        minimum_chunk_seconds: 61,
        maximum_chunk_seconds: 121,
    };
    item.flexible_constraints = json!({
        "maximum_sessions": 4,
        "minimum_gap_minutes": 15,
        "maximum_split_days": 2,
        "energy": {"value": "deep", "strength": {"level": "hard"}},
        "tags": ["writing"]
    });
    let prepared = prepare_canonical_schedule(vec![item], preview_request()).unwrap();

    assert_eq!(prepared.timezone_name, "Europe/Madrid");
    assert_eq!(prepared.source_item_count, 1);
    assert_eq!(prepared.accepted_item_count, 1);
    assert!(prepared.rejected_items.is_empty());
    assert_eq!(prepared.source_item_revisions[&Uuid::from_u128(1)], 3);
    let mapped = &prepared.plan_request.items[0];
    assert_eq!(mapped.priority.importance, 9);
    assert_eq!(mapped.priority.urgency, 1);
    assert_eq!(mapped.duration.unwrap().expected, Minutes(3));
    assert_eq!(mapped.status, WorkStatus::NotStarted);
    assert_eq!(mapped.tags, BTreeSet::from(["writing".into()]));
    assert_eq!(mapped.energy.as_ref().unwrap().value, EnergyLevel::Deep);
    assert_eq!(
        mapped.energy.as_ref().unwrap().strength,
        ConstraintStrength::Hard
    );
    assert_eq!(
        mapped
            .constraints
            .earliest_start
            .as_ref()
            .unwrap()
            .value
            .hour(),
        10
    );
    assert_eq!(
        mapped.split_policy,
        SplitPolicy::Splittable {
            minimum_session: Minutes(2),
            maximum_session: Minutes(3),
            maximum_sessions: 4,
            minimum_gap: Minutes(15),
            maximum_days: Some(2),
        }
    );
    assert_eq!(
        prepared.plan_request.availability[0].energy,
        EnergyLevel::Deep
    );
    assert_eq!(prepared.plan_request.availability[0].start.hour(), 9);
}

#[test]
fn canonical_source_order_cannot_change_preparation_output() {
    let mut root = canonical_item(30);
    root.is_sensitive = true;
    root.flexible_constraints = json!({"has_own_effort": true});
    let mut child = canonical_item(20);
    child.parent_id = Some(root.id);
    let mut invalid_high = canonical_item(90);
    invalid_high.flexible_constraints = json!({"unsupported": true});
    let mut invalid_low = canonical_item(10);
    invalid_low.flexible_constraints = json!({"also_unsupported": true});
    let forward = vec![
        root.clone(),
        invalid_high.clone(),
        child.clone(),
        invalid_low.clone(),
    ];
    let reverse = vec![invalid_low, child, invalid_high, root];

    let first = prepare_canonical_schedule(forward, preview_request()).unwrap();
    let second = prepare_canonical_schedule(reverse, preview_request()).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        first
            .rejected_items
            .iter()
            .map(|item| item.item_id)
            .collect::<Vec<_>>(),
        vec![Uuid::from_u128(10), Uuid::from_u128(90)]
    );
    assert!(first.effective_sensitivity[&Uuid::from_u128(20)]);
}

#[test]
fn duplicate_canonical_ids_fail_before_snapshot_traversal() {
    let item = canonical_item(7);
    let error =
        prepare_canonical_schedule(vec![item.clone(), item], preview_request()).unwrap_err();
    assert_eq!(
        error,
        PrepareScheduleError::DuplicateCanonicalItem(Uuid::from_u128(7))
    );
}

#[test]
fn ten_thousand_deep_sensitivity_chain_is_iterative() {
    let mut items = Vec::with_capacity(10_000);
    for value in 1_u128..=10_000 {
        let mut item = canonical_item(value);
        item.parent_id = (value < 10_000).then(|| Uuid::from_u128(value + 1));
        item.is_sensitive = value == 10_000;
        items.push(item);
    }

    let prepared = prepare_canonical_schedule(items, preview_request()).unwrap();

    assert_eq!(prepared.source_item_count, 10_000);
    assert_eq!(prepared.accepted_item_count, 10_000);
    assert_eq!(prepared.plan_request.items.len(), 10_000);
    assert!(prepared.effective_sensitivity.values().all(|value| *value));
}

#[test]
fn inbox_subtree_is_accepted_without_parsing_scheduling_metadata() {
    let mut root = canonical_item(100);
    root.status = CanonicalItemStatus::Inbox;
    let mut child = canonical_item(101);
    child.parent_id = Some(root.id);
    child.flexible_constraints = json!({"future_metadata": {"secret": true}});
    let mut request = preview_request();
    request.recurrence_context.completion_anchors.insert(
        ItemId(child.id),
        time::OffsetDateTime::from_unix_timestamp(child.updated_at.timestamp()).unwrap(),
    );

    let prepared = prepare_canonical_schedule(vec![child, root], request).unwrap();

    assert_eq!(prepared.accepted_item_count, 2);
    assert!(prepared.rejected_items.is_empty());
    assert!(prepared.plan_request.items.is_empty());
    assert!(
        prepared
            .plan_request
            .recurrence_context
            .completion_anchors
            .is_empty()
    );
}

#[test]
fn rejected_parent_prunes_descendants_transitively() {
    let mut parent = canonical_item(200);
    parent.flexible_constraints = json!({"unknown": true});
    let mut child = canonical_item(201);
    child.parent_id = Some(parent.id);
    let mut grandchild = canonical_item(202);
    grandchild.parent_id = Some(child.id);

    let prepared =
        prepare_canonical_schedule(vec![grandchild, parent, child], preview_request()).unwrap();

    assert_eq!(prepared.accepted_item_count, 0);
    assert_eq!(prepared.rejected_items.len(), 3);
    assert!(prepared.plan_request.items.is_empty());
    assert_eq!(prepared.rejected_items[0].item_id, Uuid::from_u128(200));
    assert_eq!(prepared.rejected_items[1].item_id, Uuid::from_u128(201));
    assert_eq!(prepared.rejected_items[2].item_id, Uuid::from_u128(202));
}

#[test]
fn ten_thousand_item_orphan_cascade_is_iterative_and_bounded() {
    let mut items = Vec::with_capacity(10_000);
    for value in 1_u128..=10_000 {
        let mut item = canonical_item(value);
        item.parent_id = (value > 1).then(|| Uuid::from_u128(value - 1));
        if value == 1 {
            item.flexible_constraints = json!({"unsupported_root": true});
        }
        items.push(item);
    }

    let prepared = prepare_canonical_schedule(items, preview_request()).unwrap();

    assert_eq!(prepared.source_item_count, 10_000);
    assert_eq!(prepared.accepted_item_count, 0);
    assert_eq!(prepared.rejected_items.len(), 10_000);
    assert!(prepared.plan_request.items.is_empty());
}

#[test]
fn previous_assignments_are_revision_gated_before_block_conversion() {
    let item = canonical_item(300);
    let mut request = preview_request();
    request.previous_assignments = vec![
        PreviousAssignmentInput {
            item_id: item.id,
            item_revision: item.revision - 1,
            occurrence_id: None,
            blocks: vec![PreviousBlockInput {
                start: Utc.with_ymd_and_hms(2026, 9, 1, 8, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
                session_index: 0,
            }],
            pinned: true,
        },
        PreviousAssignmentInput {
            item_id: Uuid::from_u128(301),
            item_revision: 9,
            occurrence_id: None,
            blocks: Vec::new(),
            pinned: false,
        },
    ];

    let prepared = prepare_canonical_schedule(vec![item], request).unwrap();

    assert!(prepared.plan_request.previous_assignments.is_empty());
    assert_eq!(prepared.ignored_previous_assignments.len(), 2);
    assert_eq!(
        prepared.ignored_previous_assignments[0].reason,
        "canonical item revision changed"
    );
    assert_eq!(
        prepared.ignored_previous_assignments[1].reason,
        "canonical item is unavailable for scheduling"
    );
}

#[test]
fn maps_all_statuses_exactly() {
    let cases = [
        (CanonicalItemStatus::Inbox, None),
        (CanonicalItemStatus::Planned, Some(WorkStatus::NotStarted)),
        (CanonicalItemStatus::Scheduled, Some(WorkStatus::Scheduled)),
        (CanonicalItemStatus::InProgress, Some(WorkStatus::Active)),
        (CanonicalItemStatus::Paused, Some(WorkStatus::Paused)),
        (CanonicalItemStatus::Completed, Some(WorkStatus::Completed)),
        (CanonicalItemStatus::Skipped, Some(WorkStatus::Skipped)),
        (CanonicalItemStatus::Cancelled, Some(WorkStatus::Canceled)),
    ];
    for (index, (status, expected)) in cases.into_iter().enumerate() {
        let mut item = canonical_item(400 + index as u128);
        item.status = status;
        let prepared = prepare_canonical_schedule(vec![item], preview_request()).unwrap();
        match expected {
            Some(expected) => assert_eq!(prepared.plan_request.items[0].status, expected),
            None => assert!(prepared.plan_request.items.is_empty()),
        }
    }
}

#[test]
fn maps_every_canonical_kind_and_legacy_recurrence_defaults() {
    let mut task = canonical_item(500);
    task.recurrence = Some(json!({"type": "daily"}));
    let mut habit = canonical_item(501);
    habit.kind = CanonicalItemKind::Habit;
    habit.recurrence = Some(json!({"type": "weekly", "weekdays": ["monday", "friday"]}));
    let mut routine = canonical_item(502);
    routine.kind = CanonicalItemKind::Routine;
    routine.flexible_constraints = json!({"has_own_effort": true, "routine_ordered": true});
    let mut goal = canonical_item(503);
    goal.kind = CanonicalItemKind::Goal;
    goal.flexible_constraints = json!({"has_own_effort": true});
    let mut break_item = canonical_item(504);
    break_item.kind = CanonicalItemKind::Break;
    let mut event = canonical_item(505);
    event.kind = CanonicalItemKind::Event;
    event.duration_seconds = None;
    event.deadline_at = None;
    event.flexible_constraints = json!({
        "calendar_event": {
            "start": "2026-09-01T10:00:00+02:00",
            "end": "2026-09-01T11:00:00+02:00",
            "immutable": true,
            "all_day": false,
            "source_calendar_id": null
        }
    });

    let prepared = prepare_canonical_schedule(
        vec![event, break_item, goal, routine, habit, task],
        preview_request(),
    )
    .unwrap();
    let kinds = prepared
        .plan_request
        .items
        .iter()
        .map(|item| &item.kind)
        .collect::<Vec<_>>();
    assert!(matches!(
        kinds[0],
        ItemKind::RecurringTask(dayweave_core::RecurringTaskSpec {
            recurrence: Recurrence::Daily { times_per_day: 1 }
        })
    ));
    assert!(matches!(
        kinds[1],
        ItemKind::Habit(dayweave_core::HabitSpec {
            recurrence: Recurrence::Weekly {
                times_per_week: 2,
                ..
            },
            ..
        })
    ));
    assert!(matches!(kinds[2], ItemKind::Routine(_)));
    assert!(matches!(kinds[3], ItemKind::Goal(_)));
    assert!(matches!(kinds[4], ItemKind::Break(_)));
    assert!(matches!(kinds[5], ItemKind::CalendarEvent(_)));
}

#[test]
fn calendar_context_counts_as_accepted_without_becoming_work() {
    let mut context = canonical_item(600);
    context.kind = CanonicalItemKind::Event;
    context.duration_seconds = None;
    context.deadline_at = None;
    context.flexible_constraints = json!({
        "calendar_context": {
            "start": "2026-09-01T10:00:00+02:00",
            "end": "2026-09-01T11:00:00+02:00",
            "all_day": false
        }
    });
    let task = canonical_item(601);

    let prepared = prepare_canonical_schedule(vec![context, task], preview_request()).unwrap();

    assert_eq!(prepared.accepted_item_count, 2);
    assert_eq!(prepared.plan_request.items.len(), 1);
    assert_eq!(
        prepared.plan_request.items[0].id,
        ItemId(Uuid::from_u128(601))
    );
}

#[test]
fn generated_recurrence_calendar_preserves_dst_day_length() {
    let mut request = preview_request();
    request.horizon_start = Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap();
    request.horizon_end = Utc.with_ymd_and_hms(2026, 10, 27, 0, 0, 0).unwrap();

    let prepared = prepare_canonical_schedule(Vec::new(), request).unwrap();

    assert_eq!(
        prepared
            .plan_request
            .recurrence_context
            .calendar
            .time_zone_id,
        Some("Europe/Madrid".into())
    );
    assert!(
        prepared
            .plan_request
            .recurrence_context
            .calendar
            .days
            .iter()
            .any(|day| (day.end - day.start).whole_hours() == 25)
    );
}

#[test]
fn request_and_snapshot_dtos_reject_unknown_fields_strictly() {
    let item = canonical_item(700);
    let mut item_json = serde_json::to_value(&item).unwrap();
    item_json["future_field"] = json!(true);
    assert!(serde_json::from_value::<CanonicalItem>(item_json).is_err());

    let mut split_json = serde_json::to_value(&item).unwrap();
    split_json["split_policy"] = json!({"type": "indivisible", "future": true});
    assert!(serde_json::from_value::<CanonicalItem>(split_json).is_err());

    let request = preview_request();
    let mut request_json = serde_json::to_value(&request).unwrap();
    request_json["config"]["future"] = json!(true);
    assert!(serde_json::from_value::<ComposeScheduleRequest>(request_json).is_err());

    let mut availability_json = serde_json::to_value(request).unwrap();
    availability_json["availability"][0]["future"] = json!(true);
    assert!(serde_json::from_value::<ComposeScheduleRequest>(availability_json).is_err());
}

#[test]
fn canonical_snapshot_reestablishes_server_storage_invariants() {
    let mut invalid = canonical_item(800);
    invalid.title = " leading".into();
    assert_eq!(
        prepare_canonical_schedule(vec![invalid], preview_request()).unwrap_err(),
        PrepareScheduleError::InvalidCanonicalItem(Uuid::from_u128(800))
    );

    let mut invalid = canonical_item(801);
    invalid.flexible_constraints = json!([]);
    assert_eq!(
        prepare_canonical_schedule(vec![invalid], preview_request()).unwrap_err(),
        PrepareScheduleError::InvalidCanonicalItem(Uuid::from_u128(801))
    );

    let mut invalid = canonical_item(802);
    invalid.deleted_at = Some(Utc.with_ymd_and_hms(2026, 8, 30, 0, 0, 0).unwrap());
    assert_eq!(
        prepare_canonical_schedule(vec![invalid], preview_request()).unwrap_err(),
        PrepareScheduleError::InvalidCanonicalItem(Uuid::from_u128(802))
    );
}

#[test]
fn validates_request_bounds_and_maps_fixed_sources() {
    let mut invalid = preview_request();
    invalid.config.slot_granularity_minutes = 0;
    assert!(matches!(
        validate_schedule_request(&invalid),
        Err(PrepareScheduleError::InvalidRequest(_))
    ));

    let mut request = preview_request();
    request.fixed_blocks = vec![FixedBlockInput {
        id: Uuid::from_u128(900),
        is_sensitive: true,
        title: "Protected time".into(),
        start: Utc.with_ymd_and_hms(2026, 9, 1, 9, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
        source: FixedBlockSourceInput::ProtectedTime,
    }];
    let prepared = prepare_canonical_schedule(Vec::new(), request).unwrap();
    assert_eq!(
        prepared.plan_request.fixed_blocks[0].source,
        FixedBlockSource::ProtectedTime
    );
    assert_eq!(prepared.plan_request.fixed_blocks[0].start.hour(), 11);
}
