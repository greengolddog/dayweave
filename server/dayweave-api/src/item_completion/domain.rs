//! Server adapter for reviewed completion policy and exact reopening custody.
//! Callers must supply the whole active forest under execution/canonical locks.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use dayweave_core::{
    HierarchyCompletionAction, HierarchyCompletionError, HierarchyCompletionItem,
    HierarchyCompletionOverride, HierarchyCompletionProvenance, HierarchyCompletionScope,
    HierarchyProgressStatus, ItemId, evaluate_hierarchy_completion,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    items::{BlockedReasonKind, Item, ItemStatus},
    scheduling::{has_postgres_timestamp_precision, truncate_to_postgres_timestamp_precision},
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub const MAX_COMPLETION_ITEMS: usize = 20_000;
/// Bound retained canonical content before cloning a complete forest.
pub const MAX_COMPLETION_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemCompletionMode {
    #[default]
    Automatic,
    KeepOpen,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemCompletionProvenanceKind {
    Automatic,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionReopenState {
    pub status: ItemStatus,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub blocked_reason_kind: Option<BlockedReasonKind>,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub blocked_by_item_id: Option<Uuid>,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub blocked_reason: Option<String>,
}

impl ItemCompletionReopenState {
    /// Reopening structural work never fabricates a running/reserved session.
    ///
    /// # Errors
    /// Rejects unknown reopening status or invalid blocker custody.
    pub fn validate(&self, item_id: Uuid) -> Result<(), ItemCompletionError> {
        if !matches!(
            self.status,
            ItemStatus::Inbox | ItemStatus::Planned | ItemStatus::Blocked
        ) {
            return Err(ItemCompletionError::ReopeningReviewRequired);
        }
        if self.blocked_reason.as_ref().is_some_and(|reason| {
            reason.trim() != reason
                || reason.is_empty()
                || reason.chars().count() > 1_000
                || reason.chars().any(char::is_control)
        }) {
            return Err(ItemCompletionError::Invalid);
        }
        let valid = match (
            self.status,
            self.blocked_reason_kind,
            self.blocked_by_item_id,
            &self.blocked_reason,
        ) {
            (ItemStatus::Blocked, Some(BlockedReasonKind::Dependency), Some(id), _) => {
                !id.is_nil() && id != item_id
            }
            (
                ItemStatus::Blocked,
                Some(BlockedReasonKind::Manual | BlockedReasonKind::External),
                None,
                Some(_),
            ) => true,
            (status, None, None, None) => status != ItemStatus::Blocked,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(ItemCompletionError::Invalid)
        }
    }

    /// Captures the exact open lifecycle and blocker tuple.
    ///
    /// # Errors
    /// Rejects terminal or execution-owned reopening status and invalid blockers.
    pub fn from_item(item: &Item) -> Result<Self, ItemCompletionError> {
        let state = Self {
            status: item.status,
            blocked_reason_kind: item.blocked_reason_kind,
            blocked_by_item_id: item.blocked_by_item_id,
            blocked_reason: item.blocked_reason.clone(),
        };
        state.validate(item.id)?;
        Ok(state)
    }

    fn apply_to(&self, item: &mut Item) {
        item.status = self.status;
        item.blocked_reason_kind = self.blocked_reason_kind;
        item.blocked_by_item_id = self.blocked_by_item_id;
        item.blocked_reason.clone_from(&self.blocked_reason);
        item.completed_at = None;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionProvenance {
    pub kind: ItemCompletionProvenanceKind,
    pub reopen: ItemCompletionReopenState,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionState {
    pub item_id: Uuid,
    pub revision: u64,
    pub required_for_parent: bool,
    pub mode: ItemCompletionMode,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub provenance: Option<ItemCompletionProvenance>,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl ItemCompletionState {
    #[must_use]
    pub const fn empty(item_id: Uuid) -> Self {
        Self {
            item_id,
            revision: 0,
            required_for_parent: true,
            mode: ItemCompletionMode::Automatic,
            provenance: None,
            updated_at: None,
        }
    }

    /// Validates a stored or implicit-default completion policy.
    ///
    /// # Errors
    /// Rejects invalid revision, timestamp, mode or reopening custody.
    pub fn validate(&self) -> Result<(), ItemCompletionError> {
        if self.item_id.is_nil()
            || self.revision > i64::MAX as u64
            || (self.revision == 0 && *self != Self::empty(self.item_id))
            || (self.revision > 0 && self.updated_at.is_none())
            || self
                .updated_at
                .is_some_and(|timestamp| !has_postgres_timestamp_precision(timestamp))
            || (self.mode == ItemCompletionMode::Complete && self.provenance.is_none())
        {
            return Err(ItemCompletionError::Invalid);
        }
        if let Some(provenance) = &self.provenance {
            provenance.reopen.validate(self.item_id)?;
            if !matches!(
                (self.mode, provenance.kind),
                (
                    ItemCompletionMode::Automatic,
                    ItemCompletionProvenanceKind::Automatic
                ) | (
                    ItemCompletionMode::Complete,
                    ItemCompletionProvenanceKind::Manual
                )
            ) {
                return Err(ItemCompletionError::Invalid);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionCommand {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub expected_item_revision: u64,
    pub expected_completion_revision: u64,
    pub expected_evidence_hash: String,
    pub required_for_parent: bool,
    pub mode: ItemCompletionMode,
    /// Only an explicit terminal-parent review may supply missing reopening evidence.
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub reopening: Option<ItemCompletionReopenState>,
}

impl ItemCompletionCommand {
    /// Validates command shape before authoritative compare-and-set checks.
    ///
    /// # Errors
    /// Rejects invalid identifiers, revision bounds, evidence or reopening state.
    pub fn validate(&self, item_id: Uuid) -> Result<(), ItemCompletionError> {
        if self.schema_version != 1
            || item_id.is_nil()
            || self.operation_id.is_nil()
            || self.expected_item_revision == 0
            || self.expected_item_revision > i64::MAX as u64
            || self.expected_completion_revision > i64::MAX as u64
            || !valid_hash(&self.expected_evidence_hash)
        {
            return Err(ItemCompletionError::Invalid);
        }
        if let Some(reopening) = &self.reopening {
            reopening.validate(item_id)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionExecutionEvidence {
    pub revision: u64,
    pub live_item_ids: BTreeSet<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionCounts {
    pub required_descendants: u64,
    pub completed: u64,
    pub incomplete: u64,
    pub occurrence_evidence_required: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionSnapshot {
    pub schema_version: u16,
    pub item_id: Uuid,
    pub item_revision: u64,
    pub state: ItemCompletionState,
    pub evidence_hash: String,
    pub counts: ItemCompletionCounts,
    pub occurrence_evidence_required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemCompletionMutation {
    pub operation_id: Uuid,
    pub replayed: bool,
    pub completion: ItemCompletionSnapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemCompletionEffect {
    pub before_item: Item,
    pub after_item: Item,
    pub before_state: ItemCompletionState,
    pub after_state: ItemCompletionState,
    pub reason: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemCompletionPlan {
    pub effects: Vec<ItemCompletionEffect>,
    pub evaluated_item_ids: BTreeSet<Uuid>,
    pub snapshots: BTreeMap<Uuid, ItemCompletionSnapshot>,
}

/// Stable review proof; canonical revisions also make policy-only edits visible to old clients.
///
/// # Errors
/// Rejects invalid or oversized item, policy and execution evidence.
pub fn item_completion_evidence_hash(
    items: &[Item],
    states: &[ItemCompletionState],
    execution: &ItemCompletionExecutionEvidence,
) -> Result<String, ItemCompletionError> {
    let (items, states) = index(items, states)?;
    if execution.revision > i64::MAX as u64 || execution.live_item_ids.contains(&Uuid::nil()) {
        return Err(ItemCompletionError::Invalid);
    }
    if execution.live_item_ids.len() > MAX_COMPLETION_ITEMS {
        return Err(ItemCompletionError::TooLarge);
    }
    let mut writer = BoundedDigest::new(MAX_COMPLETION_PAYLOAD_BYTES);
    writer.serialize(&(1_u16, items, states, execution))?;
    let hash = writer.digest.finalize();
    Ok(format!("sha256:{hash:x}"))
}

/// Plans all effects but writes nothing. Transaction adapters retain exact primary receipts,
/// close their primary group, then emit these effects in independent bounded derived groups.
///
/// # Errors
/// Rejects incomplete/invalid evidence, stale reviews, ambiguous reopening,
/// unqualified recurring commands and conflicting live execution.
#[allow(clippy::too_many_lines)] // One deterministic evaluation retains before/after custody together.
pub fn plan_item_completion(
    items: &[Item],
    states: &[ItemCompletionState],
    execution: &ItemCompletionExecutionEvidence,
    command: Option<(Uuid, &ItemCompletionCommand)>,
    now: DateTime<Utc>,
) -> Result<ItemCompletionPlan, ItemCompletionError> {
    let now = truncate_to_postgres_timestamp_precision(now);
    let evidence_hash = item_completion_evidence_hash(items, states, execution)?;
    let (by_id, stored_states) = index(items, states)?;
    let parents: BTreeSet<_> = items.iter().filter_map(|item| item.parent_id).collect();
    let mut requested_states = stored_states.clone();
    let mut prepared_items = by_id.clone();
    if let Some((id, command)) = command {
        command.validate(id)?;
        let item = prepared_items
            .get_mut(&id)
            .ok_or(ItemCompletionError::ItemMissing)?;
        let state = requested_states
            .get_mut(&id)
            .ok_or(ItemCompletionError::Unavailable)?;
        if item.revision != command.expected_item_revision {
            return Err(ItemCompletionError::ItemStale);
        }
        if state.revision != command.expected_completion_revision {
            return Err(ItemCompletionError::CompletionStale);
        }
        if evidence_hash != command.expected_evidence_hash {
            return Err(ItemCompletionError::EvidenceStale);
        }
        if !parents.contains(&id)
            && state.mode == ItemCompletionMode::Automatic
            && state.provenance.is_none()
            && command.mode != ItemCompletionMode::Automatic
        {
            return Err(ItemCompletionError::ParentRequired);
        }
        state.required_for_parent = command.required_for_parent;
        state.mode = command.mode;
        if let Some(reopening) = &command.reopening {
            if state.provenance.is_some() || !item.status.is_terminal() || !parents.contains(&id) {
                return Err(ItemCompletionError::ReopeningReviewRequired);
            }
            // A reviewed legacy terminal parent first acquires a known open state.
            // The requested mode then evaluates it; no prior status is guessed.
            reopening.apply_to(item);
        }
    }
    let nodes = prepared_items
        .values()
        .map(|item| {
            let state = &requested_states[&item.id];
            if state.provenance.is_some() && item.status != ItemStatus::Completed {
                return Err(ItemCompletionError::ReopeningReviewRequired);
            }
            Ok(HierarchyCompletionItem {
                id: ItemId(item.id),
                parent_id: item.parent_id.map(ItemId),
                status: core_status(item.status),
                required_for_parent: state.required_for_parent,
                recurs: item.recurrence.is_some(),
                has_children_outside_plan: false,
                manual_override: match state.mode {
                    ItemCompletionMode::Automatic => HierarchyCompletionOverride::Automatic,
                    ItemCompletionMode::KeepOpen => HierarchyCompletionOverride::KeepOpen,
                    ItemCompletionMode::Complete => HierarchyCompletionOverride::Complete,
                },
                provenance: state.provenance.as_ref().map(|value| match value.kind {
                    ItemCompletionProvenanceKind::Automatic => {
                        HierarchyCompletionProvenance::Automatic {
                            prior_open_status: core_status(value.reopen.status),
                        }
                    }
                    ItemCompletionProvenanceKind::Manual => HierarchyCompletionProvenance::Manual {
                        prior_open_status: core_status(value.reopen.status),
                    },
                }),
            })
        })
        .collect::<Result<Vec<_>, ItemCompletionError>>()?;
    let evaluation = evaluate_hierarchy_completion(&nodes, HierarchyCompletionScope::OneOff)
        .map_err(|error| map_core_error(&error))?;
    let mut effects = Vec::new();
    let mut snapshots = BTreeMap::new();
    for (id, before_item) in &by_id {
        let decision = &evaluation.decisions[&ItemId(*id)];
        let before_state = &stored_states[id];
        let mut after_state = requested_states[id].clone();
        let prepared = &prepared_items[id];
        let mut after_item = prepared.clone();
        if decision.occurrence_evidence_required
            && command.is_some_and(|(target, _)| target == *id)
            && (before_state.mode != after_state.mode || prepared.status != before_item.status)
        {
            return Err(ItemCompletionError::OccurrenceEvidenceRequired);
        }
        let reopen = if decision.provenance.is_some() {
            Some(after_state.provenance.as_ref().map_or_else(
                || ItemCompletionReopenState::from_item(prepared),
                |value| Ok(value.reopen.clone()),
            )?)
        } else {
            None
        };
        after_state.provenance = match decision.provenance {
            Some(value) => Some(ItemCompletionProvenance {
                kind: match value {
                    HierarchyCompletionProvenance::Automatic { .. } => {
                        ItemCompletionProvenanceKind::Automatic
                    }
                    HierarchyCompletionProvenance::Manual { .. } => {
                        ItemCompletionProvenanceKind::Manual
                    }
                },
                reopen: reopen.ok_or(ItemCompletionError::Unavailable)?,
            }),
            None => None,
        };
        if decision.status == HierarchyProgressStatus::Completed
            && prepared.status != ItemStatus::Completed
        {
            after_item.status = ItemStatus::Completed;
            after_item.completed_at = Some(now);
            after_item.blocked_reason_kind = None;
            after_item.blocked_by_item_id = None;
            after_item.blocked_reason = None;
        } else if decision.status != core_status(prepared.status) {
            let reopen = requested_states[id]
                .provenance
                .as_ref()
                .ok_or(ItemCompletionError::ReopeningReviewRequired)?;
            if core_status(reopen.reopen.status) != decision.status {
                return Err(ItemCompletionError::Unavailable);
            }
            reopen.reopen.apply_to(&mut after_item);
        }
        after_item.is_executable = after_item.execution_is_allowed(parents.contains(id));
        let is_command = command.is_some_and(|(target, _)| target == *id);
        if after_item != *before_item || after_state != *before_state || is_command {
            if execution.live_item_ids.contains(id) && after_item.status != before_item.status {
                return Err(ItemCompletionError::ExecutionConflict);
            }
            after_item.revision = next_revision(before_item.revision)?;
            after_item.updated_at = now;
            after_state.revision = next_revision(before_state.revision)?;
            after_state.updated_at = Some(now);
            after_state.validate()?;
            effects.push(ItemCompletionEffect {
                before_item: before_item.clone(),
                after_item,
                before_state: before_state.clone(),
                after_state,
                reason: action_name(decision.action, is_command),
            });
        }
        let counts = decision.counts;
        snapshots.insert(
            *id,
            ItemCompletionSnapshot {
                schema_version: 1,
                item_id: *id,
                item_revision: before_item.revision,
                state: before_state.clone(),
                evidence_hash: evidence_hash.clone(),
                counts: ItemCompletionCounts {
                    required_descendants: counts.required_descendants,
                    completed: counts.completed,
                    incomplete: counts.incomplete,
                    occurrence_evidence_required: counts.occurrence_evidence_required,
                },
                occurrence_evidence_required: decision.occurrence_evidence_required,
            },
        );
    }
    Ok(ItemCompletionPlan {
        effects,
        evaluated_item_ids: by_id.keys().copied().collect(),
        snapshots,
    })
}

type IndexedCompletionForest = (BTreeMap<Uuid, Item>, BTreeMap<Uuid, ItemCompletionState>);

fn index(
    items: &[Item],
    states: &[ItemCompletionState],
) -> Result<IndexedCompletionForest, ItemCompletionError> {
    if items.len() > MAX_COMPLETION_ITEMS || states.len() > MAX_COMPLETION_ITEMS {
        return Err(ItemCompletionError::TooLarge);
    }
    // Streaming validation avoids materializing a second potentially enormous JSON body.
    BoundedDigest::new(MAX_COMPLETION_PAYLOAD_BYTES).serialize(&(items, states))?;
    let mut by_id = BTreeMap::new();
    for item in items {
        if item.id.is_nil()
            || item.revision == 0
            || item.revision > i64::MAX as u64
            || item.deleted_at.is_some()
            || by_id.insert(item.id, item.clone()).is_some()
        {
            return Err(ItemCompletionError::Invalid);
        }
    }
    let mut supplied = BTreeSet::new();
    let mut indexed = by_id
        .keys()
        .map(|id| (*id, ItemCompletionState::empty(*id)))
        .collect::<BTreeMap<_, _>>();
    for state in states {
        state.validate()?;
        if !by_id.contains_key(&state.item_id) || !supplied.insert(state.item_id) {
            return Err(ItemCompletionError::Invalid);
        }
        indexed.insert(state.item_id, state.clone());
    }
    Ok((by_id, indexed))
}

struct BoundedDigest {
    digest: Sha256,
    remaining: usize,
    exceeded: bool,
}

impl BoundedDigest {
    fn new(remaining: usize) -> Self {
        Self {
            digest: Sha256::new(),
            remaining,
            exceeded: false,
        }
    }

    fn serialize(&mut self, value: &impl Serialize) -> Result<(), ItemCompletionError> {
        serde_json::to_writer(&mut *self, value).map_err(|_| {
            if self.exceeded {
                ItemCompletionError::TooLarge
            } else {
                ItemCompletionError::Unavailable
            }
        })
    }
}

impl std::io::Write for BoundedDigest {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("completion payload limit exceeded"));
        };
        self.remaining = remaining;
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn core_status(status: ItemStatus) -> HierarchyProgressStatus {
    match status {
        ItemStatus::Inbox => HierarchyProgressStatus::Inbox,
        ItemStatus::Planned => HierarchyProgressStatus::Planned,
        ItemStatus::Scheduled => HierarchyProgressStatus::Scheduled,
        ItemStatus::InProgress => HierarchyProgressStatus::InProgress,
        ItemStatus::Paused => HierarchyProgressStatus::Paused,
        ItemStatus::Completed => HierarchyProgressStatus::Completed,
        ItemStatus::Skipped => HierarchyProgressStatus::Skipped,
        ItemStatus::Cancelled => HierarchyProgressStatus::Cancelled,
        ItemStatus::Blocked => HierarchyProgressStatus::Blocked,
    }
}

fn action_name(action: HierarchyCompletionAction, command: bool) -> &'static str {
    match action {
        HierarchyCompletionAction::Unchanged if command => "policy_reviewed",
        HierarchyCompletionAction::Unchanged => "unchanged",
        HierarchyCompletionAction::OccurrenceEvidenceRequired => "occurrence_evidence_required",
        HierarchyCompletionAction::AutomaticallyCompleted => "automatically_completed",
        HierarchyCompletionAction::AutomaticallyReopened => "automatically_reopened",
        HierarchyCompletionAction::ManuallyCompleted => "manually_completed",
        HierarchyCompletionAction::ManuallyKeptOpen => "manually_kept_open",
        HierarchyCompletionAction::ManualCompletionReleased => "manual_completion_released",
    }
}

fn map_core_error(error: &HierarchyCompletionError) -> ItemCompletionError {
    match error {
        HierarchyCompletionError::MissingPriorOpenStatus(_)
        | HierarchyCompletionError::AmbiguousTerminalParent(_)
        | HierarchyCompletionError::InvalidProvenance(_) => {
            ItemCompletionError::ReopeningReviewRequired
        }
        HierarchyCompletionError::Overflow(_) => ItemCompletionError::TooLarge,
        _ => ItemCompletionError::Invalid,
    }
}

fn next_revision(revision: u64) -> Result<u64, ItemCompletionError> {
    revision
        .checked_add(1)
        .filter(|value| i64::try_from(*value).is_ok())
        .ok_or(ItemCompletionError::Unavailable)
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ItemCompletionError {
    #[error("the completion command or forest is invalid")]
    Invalid,
    #[error("the canonical item was not found")]
    ItemMissing,
    #[error("the canonical item revision changed")]
    ItemStale,
    #[error("the completion policy revision changed")]
    CompletionStale,
    #[error("the reviewed completion evidence changed")]
    EvidenceStale,
    #[error("the operation identity belongs to a different request")]
    OperationReused,
    #[error("structural completion requires a parent")]
    ParentRequired,
    #[error("the parent needs explicit reopening and completion review")]
    ReopeningReviewRequired,
    #[error("qualified recurring occurrence evidence is required")]
    OccurrenceEvidenceRequired,
    #[error("the item has a live execution session")]
    ExecutionConflict,
    #[error("the complete completion forest exceeds resource bounds")]
    TooLarge,
    #[error("completion authority is unavailable")]
    Unavailable,
}
