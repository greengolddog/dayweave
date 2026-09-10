//! Caller-authenticated lifecycle evidence for individual recurrence members.
//!
//! This is separate from the public planning request. It neither authenticates
//! evidence nor infers completion: callers supply a complete occurrence tree
//! and its already reconciled member statuses. Template lifecycle is not proof.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ExecutionDisposition, ExecutionPlanningContext, ItemId, MAX_RECURRENCE_MATERIALIZED_ITEMS,
    MaterializedPlan, OccurrenceId, OccurrenceState, PlanRequest, RecurrenceOccurrenceIdentity,
    WorkStatus,
};

/// The entire supplied horizon context shares the materialization item ceiling.
pub const MAX_OCCURRENCE_LIFECYCLE_MEMBERS: usize = MAX_RECURRENCE_MATERIALIZED_ITEMS;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OccurrenceLifecycleContext {
    /// Monotonic authoritative ledger head. Zero is valid only without instances;
    /// a nonzero head may have no instances in this particular horizon.
    pub snapshot_revision: u64,
    pub instances: Vec<OccurrenceLifecycleInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OccurrenceLifecycleInstance {
    pub root_item_id: ItemId,
    pub occurrence_id: OccurrenceId,
    pub identity: RecurrenceOccurrenceIdentity,
    pub members: Vec<OccurrenceLifecycleMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OccurrenceLifecycleMember {
    pub item_id: ItemId,
    /// The occurrence root has no parent within this tree. All other links are
    /// exact canonical member links, including members omitted from planning.
    pub parent_id: Option<ItemId>,
    /// Current admitted source revision, not an immutable first-seen revision.
    pub source_revision: u64,
    /// Active/Paused may only come from execution authority, not this overlay.
    pub status: WorkStatus,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OccurrenceLifecycleError {
    #[error("occurrence lifecycle snapshot revision is invalid")]
    InvalidSnapshotRevision,
    #[error("occurrence lifecycle exceeds the {limit}-member limit")]
    TooLarge { limit: usize },
    #[error("occurrence lifecycle instance {0} has invalid identifiers or no members")]
    InvalidInstance(OccurrenceId),
    #[error("occurrence lifecycle repeats instance {0}")]
    DuplicateInstance(OccurrenceId),
    #[error("occurrence {occurrence_id} has invalid member {item_id}")]
    InvalidMember {
        occurrence_id: OccurrenceId,
        item_id: ItemId,
    },
    #[error("occurrence {occurrence_id} repeats member {item_id}")]
    DuplicateMember {
        occurrence_id: OccurrenceId,
        item_id: ItemId,
    },
    #[error("occurrence {0} must contain exactly one complete acyclic root tree")]
    InvalidHierarchy(OccurrenceId),
    #[error("occurrence {0} does not match an exact generated instance in this horizon")]
    OccurrenceMismatch(OccurrenceId),
    #[error("occurrence {occurrence_id} does not match source member {item_id}")]
    SourceMismatch {
        occurrence_id: OccurrenceId,
        item_id: ItemId,
    },
    #[error("occurrence {occurrence_id} is missing materialized member {item_id}")]
    MissingMember {
        occurrence_id: OccurrenceId,
        item_id: ItemId,
    },
    #[error("occurrence {occurrence_id} conflicts with execution for member {item_id}")]
    ExecutionConflict {
        occurrence_id: OccurrenceId,
        item_id: ItemId,
    },
}

struct IndexedInstance<'a> {
    instance: &'a OccurrenceLifecycleInstance,
    members: BTreeMap<ItemId, &'a OccurrenceLifecycleMember>,
}

impl OccurrenceLifecycleContext {
    /// Validates the bounded, complete member trees without accepting them as
    /// scheduling proof. Planning additionally checks generated identity,
    /// present source revisions/membership and execution consistency.
    ///
    /// # Errors
    /// Rejects invalid revisions, IDs, duplicate instances/members, unsupported
    /// lifecycle, missing parents, cycles and contexts above the shared budget.
    pub fn validate(&self) -> Result<(), OccurrenceLifecycleError> {
        index_context(self).map(|_| ())
    }
}

fn index_context(
    context: &OccurrenceLifecycleContext,
) -> Result<BTreeMap<OccurrenceId, IndexedInstance<'_>>, OccurrenceLifecycleError> {
    if i64::try_from(context.snapshot_revision).is_err()
        || (context.snapshot_revision == 0 && !context.instances.is_empty())
    {
        return Err(OccurrenceLifecycleError::InvalidSnapshotRevision);
    }
    let too_large = || OccurrenceLifecycleError::TooLarge {
        limit: MAX_OCCURRENCE_LIFECYCLE_MEMBERS,
    };
    if context.instances.len() > MAX_OCCURRENCE_LIFECYCLE_MEMBERS {
        return Err(too_large());
    }
    context
        .instances
        .iter()
        .try_fold(0_usize, |total, instance| {
            total
                .checked_add(instance.members.len())
                .filter(|total| *total <= MAX_OCCURRENCE_LIFECYCLE_MEMBERS)
                .ok_or_else(too_large)
        })?;
    let mut indexed = BTreeMap::new();
    for instance in &context.instances {
        if instance.root_item_id.0.is_nil()
            || instance.occurrence_id.0.is_nil()
            || instance.members.is_empty()
        {
            return Err(OccurrenceLifecycleError::InvalidInstance(
                instance.occurrence_id,
            ));
        }
        let members = index_members(instance)?;
        if indexed
            .insert(
                instance.occurrence_id,
                IndexedInstance { instance, members },
            )
            .is_some()
        {
            return Err(OccurrenceLifecycleError::DuplicateInstance(
                instance.occurrence_id,
            ));
        }
    }
    Ok(indexed)
}

fn index_members(
    instance: &OccurrenceLifecycleInstance,
) -> Result<BTreeMap<ItemId, &OccurrenceLifecycleMember>, OccurrenceLifecycleError> {
    let mut members = BTreeMap::new();
    for member in &instance.members {
        if member.item_id.0.is_nil()
            || member.parent_id.is_some_and(|id| id.0.is_nil())
            || member.source_revision == 0
            || i64::try_from(member.source_revision).is_err()
            || matches!(member.status, WorkStatus::Active | WorkStatus::Paused)
        {
            return Err(OccurrenceLifecycleError::InvalidMember {
                occurrence_id: instance.occurrence_id,
                item_id: member.item_id,
            });
        }
        if members.insert(member.item_id, member).is_some() {
            return Err(OccurrenceLifecycleError::DuplicateMember {
                occurrence_id: instance.occurrence_id,
                item_id: member.item_id,
            });
        }
    }
    validate_member_tree(instance, &members)?;
    Ok(members)
}

fn validate_member_tree(
    instance: &OccurrenceLifecycleInstance,
    members: &BTreeMap<ItemId, &OccurrenceLifecycleMember>,
) -> Result<(), OccurrenceLifecycleError> {
    let invalid = || OccurrenceLifecycleError::InvalidHierarchy(instance.occurrence_id);
    if members
        .get(&instance.root_item_id)
        .is_none_or(|root| root.parent_id.is_some())
    {
        return Err(invalid());
    }
    let mut children: BTreeMap<_, usize> = members.keys().map(|id| (*id, 0)).collect();
    for member in members.values() {
        if let Some(parent) = member.parent_id {
            let count = children.get_mut(&parent).ok_or_else(invalid)?;
            *count += 1;
        } else if member.item_id != instance.root_item_id {
            return Err(invalid());
        }
    }
    let mut ready = children
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect::<Vec<_>>();
    while let Some(id) = ready.pop() {
        children.remove(&id);
        if let Some(parent) = members[&id].parent_id {
            let count = children.get_mut(&parent).ok_or_else(invalid)?;
            *count -= 1;
            if *count == 0 {
                ready.push(parent);
            }
        }
    }
    if !children.is_empty() {
        return Err(invalid());
    }
    Ok(())
}

/// Only a generated, exact in-horizon instance is admissible. Out-of-horizon,
/// completed, skipped and paused envelopes are rejected, not silently ignored.
/// Validate the whole overlay before changing any private materialized clone.
#[allow(clippy::too_many_lines)] // Validate the joined evidence completely before projecting any clone.
pub(crate) fn apply_occurrence_lifecycle(
    source: &PlanRequest,
    execution: &ExecutionPlanningContext,
    context: &OccurrenceLifecycleContext,
    materialized: &mut MaterializedPlan,
) -> Result<(), OccurrenceLifecycleError> {
    let indexed = index_context(context)?;
    if indexed.is_empty() {
        return Ok(());
    }
    let source_items: BTreeMap<_, _> = source.items.iter().map(|item| (item.id, item)).collect();
    let occurrences: BTreeMap<_, _> = materialized
        .occurrences
        .iter()
        .map(|item| (item.id, item))
        .collect();
    let mut cloned_members = BTreeMap::<OccurrenceId, BTreeMap<ItemId, ItemId>>::new();
    for (clone_id, identity) in &materialized.identities {
        cloned_members
            .entry(identity.occurrence_id)
            .or_default()
            .insert(identity.series_item_id, *clone_id);
    }
    let execution: BTreeMap<_, _> = execution
        .work_units
        .iter()
        .map(|unit| ((unit.item_id, unit.occurrence_id), unit))
        .collect();
    let mut statuses = BTreeMap::new();
    for (occurrence_id, indexed) in indexed {
        let instance = indexed.instance;
        if occurrences.get(&occurrence_id).is_none_or(|occurrence| {
            occurrence.series_item_id != instance.root_item_id
                || occurrence.identity != instance.identity
                || occurrence.state != OccurrenceState::Generated
        }) {
            return Err(OccurrenceLifecycleError::OccurrenceMismatch(occurrence_id));
        }
        let clones = cloned_members
            .get(&occurrence_id)
            .ok_or(OccurrenceLifecycleError::OccurrenceMismatch(occurrence_id))?;
        for item_id in clones.keys() {
            if !indexed.members.contains_key(item_id) {
                return Err(OccurrenceLifecycleError::MissingMember {
                    occurrence_id,
                    item_id: *item_id,
                });
            }
        }
        let omitted_parents: BTreeSet<_> = indexed
            .members
            .values()
            .filter(|member| !source_items.contains_key(&member.item_id))
            .filter_map(|member| member.parent_id)
            .collect();
        for member in indexed.members.values() {
            let Some(source_item) = source_items.get(&member.item_id) else {
                continue;
            };
            let expected_parent = if member.item_id == instance.root_item_id {
                None
            } else {
                source_item.parent_id
            };
            if source_item.revision != member.source_revision
                || member.parent_id != expected_parent
                || source_item.has_children_outside_plan
                    != omitted_parents.contains(&member.item_id)
                || !clones.contains_key(&member.item_id)
            {
                return Err(OccurrenceLifecycleError::SourceMismatch {
                    occurrence_id,
                    item_id: member.item_id,
                });
            }
            // An occurrence reopening is not an instruction to remove a
            // current template-wide block. Terminal history remains evidence,
            // but open work inherits the stricter of the two blocking states.
            let status =
                if source_item.status == WorkStatus::Blocked && !member.status.is_terminal() {
                    WorkStatus::Blocked
                } else {
                    member.status
                };
            if let Some(unit) = execution.get(&(member.item_id, Some(occurrence_id)))
                && ((!unit.reservations.is_empty()
                    && (status.is_terminal() || status == WorkStatus::Blocked))
                    || (unit.disposition == Some(ExecutionDisposition::Skipped)
                        && member.status != WorkStatus::Skipped))
            {
                return Err(OccurrenceLifecycleError::ExecutionConflict {
                    occurrence_id,
                    item_id: member.item_id,
                });
            }
            statuses.insert(clones[&member.item_id], status);
        }
    }
    for item in &mut materialized.request.items {
        if let Some(status) = statuses.get(&item.id) {
            item.status = *status;
        }
    }
    Ok(())
}
