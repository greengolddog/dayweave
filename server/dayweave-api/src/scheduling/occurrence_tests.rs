use std::collections::BTreeMap;

use chrono::Duration;
use dayweave_core::{
    ExecutionPlanningContext, ItemId, OccurrenceLifecycleContext, OccurrenceLifecycleInstance,
    OccurrenceLifecycleMember, OccurrenceState, WorkStatus,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::{
    AuthoritativePlanningEvidence, ComposeScheduleRequest, ComposeScheduleResult,
    OCCURRENCE_PUBLICATION_SCHEMA, SCHEDULER_PUBLICATION_SCHEMA,
    compose::{compose_items_with_lifecycle_for_schema, request_digest},
    occurrence::{lifecycle_from_evidence, planning_identities},
    postgres::{publication_content_hash, validate_publishable_compose_result},
};
use crate::{
    item_completion::ItemCompletionReopenState,
    items::{DurationKind, Item, ItemKind, ItemStatus},
    persistence::{RoutineOccurrencePlanningEvidence, RoutineOccurrencePlanningInstance},
    routine_occurrences::{
        RoutineOccurrenceError, RoutineOccurrenceManifest, RoutineOccurrenceMemberDefinition,
        initialize_routine_occurrence,
    },
};

const FIXTURE: &[u8] = include_bytes!(
    "../../../../crates/dayweave-scheduler-helper/tests/fixtures/compose-request-v1.json"
);

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn inputs() -> (Vec<Item>, ComposeScheduleRequest) {
    let fixture: Value = serde_json::from_slice(FIXTURE).unwrap();
    let leaf: Item =
        serde_json::from_value(fixture["request"]["canonical_items"][0].clone()).unwrap();
    let mut root = leaf.clone();
    root.kind = ItemKind::Routine;
    root.title = "Synthetic recurring parent".into();
    root.recurrence = Some(json!({"type":"daily","times_per_day":1}));
    root.duration_kind = DurationKind::Unknown;
    root.duration_seconds = None;
    root.duration_min_seconds = None;
    root.duration_max_seconds = None;
    root.duration_source = None;
    root.is_executable = false;
    let mut required = leaf.clone();
    required.id = id(2);
    required.parent_id = Some(root.id);
    let mut optional = leaf;
    optional.id = id(3);
    optional.parent_id = Some(root.id);
    optional.title = "Still actionable optional step".into();
    let mut request: ComposeScheduleRequest =
        serde_json::from_value(fixture["request"]["schedule"].clone()).unwrap();
    request.horizon_end += Duration::days(1);
    let mut next_day = request.availability[0].clone();
    next_day.start += Duration::days(1);
    next_day.end += Duration::days(1);
    request.availability.push(next_day);
    (vec![root, required, optional], request)
}

fn compose(context: OccurrenceLifecycleContext) -> ComposeScheduleResult {
    let (items, request) = inputs();
    let schema = if context.snapshot_revision == 0 {
        SCHEDULER_PUBLICATION_SCHEMA
    } else {
        OCCURRENCE_PUBLICATION_SCHEMA
    };
    compose_items_with_lifecycle_for_schema(
        items,
        request,
        schema,
        Vec::new(),
        AuthoritativePlanningEvidence::default(),
        0,
        Vec::new(),
        context,
    )
    .unwrap()
}

fn context(result: &ComposeScheduleResult) -> OccurrenceLifecycleContext {
    OccurrenceLifecycleContext {
        snapshot_revision: 1,
        instances: result
            .plan
            .occurrences
            .iter()
            .map(|occurrence| OccurrenceLifecycleInstance {
                root_item_id: occurrence.series_item_id,
                occurrence_id: occurrence.id,
                identity: occurrence.identity,
                members: result
                    .planning_request
                    .items
                    .iter()
                    .map(|item| OccurrenceLifecycleMember {
                        item_id: item.id,
                        parent_id: item.parent_id,
                        source_revision: item.revision,
                        status: WorkStatus::NotStarted,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn digest_for(result: &ComposeScheduleResult) -> String {
    request_digest(
        result.publication_schema(),
        "UTC",
        &result.source_item_revisions,
        &result.calendar_projection_stamps,
        &result.planning_evidence.execution,
        result.habit_change_head,
        &result.occurrence_lifecycle,
        &result.planning_request,
    )
    .unwrap()
}

#[test]
fn empty_ledger_preserves_exact_legacy_v5_digest_and_publication_proof() {
    #[derive(Serialize)]
    struct LegacyInput<'a> {
        scheduler_publication_schema: &'a str,
        timezone_name: &'a str,
        source_item_revisions: &'a BTreeMap<Uuid, u64>,
        calendar_projection_stamps: &'a [super::CalendarProjectionStamp],
        execution: &'a ExecutionPlanningContext,
        request: &'a dayweave_core::PlanRequest,
    }
    let result = compose(OccurrenceLifecycleContext::default());
    let bytes = serde_json::to_vec(&LegacyInput {
        scheduler_publication_schema: SCHEDULER_PUBLICATION_SCHEMA,
        timezone_name: "UTC",
        source_item_revisions: &result.source_item_revisions,
        calendar_projection_stamps: &result.calendar_projection_stamps,
        execution: &result.planning_evidence.execution,
        request: &result.planning_request,
    })
    .unwrap();
    assert_eq!(
        result.input_digest,
        format!("sha256:{:x}", Sha256::digest(bytes))
    );
    assert_eq!(result.publication_schema(), SCHEDULER_PUBLICATION_SCHEMA);
    let (_, snapshot) = validate_publishable_compose_result("UTC", &result).unwrap();
    assert_eq!(snapshot["schema_version"], 5);
    assert_eq!(
        snapshot["scheduler_publication_schema"],
        SCHEDULER_PUBLICATION_SCHEMA
    );
    assert!(snapshot["compose"].get("occurrence_lifecycle").is_none());
}

#[test]
fn nonzero_empty_horizon_ledger_selects_v6_and_binds_head_without_changing_plan() {
    let legacy = compose(OccurrenceLifecycleContext::default());
    let result = compose(OccurrenceLifecycleContext {
        snapshot_revision: 9,
        instances: Vec::new(),
    });
    assert_eq!(result.publication_schema(), OCCURRENCE_PUBLICATION_SCHEMA);
    assert_eq!(*result.plan, *legacy.plan);
    assert_ne!(result.input_digest, legacy.input_digest);
    assert_ne!(
        publication_content_hash("UTC", &result).unwrap(),
        publication_content_hash("UTC", &legacy).unwrap()
    );
    let (_, snapshot) = validate_publishable_compose_result("UTC", &result).unwrap();
    assert_eq!(snapshot["schema_version"], 6);
    assert_eq!(
        snapshot["evidence"]["occurrence_lifecycle"]["snapshot_revision"],
        9
    );
    assert_eq!(
        snapshot["evidence"]["occurrence_lifecycle"]["instances"],
        json!([])
    );
    assert!(snapshot["compose"].get("occurrence_lifecycle").is_none());
}

#[test]
fn completed_parent_preserves_optional_demand_and_other_occurrence_members() {
    let baseline = compose(OccurrenceLifecycleContext::default());
    let mut lifecycle = context(&baseline);
    assert_eq!(lifecycle.instances.len(), 2);
    let first = lifecycle.instances[0].occurrence_id;
    let second = lifecycle.instances[1].occurrence_id;
    for member in &mut lifecycle.instances[0].members {
        if member.item_id != ItemId(id(3)) {
            member.status = WorkStatus::Completed;
        }
    }
    let result = compose(lifecycle);
    assert!(result.plan.blocks.iter().any(|block| block.item_id == Some(ItemId(id(3))) && block.occurrence_id == Some(first)));
    assert!(!result.plan.blocks.iter().any(|block| block.item_id == Some(ItemId(id(2))) && block.occurrence_id == Some(first)));
    assert!(
        result.plan.blocks.iter().any(
            |block| block.item_id == Some(ItemId(id(2))) && block.occurrence_id == Some(second)
        )
    );
    assert!(
        result
            .plan
            .occurrences
            .iter()
            .all(|occurrence| occurrence.state == OccurrenceState::Generated)
    );
    assert_eq!(result.source_item_revisions, baseline.source_item_revisions);
    assert_eq!(
        result.planning_request.items,
        baseline.planning_request.items
    );
    assert!(validate_publishable_compose_result("UTC", &result).is_ok());
}

#[test]
fn publication_validation_rejects_changed_lifecycle_head_and_even_rehashed_stale_plan() {
    let baseline = compose(OccurrenceLifecycleContext::default());
    let mut lifecycle = context(&baseline);
    lifecycle.instances[0]
        .members
        .iter_mut()
        .find(|member| member.item_id == ItemId(id(2)))
        .unwrap()
        .status = WorkStatus::Completed;
    let result = compose(lifecycle);
    let mut changed = result.clone();
    changed.occurrence_lifecycle.snapshot_revision += 1;
    assert!(validate_publishable_compose_result("UTC", &changed).is_err());
    changed.input_digest = digest_for(&changed);
    assert!(validate_publishable_compose_result("UTC", &changed).is_ok());
    changed.occurrence_lifecycle.instances[0]
        .members
        .iter_mut()
        .find(|member| member.item_id == ItemId(id(2)))
        .unwrap()
        .status = WorkStatus::NotStarted;
    changed.input_digest = digest_for(&changed);
    assert!(validate_publishable_compose_result("UTC", &changed).is_err());
    let mut downgraded = result;
    downgraded.occurrence_lifecycle.snapshot_revision = 0;
    downgraded.input_digest = digest_for(&downgraded);
    assert!(validate_publishable_compose_result("UTC", &downgraded).is_err());
}

fn planning_evidence() -> RoutineOccurrencePlanningEvidence {
    let baseline = compose(OccurrenceLifecycleContext::default());
    let occurrence = &baseline.plan.occurrences[0];
    let (items, request) = inputs();
    let open = ItemCompletionReopenState {
        status: ItemStatus::Planned,
        blocked_reason_kind: None,
        blocked_by_item_id: None,
        blocked_reason: None,
    };
    let manifest = RoutineOccurrenceManifest {
        schema_version: 1,
        id: id(100),
        series_item_id: occurrence.series_item_id.0,
        occurrence_id: occurrence.id.0,
        identity: occurrence.identity,
        nominal_start: chrono::DateTime::from_timestamp_micros(
            i64::try_from(occurrence.nominal_start.unix_timestamp_nanos() / 1_000).unwrap(),
        )
        .unwrap(),
        nominal_end: chrono::DateTime::from_timestamp_micros(
            i64::try_from(occurrence.nominal_end.unix_timestamp_nanos() / 1_000).unwrap(),
        )
        .unwrap(),
        window_start: chrono::DateTime::from_timestamp_micros(
            i64::try_from(occurrence.window_start.unix_timestamp_nanos() / 1_000).unwrap(),
        )
        .unwrap(),
        window_end: chrono::DateTime::from_timestamp_micros(
            i64::try_from(occurrence.window_end.unix_timestamp_nanos() / 1_000).unwrap(),
        )
        .unwrap(),
        timezone_name: "UTC".into(),
        definition_hash: format!("sha256:{}", "a".repeat(64)),
        members: items
            .iter()
            .map(|item| RoutineOccurrenceMemberDefinition {
                item_id: item.id,
                parent_id: item.parent_id,
                source_revision: item.revision,
                title: item.title.clone(),
                kind: item.kind,
                recurs: item.recurrence.is_some(),
                sibling_order: item.sibling_order,
                required_for_parent: item.id != id(3),
                initial_open: open.clone(),
            })
            .collect(),
    };
    RoutineOccurrencePlanningEvidence {
        change_head: 11,
        instances: vec![RoutineOccurrencePlanningInstance {
            aggregate: initialize_routine_occurrence(manifest, request.as_of).unwrap(),
            current_source_revisions: items
                .iter()
                .map(|item| (item.id, item.revision + 1))
                .collect(),
        }],
    }
}

#[test]
fn repository_translation_preserves_complete_membership_and_current_not_first_revisions() {
    let evidence = planning_evidence();
    let original = evidence.instances[0].aggregate.clone();
    let context = lifecycle_from_evidence(evidence.clone()).unwrap();
    assert_eq!(context.snapshot_revision, 11);
    assert_eq!(context.instances.len(), 1);
    assert_eq!(context.instances[0].members.len(), 3);
    assert!(
        context.instances[0]
            .members
            .iter()
            .all(|member| member.source_revision == 2)
    );
    assert_eq!(evidence.instances[0].aggregate, original);
    let mut reordered = evidence.clone();
    reordered.instances[0].aggregate.manifest.members.reverse();
    reordered.instances[0].aggregate.members.reverse();
    assert_eq!(lifecycle_from_evidence(reordered).unwrap(), context);
    let mut missing = evidence;
    missing.instances[0].current_source_revisions.remove(&id(3));
    assert_eq!(
        lifecycle_from_evidence(missing),
        Err(RoutineOccurrenceError::SourceIneligible)
    );
}

#[test]
fn planning_selector_preserves_exact_generated_routine_and_recurring_task_identity() {
    let baseline = compose(OccurrenceLifecycleContext::default());
    let expected: Vec<_> = baseline
        .plan
        .occurrences
        .iter()
        .map(|occurrence| (occurrence.series_item_id.0, occurrence.id.0))
        .collect();
    assert_eq!(
        planning_identities(&baseline.planning_request).unwrap(),
        expected
    );
    let mut task = baseline.planning_request.clone();
    task.items.retain(|item| item.id == ItemId(id(1)));
    task.items[0].kind = dayweave_core::ItemKind::RecurringTask(dayweave_core::RecurringTaskSpec {
        recurrence: dayweave_core::Recurrence::Daily { times_per_day: 1 },
    });
    task.items[0].duration = Some(dayweave_core::DurationEstimate::exact(30));
    assert_eq!(planning_identities(&task).unwrap(), expected);
}
