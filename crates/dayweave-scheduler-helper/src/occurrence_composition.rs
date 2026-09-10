//! Versioned local composition with caller-qualified, per-instance lifecycle.
//! This validates joined data, not the source's authenticity or freshness.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

use dayweave_compose::{
    CanonicalItem, ComposeScheduleRequest, MAX_CANONICAL_ITEMS, PreparedSchedule,
    prepare_canonical_schedule,
};
use dayweave_core::{
    ExecutionPlanningContext, OccurrenceLifecycleContext, OccurrenceLifecycleError, Scheduler,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::{
    CompositionOutput, DigestWriter, ErrorCode, LOCAL_FINGERPRINT_PREFIX, ResponseResult,
    map_preflight_error, map_prepare_error, map_schedule_error, preflight_plan_request,
    wire::PlanOutput,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    canonical_items: Vec<CanonicalItem>,
    schedule: ComposeScheduleRequest,
    occurrence_lifecycle: OccurrenceLifecycleContext,
}

pub(super) fn process_composition(value: &serde_json::Value) -> Result<ResponseResult, ErrorCode> {
    let mut request = Request::deserialize(value).map_err(|_| ErrorCode::InvalidRequest)?;
    if request.canonical_items.len() > MAX_CANONICAL_ITEMS {
        return Err(ErrorCode::ResourceLimitExceeded);
    }
    // Unlike the permissive core Option field, this versioned bridge requires
    // the root's explicit null parent as well as every descendant's exact link.
    if value["occurrence_lifecycle"]["instances"]
        .as_array()
        .is_none_or(|instances| {
            instances.iter().any(|instance| {
                instance["members"].as_array().is_none_or(|members| {
                    members
                        .iter()
                        .any(|member| member.get("parent_id").is_none())
                })
            })
        })
    {
        return Err(ErrorCode::InvalidRequest);
    }
    request
        .occurrence_lifecycle
        .validate()
        .map_err(|error| match error {
            OccurrenceLifecycleError::TooLarge { .. } => ErrorCode::ResourceLimitExceeded,
            _ => ErrorCode::InvalidRequest,
        })?;
    let sources = request
        .canonical_items
        .iter()
        .map(|item| {
            (
                item.id,
                (item.parent_id, item.revision, item.deleted_at.is_some()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut prepared = catch_unwind(AssertUnwindSafe(|| {
        prepare_canonical_schedule(request.canonical_items, request.schedule)
    }))
    .map_err(|_| ErrorCode::InternalFailure)?
    .map_err(|error| map_prepare_error(&error))?;
    validate_source_membership(&request.occurrence_lifecycle, &sources)?;
    let context = &mut request.occurrence_lifecycle;
    context
        .instances
        .sort_by_key(|instance| (instance.root_item_id, instance.occurrence_id));
    for instance in &mut context.instances {
        instance.members.sort_by_key(|member| member.item_id);
    }
    let managed = context
        .instances
        .iter()
        .map(|instance| instance.occurrence_id)
        .collect::<BTreeSet<_>>();
    prepared
        .plan_request
        .recurrence_context
        .completed_occurrence_ids
        .retain(|id| !managed.contains(id));
    prepared
        .plan_request
        .recurrence_context
        .partial_progress
        .retain(|id, _| !managed.contains(id));
    preflight_plan_request(&prepared.plan_request).map_err(map_preflight_error)?;
    let plan = catch_unwind(AssertUnwindSafe(|| {
        Scheduler.plan_with_lifecycle(
            &prepared.plan_request,
            &ExecutionPlanningContext::default(),
            context,
        )
    }))
    .map_err(|_| ErrorCode::InternalFailure)?
    .map_err(|error| map_schedule_error(&error))?;
    let local_input_fingerprint = fingerprint(&prepared, context)?;
    Ok(ResponseResult::Composition {
        composition: CompositionOutput {
            local_input_fingerprint,
            source_item_count: prepared.source_item_count,
            source_item_revisions: prepared.source_item_revisions,
            accepted_item_count: prepared.accepted_item_count,
            rejected_items: prepared.rejected_items,
            ignored_previous_assignments: prepared.ignored_previous_assignments,
            plan: PlanOutput::try_from(plan).map_err(|_| ErrorCode::InternalFailure)?,
            occurrence_snapshot_revision: Some(context.snapshot_revision),
        },
    })
}

/// Check even Inbox/rejected members against the complete canonical snapshot.
/// Matching only prepared `WorkItems` would lose those source-revision fences.
fn validate_source_membership(
    context: &OccurrenceLifecycleContext,
    sources: &BTreeMap<Uuid, (Option<Uuid>, u64, bool)>,
) -> Result<(), ErrorCode> {
    let mut source_children = BTreeMap::<Uuid, usize>::new();
    for (parent, _, deleted) in sources.values() {
        if !deleted && let Some(parent) = parent {
            *source_children.entry(*parent).or_default() += 1;
        }
    }
    for instance in &context.instances {
        let mut member_children = BTreeMap::<Uuid, usize>::new();
        for member in &instance.members {
            if let Some(parent) = member.parent_id {
                *member_children.entry(parent.0).or_default() += 1;
            }
        }
        for member in &instance.members {
            let Some((parent, revision, deleted)) = sources.get(&member.item_id.0) else {
                return Err(ErrorCode::InvalidRequest);
            };
            let expected_parent = (member.item_id != instance.root_item_id)
                .then_some(*parent)
                .flatten();
            if *deleted
                || *revision != member.source_revision
                || expected_parent != member.parent_id.map(|id| id.0)
                || source_children
                    .get(&member.item_id.0)
                    .copied()
                    .unwrap_or_default()
                    != member_children
                        .get(&member.item_id.0)
                        .copied()
                        .unwrap_or_default()
            {
                return Err(ErrorCode::InvalidRequest);
            }
        }
    }
    Ok(())
}

fn fingerprint(
    prepared: &PreparedSchedule,
    context: &OccurrenceLifecycleContext,
) -> Result<String, ErrorCode> {
    #[derive(Serialize)]
    struct Input<'a> {
        domain: &'static str,
        timezone_name: &'a str,
        source_item_revisions: &'a BTreeMap<Uuid, u64>,
        plan_request: &'a dayweave_core::PlanRequest,
        occurrence_lifecycle: &'a OccurrenceLifecycleContext,
    }
    let mut hasher = Sha256::new();
    serde_json::to_writer(
        DigestWriter(&mut hasher),
        &Input {
            domain: "dayweave.scheduler-helper.local-composition.v2",
            timezone_name: &prepared.timezone_name,
            source_item_revisions: &prepared.source_item_revisions,
            plan_request: &prepared.plan_request,
            occurrence_lifecycle: context,
        },
    )
    .map_err(|_| ErrorCode::InternalFailure)?;
    let mut result = String::from(LOCAL_FINGERPRINT_PREFIX);
    for byte in hasher.finalize() {
        write!(&mut result, "{byte:02x}").map_err(|_| ErrorCode::InternalFailure)?;
    }
    Ok(result)
}
