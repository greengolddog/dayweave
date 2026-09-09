//! Pure completion decisions over complete, caller-admitted canonical trees.
//!
//! This module provides no persistence, authorization, freshness, execution, or
//! occurrence-read proof. Callers must apply decisions through their shared
//! authoritative mutation boundary. Estimates and independent progress values
//! are deliberately absent: neither implies lifecycle completion.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{HierarchyProgressStatus, ItemId, OccurrenceId};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyCompletionOverride {
    #[default]
    Automatic,
    KeepOpen,
    Complete,
}

/// Retained reopening evidence, independent of the currently requested policy.
/// A caller changing `Complete` to `Automatic` or `KeepOpen` must retain this
/// evidence, not discard it before evaluation. Its prior status is nonterminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyCompletionProvenance {
    Automatic {
        prior_open_status: HierarchyProgressStatus,
    },
    Manual {
        prior_open_status: HierarchyProgressStatus,
    },
}

impl HierarchyCompletionProvenance {
    #[must_use]
    pub const fn prior_open_status(self) -> HierarchyProgressStatus {
        match self {
            Self::Automatic { prior_open_status } | Self::Manual { prior_open_status } => {
                prior_open_status
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyCompletionScope {
    OneOff,
    /// Input must be exactly the complete tree rooted at this recurring item.
    /// Every supplied lifecycle is caller-qualified for this occurrence, not
    /// copied from template status. The scope is returned with the decisions;
    /// these decisions must never be written back as template completion.
    /// A nested recurrence remains unproven by this outer occurrence identity.
    Occurrence {
        root_id: ItemId,
        occurrence_id: OccurrenceId,
    },
}

/// A normalized node in the complete active forest, excluding trashed records.
/// `required_for_parent` cuts the entire branch when false. It is explicit;
/// defaulting legacy/new items to required is the caller's responsibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchyCompletionItem {
    pub id: ItemId,
    pub parent_id: Option<ItemId>,
    pub status: HierarchyProgressStatus,
    pub required_for_parent: bool,
    pub recurs: bool,
    pub has_children_outside_plan: bool,
    pub manual_override: HierarchyCompletionOverride,
    /// Present only for a recorded Completed status. Absence preserves an
    /// independently recorded terminal *leaf* under Automatic. An ambiguous
    /// terminal parent needs explicit policy/reopening review; absence cannot
    /// authorize inventing a reopening status for a new manual override.
    pub provenance: Option<HierarchyCompletionProvenance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyCompletionReadiness {
    NoRequiredDescendants,
    RequiredDescendantsIncomplete,
    OccurrenceEvidenceRequired,
    AllRequiredDescendantsCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyCompletionAction {
    Unchanged,
    OccurrenceEvidenceRequired,
    AutomaticallyCompleted,
    AutomaticallyReopened,
    ManuallyCompleted,
    ManuallyKeptOpen,
    ManualCompletionReleased,
}

/// Counts cover every descendant reachable through required edges, not merely
/// direct children or leaves. The three categories partition the total.
/// A manually completed node does not waive an incomplete required descendant.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HierarchyCompletionCounts {
    pub required_descendants: u64,
    pub completed: u64,
    pub incomplete: u64,
    pub occurrence_evidence_required: u64,
}

impl HierarchyCompletionCounts {
    fn add_branch(&mut self, child: &HierarchyCompletionDecision) -> Option<()> {
        let mut branch = child.counts;
        branch.required_descendants = branch.required_descendants.checked_add(1)?;
        let category = if child.occurrence_evidence_required {
            &mut branch.occurrence_evidence_required
        } else if child.status == HierarchyProgressStatus::Completed {
            &mut branch.completed
        } else {
            &mut branch.incomplete
        };
        *category = category.checked_add(1)?;
        self.required_descendants = self
            .required_descendants
            .checked_add(branch.required_descendants)?;
        self.completed = self.completed.checked_add(branch.completed)?;
        self.incomplete = self.incomplete.checked_add(branch.incomplete)?;
        self.occurrence_evidence_required = self
            .occurrence_evidence_required
            .checked_add(branch.occurrence_evidence_required)?;
        i64::try_from(self.required_descendants).ok().map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HierarchyCompletionDecision {
    pub status: HierarchyProgressStatus,
    pub provenance: Option<HierarchyCompletionProvenance>,
    pub action: HierarchyCompletionAction,
    pub readiness: HierarchyCompletionReadiness,
    pub counts: HierarchyCompletionCounts,
    /// This node itself is an unqualified recurring template/descendant.
    /// Readiness also reflects unqualified *required descendants* in counts.
    pub occurrence_evidence_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchyCompletionEvaluation {
    pub scope: HierarchyCompletionScope,
    pub decisions: BTreeMap<ItemId, HierarchyCompletionDecision>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HierarchyCompletionError {
    #[error("hierarchy completion identifiers must not be nil")]
    InvalidId,
    #[error("duplicate hierarchy completion item {0}")]
    DuplicateItem(ItemId),
    #[error("item {item} references missing parent {parent}")]
    MissingParent { item: ItemId, parent: ItemId },
    #[error("hierarchy cycle includes item {0}")]
    Cycle(ItemId),
    #[error("item {0} has explicitly incomplete topology")]
    IncompleteTopology(ItemId),
    #[error("item {0} has invalid completion provenance")]
    InvalidProvenance(ItemId),
    #[error("item {0} needs an explicitly preserved prior open status")]
    MissingPriorOpenStatus(ItemId),
    #[error("terminal parent {0} needs explicit completion policy and reopening review")]
    AmbiguousTerminalParent(ItemId),
    #[error("occurrence scope must identify exactly one complete recurring-root tree")]
    InvalidOccurrenceScope,
    #[error("hierarchy completion counts for item {0} exceed the portable integer bound")]
    Overflow(ItemId),
}

/// Evaluate bottom-up without mutating the input or inferring any work credit.
///
/// Automatic completion requires at least one required descendant and every
/// required descendant Completed after its own decision. Skipped/cancelled
/// nodes remain unmet. Independently recorded terminal leaf statuses are
/// preserved; terminal parents need explicit provenance. Only retained
/// completion provenance authorizes automatic reopening. Unqualified recurring
/// nodes are unchanged, including their provenance and manual overrides.
///
/// # Errors
/// Rejects invalid/duplicate identifiers, incomplete or cyclic topology,
/// missing parents, inconsistent provenance, an unsupported occurrence scope,
/// manual overrides without a known reopening status, or count overflow.
pub fn evaluate_hierarchy_completion(
    items: &[HierarchyCompletionItem],
    scope: HierarchyCompletionScope,
) -> Result<HierarchyCompletionEvaluation, HierarchyCompletionError> {
    let by_id = index_items(items)?;
    let postorder = complete_postorder(&by_id)?;
    let parents: BTreeSet<_> = by_id.values().filter_map(|item| item.parent_id).collect();
    let qualified_root = validate_scope(&by_id, scope)?;
    let mut unproven = BTreeMap::new();
    for id in postorder.iter().rev() {
        let item = by_id[id];
        unproven.insert(
            *id,
            (item.recurs && qualified_root != Some(*id))
                || item.parent_id.is_some_and(|parent| unproven[&parent]),
        );
    }
    let mut counts: BTreeMap<_, _> = by_id
        .keys()
        .map(|id| (*id, HierarchyCompletionCounts::default()))
        .collect();
    let mut decisions = BTreeMap::new();
    for id in postorder {
        let item = by_id[&id];
        let decision = decide(item, counts[&id], unproven[&id], parents.contains(&id))?;
        if item.required_for_parent
            && let Some(parent) = item.parent_id
        {
            counts
                .get_mut(&parent)
                .and_then(|counts| counts.add_branch(&decision))
                .ok_or(HierarchyCompletionError::Overflow(parent))?;
        }
        decisions.insert(id, decision);
    }
    Ok(HierarchyCompletionEvaluation { scope, decisions })
}

fn index_items(
    items: &[HierarchyCompletionItem],
) -> Result<BTreeMap<ItemId, &HierarchyCompletionItem>, HierarchyCompletionError> {
    let mut by_id = BTreeMap::new();
    for item in items {
        if item.id.0.is_nil() || item.parent_id.is_some_and(|parent| parent.0.is_nil()) {
            return Err(HierarchyCompletionError::InvalidId);
        }
        if by_id.insert(item.id, item).is_some() {
            return Err(HierarchyCompletionError::DuplicateItem(item.id));
        }
        if item.has_children_outside_plan {
            return Err(HierarchyCompletionError::IncompleteTopology(item.id));
        }
        if let Some(provenance) = item.provenance
            && (item.status != HierarchyProgressStatus::Completed
                || !is_open(provenance.prior_open_status()))
        {
            return Err(HierarchyCompletionError::InvalidProvenance(item.id));
        }
    }
    Ok(by_id)
}

fn complete_postorder(
    by_id: &BTreeMap<ItemId, &HierarchyCompletionItem>,
) -> Result<Vec<ItemId>, HierarchyCompletionError> {
    let mut remaining: BTreeMap<_, usize> = by_id.keys().map(|id| (*id, 0)).collect();
    for item in by_id.values() {
        if let Some(parent) = item.parent_id {
            *remaining
                .get_mut(&parent)
                .ok_or(HierarchyCompletionError::MissingParent {
                    item: item.id,
                    parent,
                })? += 1;
        }
    }
    let mut ready: BTreeSet<_> = remaining
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut result = Vec::with_capacity(by_id.len());
    while let Some(id) = ready.pop_first() {
        remaining.remove(&id);
        result.push(id);
        if let Some(parent) = by_id[&id].parent_id
            && let Some(count) = remaining.get_mut(&parent)
        {
            *count -= 1;
            if *count == 0 {
                ready.insert(parent);
            }
        }
    }
    if let Some((id, _)) = remaining.first_key_value() {
        return Err(HierarchyCompletionError::Cycle(*id));
    }
    Ok(result)
}

fn validate_scope(
    by_id: &BTreeMap<ItemId, &HierarchyCompletionItem>,
    scope: HierarchyCompletionScope,
) -> Result<Option<ItemId>, HierarchyCompletionError> {
    match scope {
        HierarchyCompletionScope::OneOff => Ok(None),
        HierarchyCompletionScope::Occurrence {
            root_id,
            occurrence_id,
        } => {
            if root_id.0.is_nil() || occurrence_id.0.is_nil() {
                return Err(HierarchyCompletionError::InvalidId);
            }
            if !by_id
                .get(&root_id)
                .is_some_and(|root| root.recurs && root.parent_id.is_none())
                || by_id
                    .values()
                    .any(|item| item.parent_id.is_none() && item.id != root_id)
            {
                return Err(HierarchyCompletionError::InvalidOccurrenceScope);
            }
            Ok(Some(root_id))
        }
    }
}

fn decide(
    item: &HierarchyCompletionItem,
    counts: HierarchyCompletionCounts,
    occurrence_evidence_required: bool,
    has_children: bool,
) -> Result<HierarchyCompletionDecision, HierarchyCompletionError> {
    use HierarchyCompletionAction as Action;
    use HierarchyCompletionOverride as Override;
    use HierarchyCompletionProvenance as Provenance;
    use HierarchyCompletionReadiness as Readiness;
    use HierarchyProgressStatus::Completed;

    let readiness = if occurrence_evidence_required || counts.occurrence_evidence_required != 0 {
        Readiness::OccurrenceEvidenceRequired
    } else if counts.required_descendants == 0 {
        Readiness::NoRequiredDescendants
    } else if counts.incomplete != 0 {
        Readiness::RequiredDescendantsIncomplete
    } else {
        Readiness::AllRequiredDescendantsCompleted
    };
    let prior = item
        .provenance
        .map(HierarchyCompletionProvenance::prior_open_status)
        .or_else(|| is_open(item.status).then_some(item.status));
    let mut decision = HierarchyCompletionDecision {
        status: item.status,
        provenance: item.provenance,
        action: Action::Unchanged,
        readiness,
        counts,
        occurrence_evidence_required,
    };
    if occurrence_evidence_required {
        decision.action = Action::OccurrenceEvidenceRequired;
        return Ok(decision);
    }
    if has_children
        && !is_open(item.status)
        && item.provenance.is_none()
        && item.manual_override == Override::Automatic
    {
        return Err(HierarchyCompletionError::AmbiguousTerminalParent(item.id));
    }
    match item.manual_override {
        Override::Automatic => {
            if readiness == Readiness::AllRequiredDescendantsCompleted {
                if let Some(prior_open_status) = prior {
                    let provenance = Some(Provenance::Automatic { prior_open_status });
                    if decision.status != Completed || decision.provenance != provenance {
                        decision.action = Action::AutomaticallyCompleted;
                    }
                    decision.status = Completed;
                    decision.provenance = provenance;
                }
            } else if let Some(provenance) = item.provenance {
                decision.status = provenance.prior_open_status();
                decision.provenance = None;
                decision.action = match provenance {
                    Provenance::Automatic { .. } => Action::AutomaticallyReopened,
                    Provenance::Manual { .. } => Action::ManualCompletionReleased,
                };
            }
        }
        Override::KeepOpen => {
            decision.status =
                prior.ok_or(HierarchyCompletionError::MissingPriorOpenStatus(item.id))?;
            decision.provenance = None;
            if decision.status != item.status || item.provenance.is_some() {
                decision.action = Action::ManuallyKeptOpen;
            }
        }
        Override::Complete => {
            let prior_open_status =
                prior.ok_or(HierarchyCompletionError::MissingPriorOpenStatus(item.id))?;
            decision.status = Completed;
            decision.provenance = Some(Provenance::Manual { prior_open_status });
            if decision.status != item.status || decision.provenance != item.provenance {
                decision.action = Action::ManuallyCompleted;
            }
        }
    }
    Ok(decision)
}

const fn is_open(status: HierarchyProgressStatus) -> bool {
    matches!(
        status,
        HierarchyProgressStatus::Inbox
            | HierarchyProgressStatus::Planned
            | HierarchyProgressStatus::Scheduled
            | HierarchyProgressStatus::InProgress
            | HierarchyProgressStatus::Paused
            | HierarchyProgressStatus::Blocked
    )
}
