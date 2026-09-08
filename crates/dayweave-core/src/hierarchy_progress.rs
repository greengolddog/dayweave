//! Read-only summaries of complete canonical forests, independent of planning.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ItemId;

/// Portable integer bound shared with native clients' signed 64-bit storage.
pub const MAX_HIERARCHY_PROGRESS_VALUE: u64 = i64::MAX as u64;

/// Canonical kinds, before recurring templates are expanded for scheduling.
/// `Event` corresponds to the scheduler's `ItemKind::CalendarEvent`, never to
/// flexible effort, including when an event has children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HierarchyProgressKind {
    Task,
    Project,
    Goal,
    Routine,
    Habit,
    Break,
    Event,
}

/// Recorded canonical lifecycle, not inferred execution or occurrence progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HierarchyProgressStatus {
    Inbox,
    Planned,
    Scheduled,
    InProgress,
    Paused,
    Completed,
    Skipped,
    Cancelled,
    Blocked,
}

/// Original recorded estimate in exact seconds. No remaining-work override,
/// duration rounding, event interval, or actual execution time belongs here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchyProgressEstimate {
    pub minimum_seconds: u64,
    pub expected_seconds: u64,
    pub maximum_seconds: u64,
}

/// Minimal normalized source for canonical roll-ups.
///
/// Callers must provide the entire admitted active forest, excluding trashed
/// records. A pruned plan, search result, or collapsed view is not a valid
/// source. Authority, pending-mutation and privacy admission remain caller
/// responsibilities; numerical availability does not establish those proofs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchyProgressItem {
    pub id: ItemId,
    pub parent_id: Option<ItemId>,
    pub kind: HierarchyProgressKind,
    pub status: HierarchyProgressStatus,
    pub has_own_effort: bool,
    /// Whether this canonical node itself carries recurrence. Recurrence is
    /// inherited through every ancestor, regardless of that ancestor's kind.
    pub recurs: bool,
    pub has_children_outside_plan: bool,
    pub duration: Option<HierarchyProgressEstimate>,
}

/// Counts of recorded leaf items and flexible leaf effort estimates.
///
/// This is neither weighted progress, remaining/elapsed work, required-child
/// completion, nor authorization to change any lifecycle. Known estimate sums
/// are incomplete whenever `unknown_estimates` is nonzero. Recurring leaves
/// are excluded from every one-off lifecycle and effort total.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchyProgressSummary {
    pub completed_leaf_items: u64,
    pub skipped_leaf_items: u64,
    pub cancelled_leaf_items: u64,
    pub open_leaf_items: u64,
    pub recurring_leaf_items: u64,
    pub fixed_events: u64,
    pub unknown_estimates: u64,
    pub minimum_estimate_seconds: u64,
    pub expected_estimate_seconds: u64,
    pub maximum_estimate_seconds: u64,
}

impl HierarchyProgressSummary {
    fn checked_add(&mut self, other: Self) -> Option<()> {
        macro_rules! add {
            ($($field:ident),+ $(,)?) => {$({
                self.$field = self.$field.checked_add(other.$field)?;
                if self.$field > MAX_HIERARCHY_PROGRESS_VALUE {
                    return None;
                }
            })+};
        }
        add!(
            completed_leaf_items,
            skipped_leaf_items,
            cancelled_leaf_items,
            open_leaf_items,
            recurring_leaf_items,
            fixed_events,
            unknown_estimates,
            minimum_estimate_seconds,
            expected_estimate_seconds,
            maximum_estimate_seconds,
        );
        Some(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HierarchyProgressError {
    #[error("duplicate item id {0}")]
    DuplicateItem(ItemId),
    #[error("item {item} references missing parent {parent}")]
    MissingParent { item: ItemId, parent: ItemId },
    #[error("hierarchy cycle includes item {0}")]
    Cycle(ItemId),
    #[error("item {0} has explicitly incomplete topology")]
    IncompleteTopology(ItemId),
    #[error("hierarchy item identifiers must not be nil")]
    InvalidId,
    #[error("item {0} has an invalid recorded duration estimate")]
    InvalidDuration(ItemId),
    #[error("hierarchy summary for item {0} exceeds the portable integer bound")]
    Overflow(ItemId),
}

impl HierarchyProgressError {
    /// Stable content-free classification for fixture and adapter diagnostics.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DuplicateItem(_) => "duplicate_item",
            Self::MissingParent { .. } => "missing_parent",
            Self::Cycle(_) => "cycle",
            Self::IncompleteTopology(_) => "incomplete_topology",
            Self::InvalidId => "invalid_id",
            Self::InvalidDuration(_) => "invalid_duration",
            Self::Overflow(_) => "overflow",
        }
    }
}

/// Summarizes every subtree without changing recorded lifecycle or scheduling.
///
/// Both topology validation and reduction are iterative. Each structural leaf
/// contributes once, including an empty semantic container; only eligible
/// nonrecurring flexible leaves contribute recorded effort. Nonrecurring fixed
/// events contribute their own event count even when they have children.
///
/// # Errors
///
/// Rejects nil/duplicate IDs, missing parents, cycles, explicitly incomplete
/// topology, malformed estimates, or any count/sum exceeding `i64::MAX`.
/// The entire projection is unavailable on error; partial totals are never
/// returned. An empty admitted forest produces an empty map.
pub fn roll_up_hierarchy_progress(
    items: &[HierarchyProgressItem],
) -> Result<BTreeMap<ItemId, HierarchyProgressSummary>, HierarchyProgressError> {
    let by_id = index_items(items)?;
    let mut remaining_children: BTreeMap<_, usize> = by_id.keys().map(|id| (*id, 0)).collect();
    for item in by_id.values() {
        if let Some(parent) = item.parent_id {
            let count = remaining_children.get_mut(&parent).ok_or(
                HierarchyProgressError::MissingParent {
                    item: item.id,
                    parent,
                },
            )?;
            *count += 1;
        }
    }
    let child_counts = remaining_children.clone();
    let mut ready: BTreeSet<_> = remaining_children
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut postorder = Vec::with_capacity(items.len());
    while let Some(id) = ready.pop_first() {
        postorder.push(id);
        remaining_children.remove(&id);
        if let Some(parent) = by_id[&id].parent_id
            && let Some(count) = remaining_children.get_mut(&parent)
        {
            *count -= 1;
            if *count == 0 {
                ready.insert(parent);
            }
        }
    }
    if let Some((id, _)) = remaining_children.first_key_value() {
        return Err(HierarchyProgressError::Cycle(*id));
    }

    let mut recurring = BTreeMap::new();
    let mut summaries = BTreeMap::new();
    // Reverse reduction order ensures every ancestor is available before its
    // descendants, without rescanning ancestry once per node.
    for id in postorder.iter().rev() {
        let item = by_id[id];
        let is_recurring = item.recurs || item.parent_id.is_some_and(|parent| recurring[&parent]);
        recurring.insert(*id, is_recurring);
        summaries.insert(*id, own_summary(item, child_counts[id] == 0, is_recurring));
    }
    for id in postorder {
        if let Some(parent) = by_id[&id].parent_id {
            let child = summaries[&id];
            if summaries
                .get_mut(&parent)
                .and_then(|value| value.checked_add(child))
                .is_none()
            {
                return Err(HierarchyProgressError::Overflow(parent));
            }
        }
    }
    Ok(summaries)
}

fn index_items(
    items: &[HierarchyProgressItem],
) -> Result<BTreeMap<ItemId, &HierarchyProgressItem>, HierarchyProgressError> {
    let mut by_id = BTreeMap::new();
    for item in items {
        if item.id.0.is_nil() || item.parent_id.is_some_and(|id| id.0.is_nil()) {
            return Err(HierarchyProgressError::InvalidId);
        }
        if by_id.insert(item.id, item).is_some() {
            return Err(HierarchyProgressError::DuplicateItem(item.id));
        }
        if item.has_children_outside_plan {
            return Err(HierarchyProgressError::IncompleteTopology(item.id));
        }
        if item.duration.is_some_and(|duration| {
            duration.minimum_seconds == 0
                || duration.minimum_seconds > duration.expected_seconds
                || duration.expected_seconds > duration.maximum_seconds
                || duration.maximum_seconds > MAX_HIERARCHY_PROGRESS_VALUE
        }) {
            return Err(HierarchyProgressError::InvalidDuration(item.id));
        }
    }
    Ok(by_id)
}

fn own_summary(
    item: &HierarchyProgressItem,
    is_leaf: bool,
    is_recurring: bool,
) -> HierarchyProgressSummary {
    let mut summary = HierarchyProgressSummary::default();
    if is_recurring {
        summary.recurring_leaf_items = u64::from(is_leaf);
        return summary;
    }
    summary.fixed_events = u64::from(item.kind == HierarchyProgressKind::Event);
    if !is_leaf {
        return summary;
    }
    match item.status {
        HierarchyProgressStatus::Completed => summary.completed_leaf_items = 1,
        HierarchyProgressStatus::Skipped => summary.skipped_leaf_items = 1,
        HierarchyProgressStatus::Cancelled => summary.cancelled_leaf_items = 1,
        _ => summary.open_leaf_items = 1,
    }
    let has_effort = match item.kind {
        HierarchyProgressKind::Event => false,
        HierarchyProgressKind::Project
        | HierarchyProgressKind::Goal
        | HierarchyProgressKind::Routine => item.has_own_effort,
        _ => true,
    };
    if has_effort {
        if let Some(duration) = item.duration {
            summary.minimum_estimate_seconds = duration.minimum_seconds;
            summary.expected_estimate_seconds = duration.expected_seconds;
            summary.maximum_estimate_seconds = duration.maximum_seconds;
        } else {
            summary.unknown_estimates = 1;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_summary_field_rejects_portable_overflow() {
        macro_rules! check {
            ($($field:ident),+ $(,)?) => {$({
                let mut summary = HierarchyProgressSummary { $field: MAX_HIERARCHY_PROGRESS_VALUE, ..HierarchyProgressSummary::default() };
                let increment = HierarchyProgressSummary { $field: 1, ..HierarchyProgressSummary::default() };
                assert_eq!(summary.checked_add(increment), None, stringify!($field));
            })+};
        }
        check!(
            completed_leaf_items,
            skipped_leaf_items,
            cancelled_leaf_items,
            open_leaf_items,
            recurring_leaf_items,
            fixed_events,
            unknown_estimates,
            minimum_estimate_seconds,
            expected_estimate_seconds,
            maximum_estimate_seconds,
        );
    }
}
