//! Private translation of durable occurrence authority into scheduler evidence.
use std::collections::BTreeSet;

use dayweave_core::{
    ItemId, OccurrenceId, OccurrenceLifecycleContext, OccurrenceLifecycleInstance,
    OccurrenceLifecycleMember, OccurrenceState, PlanRequest, WorkStatus, expand_occurrences,
};
use dayweave_scheduler_helper::preflight_plan_request;
use uuid::Uuid;

use crate::{
    items::ItemStatus, persistence::RoutineOccurrencePlanningEvidence,
    routine_occurrences::RoutineOccurrenceError,
};

/// Queries only exact generated instances, including explicitly moved windows.
/// Caller whole-completion claims are removed for ownership discovery, not
/// treated as authoritative member outcomes. Habit authority stays separate.
pub(super) fn planning_identities(
    request: &PlanRequest,
) -> Result<Vec<(Uuid, Uuid)>, RoutineOccurrenceError> {
    preflight_plan_request(request).map_err(|_| RoutineOccurrenceError::TooLarge)?;
    let roots = request
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                dayweave_core::ItemKind::RecurringTask(_) | dayweave_core::ItemKind::Routine(_)
            )
        })
        .map(|item| item.id)
        .collect::<BTreeSet<_>>();
    Ok(expand_occurrences(request)
        .map_err(|_| RoutineOccurrenceError::Invalid)?
        .into_iter()
        .filter(|occurrence| {
            roots.contains(&occurrence.series_item_id)
                && occurrence.state == OccurrenceState::Generated
        })
        .map(|occurrence| (occurrence.series_item_id.0, occurrence.id.0))
        .collect())
}

pub(super) fn lifecycle_from_evidence(
    evidence: RoutineOccurrencePlanningEvidence,
) -> Result<OccurrenceLifecycleContext, RoutineOccurrenceError> {
    let mut instances = Vec::with_capacity(evidence.instances.len());
    for instance in evidence.instances {
        instance.aggregate.validate()?;
        let aggregate = instance.aggregate;
        let outcomes = aggregate
            .members
            .iter()
            .map(|member| (member.item_id, member.status))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut members = Vec::with_capacity(aggregate.manifest.members.len());
        for definition in &aggregate.manifest.members {
            let source_revision = *instance
                .current_source_revisions
                .get(&definition.item_id)
                .ok_or(RoutineOccurrenceError::SourceIneligible)?;
            let status = match outcomes.get(&definition.item_id) {
                Some(ItemStatus::Inbox | ItemStatus::Planned) => WorkStatus::NotStarted,
                Some(ItemStatus::Blocked) => WorkStatus::Blocked,
                Some(ItemStatus::Completed) => WorkStatus::Completed,
                Some(ItemStatus::Skipped) => WorkStatus::Skipped,
                Some(ItemStatus::Cancelled) => WorkStatus::Canceled,
                _ => return Err(RoutineOccurrenceError::Invalid),
            };
            members.push(OccurrenceLifecycleMember {
                item_id: ItemId(definition.item_id),
                parent_id: definition.parent_id.map(ItemId),
                source_revision,
                status,
            });
        }
        members.sort_by_key(|member| member.item_id);
        instances.push(OccurrenceLifecycleInstance {
            root_item_id: ItemId(aggregate.manifest.series_item_id),
            occurrence_id: OccurrenceId(aggregate.manifest.occurrence_id),
            identity: aggregate.manifest.identity,
            members,
        });
    }
    instances.sort_by_key(|instance| (instance.root_item_id, instance.occurrence_id));
    let context = OccurrenceLifecycleContext {
        snapshot_revision: evidence.change_head,
        instances,
    };
    context
        .validate()
        .map_err(|_| RoutineOccurrenceError::Invalid)?;
    Ok(context)
}
