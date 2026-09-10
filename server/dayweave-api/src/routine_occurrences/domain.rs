//! Pure lifecycle for an immutable, publication-qualified recurring instance.
//! Repositories admit definitions and current source/execution evidence under
//! their locks, and check exact receipts before calling this fresh-command path.
//! No result is a canonical template mutation or an execution command.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike as _, Utc};
use dayweave_core::{
    HierarchyCompletionAction, HierarchyCompletionEvaluation, HierarchyCompletionItem,
    HierarchyCompletionOverride, HierarchyCompletionProvenance, HierarchyCompletionScope,
    HierarchyProgressStatus, ItemId, OccurrenceId, RecurrenceOccurrenceIdentity,
    evaluate_hierarchy_completion,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    item_completion::{
        ItemCompletionCounts, ItemCompletionMode, ItemCompletionProvenance,
        ItemCompletionProvenanceKind, ItemCompletionReopenState,
    },
    items::{ItemKind, ItemStatus},
    scheduling::truncate_to_postgres_timestamp_precision,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub const MAX_ROUTINE_OCCURRENCE_MEMBERS: usize = 10_000;
pub const MAX_ROUTINE_OCCURRENCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceManifest {
    pub schema_version: u16,
    pub id: Uuid,
    pub series_item_id: Uuid,
    pub occurrence_id: Uuid,
    #[schema(value_type = Object)]
    pub identity: RecurrenceOccurrenceIdentity,
    pub nominal_start: DateTime<Utc>,
    pub nominal_end: DateTime<Utc>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub timezone_name: String,
    /// Repository-computed semantic definition hash. Titles, estimates and
    /// occurrence-local policies are not semantic template identity.
    pub definition_hash: String,
    pub members: Vec<RoutineOccurrenceMemberDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceMemberDefinition {
    pub item_id: Uuid,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub parent_id: Option<Uuid>,
    pub source_revision: u64,
    pub title: String,
    pub kind: ItemKind,
    pub recurs: bool,
    pub sibling_order: u32,
    pub required_for_parent: bool,
    pub initial_open: ItemCompletionReopenState,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceMemberState {
    pub item_id: Uuid,
    pub revision: u64,
    pub status: ItemStatus,
    pub required_for_parent: bool,
    pub mode: ItemCompletionMode,
    pub open: ItemCompletionReopenState,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub provenance: Option<ItemCompletionProvenance>,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub completed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceAggregate {
    pub manifest: RoutineOccurrenceManifest,
    pub revision: u64,
    pub members: Vec<RoutineOccurrenceMemberState>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceCommand {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub expected_instance_revision: u64,
    pub expected_member_revision: u64,
    pub expected_evidence_hash: String,
    pub action: RoutineOccurrenceAction,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineOccurrenceAction {
    SetOutcome {
        status: ItemStatus,
    },
    Reopen {
        open: ItemCompletionReopenState,
    },
    SetPolicy {
        required_for_parent: bool,
        mode: ItemCompletionMode,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceSourceEvidence {
    pub item_id: Uuid,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required)]
    pub current_revision: Option<u64>,
    pub eligible: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceWorkUnit {
    pub item_id: Uuid,
    pub occurrence_id: Uuid,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceEvidence {
    pub current_definition_hash: String,
    pub sources: Vec<RoutineOccurrenceSourceEvidence>,
    pub execution_revision: u64,
    pub live_work_units: BTreeSet<RoutineOccurrenceWorkUnit>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoutineOccurrenceReason {
    Unchanged,
    OutcomeRecorded,
    Reopened,
    PolicyReviewed,
    OccurrenceEvidenceRequired,
    AutomaticallyCompleted,
    AutomaticallyReopened,
    ManuallyCompleted,
    ManuallyKeptOpen,
    ManualCompletionReleased,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceMemberEvaluation {
    pub item_id: Uuid,
    pub counts: ItemCompletionCounts,
    pub occurrence_evidence_required: bool,
    pub reason: RoutineOccurrenceReason,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutineOccurrenceSnapshot {
    pub schema_version: u16,
    pub aggregate: RoutineOccurrenceAggregate,
    pub evidence_hash: String,
    pub fresh_edit_eligible: bool,
    pub members: Vec<RoutineOccurrenceMemberEvaluation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineOccurrenceMemberEffect {
    pub before: RoutineOccurrenceMemberState,
    pub after: RoutineOccurrenceMemberState,
    pub reason: RoutineOccurrenceReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineOccurrencePlan {
    pub snapshot: RoutineOccurrenceSnapshot,
    pub effects: Vec<RoutineOccurrenceMemberEffect>,
}

impl RoutineOccurrenceCommand {
    /// # Errors
    /// Rejects invalid revision, identity, evidence and action/reopening shapes.
    pub fn validate(&self, item_id: Uuid) -> Result<(), RoutineOccurrenceError> {
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || item_id.is_nil()
            || !valid_revision(self.expected_instance_revision)
            || !valid_revision(self.expected_member_revision)
            || !valid_hash(&self.expected_evidence_hash)
        {
            return Err(RoutineOccurrenceError::Invalid);
        }
        match &self.action {
            RoutineOccurrenceAction::SetOutcome { status }
                if !matches!(status, ItemStatus::Completed | ItemStatus::Skipped) =>
            {
                Err(RoutineOccurrenceError::Invalid)
            }
            RoutineOccurrenceAction::Reopen { open } => open
                .validate(item_id)
                .map_err(|_| RoutineOccurrenceError::Invalid),
            _ => Ok(()),
        }
    }
}

impl RoutineOccurrenceManifest {
    /// # Errors
    /// Rejects incomplete topology, unsupported roots, unknown open state,
    /// invalid identity/time context and excess resource use.
    pub fn validate(&self) -> Result<(), RoutineOccurrenceError> {
        validate_manifest(self).map(|_| ())
    }
}

impl RoutineOccurrenceAggregate {
    /// # Errors
    /// Rejects malformed, incomplete or not-yet-reconciled stored state.
    pub fn validate(&self) -> Result<(), RoutineOccurrenceError> {
        validate_aggregate(self).map(|_| ())
    }
}

/// Initialize only explicitly admitted open occurrence state, never template
/// terminal state or guessed reopening custody.
///
/// # Errors
/// Rejects an invalid or oversized manifest or unsupported timestamp.
pub fn initialize_routine_occurrence(
    mut manifest: RoutineOccurrenceManifest,
    now: DateTime<Utc>,
) -> Result<RoutineOccurrenceAggregate, RoutineOccurrenceError> {
    manifest.validate()?;
    let now = truncate_to_postgres_timestamp_precision(now);
    if !valid_instant(now) {
        return Err(RoutineOccurrenceError::Invalid);
    }
    manifest.members.sort_by_key(|member| member.item_id);
    let members = manifest
        .members
        .iter()
        .map(|member| RoutineOccurrenceMemberState {
            item_id: member.item_id,
            revision: 1,
            status: member.initial_open.status,
            required_for_parent: member.required_for_parent,
            mode: ItemCompletionMode::Automatic,
            open: member.initial_open.clone(),
            provenance: None,
            completed_at: None,
            updated_at: now,
        })
        .collect();
    let aggregate = RoutineOccurrenceAggregate {
        manifest,
        revision: 1,
        members,
    };
    aggregate.validate()?;
    Ok(aggregate)
}

/// Read historical evidence even when the current definition is no longer
/// eligible for fresh editing. This is not replay or authoring permission.
///
/// # Errors
/// Rejects invalid, incomplete or oversized aggregate/evidence inputs.
pub fn routine_occurrence_snapshot(
    aggregate: &RoutineOccurrenceAggregate,
    evidence: &RoutineOccurrenceEvidence,
) -> Result<RoutineOccurrenceSnapshot, RoutineOccurrenceError> {
    let evaluation = validate_aggregate(aggregate)?;
    let sources = validate_evidence(aggregate, evidence)?;
    let mut normalized = aggregate.clone();
    normalized
        .manifest
        .members
        .sort_by_key(|member| member.item_id);
    normalized.members.sort_by_key(|member| member.item_id);
    let evidence_hash = hash_value(&(
        1_u16,
        &normalized,
        &evidence.current_definition_hash,
        &sources,
        evidence.execution_revision,
        &evidence.live_work_units,
    ))?;
    let fresh_edit_eligible = evidence.current_definition_hash
        == aggregate.manifest.definition_hash
        && sources.values().all(|source| source.eligible);
    let snapshot = RoutineOccurrenceSnapshot {
        schema_version: 1,
        aggregate: normalized,
        evidence_hash,
        fresh_edit_eligible,
        members: member_evaluations(&evaluation),
    };
    hash_value(&snapshot)?;
    Ok(snapshot)
}

/// Plan one reviewed member command and all derived occurrence ancestors.
/// Repositories must settle exact old receipts before invoking this function.
///
/// # Errors
/// Rejects stale reviews, ineligible definitions, unqualified nested recurrence,
/// invalid action scope and changes to exact live occurrence work units.
#[allow(clippy::too_many_lines)] // Keep one immutable before/after evaluation and its CAS together.
pub fn plan_routine_occurrence(
    aggregate: &RoutineOccurrenceAggregate,
    evidence: &RoutineOccurrenceEvidence,
    item_id: Uuid,
    command: &RoutineOccurrenceCommand,
    now: DateTime<Utc>,
) -> Result<RoutineOccurrencePlan, RoutineOccurrenceError> {
    command.validate(item_id)?;
    let before = routine_occurrence_snapshot(aggregate, evidence)?;
    if aggregate.revision != command.expected_instance_revision {
        return Err(RoutineOccurrenceError::InstanceStale);
    }
    let old = aggregate
        .members
        .iter()
        .find(|member| member.item_id == item_id)
        .ok_or(RoutineOccurrenceError::MemberMissing)?;
    if old.revision != command.expected_member_revision {
        return Err(RoutineOccurrenceError::MemberStale);
    }
    if before.evidence_hash != command.expected_evidence_hash {
        return Err(RoutineOccurrenceError::EvidenceStale);
    }
    if evidence.current_definition_hash != aggregate.manifest.definition_hash {
        return Err(RoutineOccurrenceError::DefinitionChanged);
    }
    if !before.fresh_edit_eligible {
        return Err(RoutineOccurrenceError::SourceIneligible);
    }
    if before
        .members
        .iter()
        .any(|member| member.item_id == item_id && member.occurrence_evidence_required)
    {
        return Err(RoutineOccurrenceError::OccurrenceEvidenceRequired);
    }
    let now = truncate_to_postgres_timestamp_precision(now);
    if !valid_instant(now) {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let is_parent = aggregate
        .manifest
        .members
        .iter()
        .any(|member| member.parent_id == Some(item_id));
    let mut next = before.aggregate;
    let target = next
        .members
        .iter_mut()
        .find(|member| member.item_id == item_id)
        .ok_or(RoutineOccurrenceError::MemberMissing)?;
    match &command.action {
        RoutineOccurrenceAction::SetOutcome { status } => {
            if is_parent {
                return Err(RoutineOccurrenceError::LeafRequired);
            }
            target.status = *status;
            target.provenance = None;
        }
        RoutineOccurrenceAction::Reopen { open } => {
            if is_parent {
                return Err(RoutineOccurrenceError::LeafRequired);
            }
            target.status = open.status;
            target.open.clone_from(open);
            target.provenance = None;
        }
        RoutineOccurrenceAction::SetPolicy {
            required_for_parent,
            mode,
        } => {
            if !is_parent && *mode != ItemCompletionMode::Automatic {
                return Err(RoutineOccurrenceError::ParentRequired);
            }
            target.required_for_parent = *required_for_parent;
            target.mode = *mode;
        }
    }
    let evaluation = evaluate(&next.manifest, &next.members)?;
    let originals: BTreeMap<_, _> = aggregate
        .members
        .iter()
        .map(|member| (member.item_id, member))
        .collect();
    let mut effects = Vec::new();
    for member in &mut next.members {
        let decision = evaluation.decisions[&ItemId(member.item_id)];
        member.status = item_status(decision.status);
        member.provenance = decision
            .provenance
            .map(|provenance| ItemCompletionProvenance {
                kind: match provenance {
                    HierarchyCompletionProvenance::Automatic { .. } => {
                        ItemCompletionProvenanceKind::Automatic
                    }
                    HierarchyCompletionProvenance::Manual { .. } => {
                        ItemCompletionProvenanceKind::Manual
                    }
                },
                reopen: member.open.clone(),
            });
        let original = originals[&member.item_id];
        member.completed_at = if member.status == ItemStatus::Completed {
            original.completed_at.or(Some(now))
        } else {
            None
        };
        if member != original || member.item_id == item_id {
            if evidence
                .live_work_units
                .contains(&RoutineOccurrenceWorkUnit {
                    item_id: member.item_id,
                    occurrence_id: aggregate.manifest.occurrence_id,
                })
            {
                return Err(RoutineOccurrenceError::ExecutionConflict);
            }
            member.revision = next_revision(original.revision)?;
            member.updated_at = now;
            let reason = if member.item_id == item_id {
                match command.action {
                    RoutineOccurrenceAction::SetOutcome { .. } => {
                        RoutineOccurrenceReason::OutcomeRecorded
                    }
                    RoutineOccurrenceAction::Reopen { .. } => RoutineOccurrenceReason::Reopened,
                    RoutineOccurrenceAction::SetPolicy { .. }
                        if decision.action == HierarchyCompletionAction::Unchanged =>
                    {
                        RoutineOccurrenceReason::PolicyReviewed
                    }
                    RoutineOccurrenceAction::SetPolicy { .. } => action_reason(decision.action),
                }
            } else {
                action_reason(decision.action)
            };
            effects.push(RoutineOccurrenceMemberEffect {
                before: original.clone(),
                after: member.clone(),
                reason,
            });
        }
    }
    next.revision = next_revision(aggregate.revision)?;
    let mut snapshot = routine_occurrence_snapshot(&next, evidence)?;
    let reasons: BTreeMap<_, _> = effects
        .iter()
        .map(|effect| (effect.after.item_id, effect.reason))
        .collect();
    for member in &mut snapshot.members {
        if let Some(reason) = reasons.get(&member.item_id) {
            member.reason = *reason;
        }
    }
    Ok(RoutineOccurrencePlan { snapshot, effects })
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RoutineOccurrenceError {
    #[error("the occurrence command or evidence is invalid")]
    Invalid,
    #[error("the complete occurrence exceeds resource bounds")]
    TooLarge,
    #[error("the current recurring definition changed")]
    DefinitionChanged,
    #[error("an occurrence source is no longer eligible")]
    SourceIneligible,
    #[error("the occurrence instance revision changed")]
    InstanceStale,
    #[error("the occurrence member revision changed")]
    MemberStale,
    #[error("the reviewed occurrence evidence changed")]
    EvidenceStale,
    #[error("the occurrence member was not found")]
    MemberMissing,
    #[error("the recurring occurrence was not found")]
    OccurrenceMissing,
    #[error("the operation identity belongs to another request")]
    OperationReused,
    #[error("the occurrence cursor is invalid")]
    InvalidCursor,
    #[error("an outcome or reopening action requires a leaf")]
    LeafRequired,
    #[error("a manual completion mode requires a parent")]
    ParentRequired,
    #[error("independent nested occurrence evidence is required")]
    OccurrenceEvidenceRequired,
    #[error("an affected occurrence member has a live execution lease")]
    ExecutionConflict,
    #[error("occurrence authority is unavailable")]
    Unavailable,
}

fn validate_manifest(
    manifest: &RoutineOccurrenceManifest,
) -> Result<BTreeMap<Uuid, &RoutineOccurrenceMemberDefinition>, RoutineOccurrenceError> {
    if manifest.members.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    hash_value(manifest)?;
    if manifest.schema_version != 1
        || manifest.id.is_nil()
        || manifest.series_item_id.is_nil()
        || manifest.occurrence_id.get_version_num() != 5
        || manifest.occurrence_id.get_variant() != uuid::Variant::RFC4122
        || manifest.id == manifest.occurrence_id
        || !valid_hash(&manifest.definition_hash)
        || ![
            manifest.nominal_start,
            manifest.nominal_end,
            manifest.window_start,
            manifest.window_end,
        ]
        .into_iter()
        .all(valid_instant)
        || manifest.nominal_start >= manifest.nominal_end
        || manifest.window_start >= manifest.window_end
        || manifest.timezone_name.is_empty()
        || manifest.timezone_name.len() > 100
    {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let timezone = manifest
        .timezone_name
        .parse::<chrono_tz::Tz>()
        .map_err(|_| RoutineOccurrenceError::Invalid)?;
    if !identity_is_valid(manifest, timezone) {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let mut members = BTreeMap::new();
    for member in &manifest.members {
        if member.item_id.is_nil()
            || member
                .parent_id
                .is_some_and(|id| id.is_nil() || id == member.item_id)
            || !valid_revision(member.source_revision)
            || member.sibling_order > 1_000_000
            || member.title.is_empty()
            || member.title.trim() != member.title
            || member.title.chars().count() > 500
            || member.title.chars().any(char::is_control)
            || (member.kind == ItemKind::Habit && !member.recurs)
            || members.insert(member.item_id, member).is_some()
        {
            return Err(RoutineOccurrenceError::Invalid);
        }
        member
            .initial_open
            .validate(member.item_id)
            .map_err(|_| RoutineOccurrenceError::Invalid)?;
    }
    let root = members
        .get(&manifest.series_item_id)
        .ok_or(RoutineOccurrenceError::Invalid)?;
    if !matches!(root.kind, ItemKind::Task | ItemKind::Routine)
        || !root.recurs
        || root.parent_id.is_some()
    {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let nodes = manifest
        .members
        .iter()
        .map(|member| HierarchyCompletionItem {
            id: ItemId(member.item_id),
            parent_id: member.parent_id.map(ItemId),
            status: core_status(member.initial_open.status),
            required_for_parent: member.required_for_parent,
            recurs: member.recurs,
            has_children_outside_plan: false,
            manual_override: HierarchyCompletionOverride::Automatic,
            provenance: None,
        })
        .collect::<Vec<_>>();
    evaluate_hierarchy_completion(&nodes, scope(manifest))
        .map_err(|_| RoutineOccurrenceError::Invalid)?;
    Ok(members)
}

fn validate_aggregate(
    aggregate: &RoutineOccurrenceAggregate,
) -> Result<HierarchyCompletionEvaluation, RoutineOccurrenceError> {
    if aggregate.members.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    hash_value(aggregate)?;
    let definitions = validate_manifest(&aggregate.manifest)?;
    if !valid_revision(aggregate.revision) || aggregate.members.len() != definitions.len() {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let parents: BTreeSet<_> = definitions
        .values()
        .filter_map(|member| member.parent_id)
        .collect();
    let mut seen = BTreeSet::new();
    for member in &aggregate.members {
        if !definitions.contains_key(&member.item_id)
            || !seen.insert(member.item_id)
            || !valid_revision(member.revision)
            || member.revision > aggregate.revision
            || !valid_instant(member.updated_at)
            || !matches!(
                member.status,
                ItemStatus::Inbox
                    | ItemStatus::Planned
                    | ItemStatus::Blocked
                    | ItemStatus::Completed
                    | ItemStatus::Skipped
                    | ItemStatus::Cancelled
            )
            || (member.status == ItemStatus::Completed) != member.completed_at.is_some()
            || member
                .completed_at
                .is_some_and(|instant| !valid_instant(instant))
            || (is_open(member.status) && member.status != member.open.status)
            || (!parents.contains(&member.item_id)
                && (member.mode != ItemCompletionMode::Automatic || member.provenance.is_some()))
            || (parents.contains(&member.item_id)
                && member.status.is_terminal()
                && member.provenance.is_none())
        {
            return Err(RoutineOccurrenceError::Invalid);
        }
        member
            .open
            .validate(member.item_id)
            .map_err(|_| RoutineOccurrenceError::Invalid)?;
        if let Some(provenance) = &member.provenance {
            if member.status != ItemStatus::Completed
                || provenance.reopen != member.open
                || !matches!(
                    (member.mode, provenance.kind),
                    (
                        ItemCompletionMode::Automatic,
                        ItemCompletionProvenanceKind::Automatic
                    ) | (
                        ItemCompletionMode::Complete,
                        ItemCompletionProvenanceKind::Manual
                    )
                )
            {
                return Err(RoutineOccurrenceError::Invalid);
            }
        } else if member.mode == ItemCompletionMode::Complete {
            return Err(RoutineOccurrenceError::Invalid);
        }
    }
    let evaluation = evaluate(&aggregate.manifest, &aggregate.members)?;
    for member in &aggregate.members {
        let decision = evaluation.decisions[&ItemId(member.item_id)];
        if decision.status != core_status(member.status)
            || decision.provenance != core_provenance(member.provenance.as_ref())
        {
            return Err(RoutineOccurrenceError::Invalid);
        }
    }
    Ok(evaluation)
}

fn validate_evidence<'a>(
    aggregate: &RoutineOccurrenceAggregate,
    evidence: &'a RoutineOccurrenceEvidence,
) -> Result<BTreeMap<Uuid, &'a RoutineOccurrenceSourceEvidence>, RoutineOccurrenceError> {
    if evidence.sources.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS
        || evidence.live_work_units.len() > MAX_ROUTINE_OCCURRENCE_MEMBERS
    {
        return Err(RoutineOccurrenceError::TooLarge);
    }
    hash_value(&(aggregate, evidence))?;
    if evidence.sources.len() != aggregate.manifest.members.len()
        || evidence.execution_revision > i64::MAX as u64
        || !valid_hash(&evidence.current_definition_hash)
        || evidence
            .live_work_units
            .iter()
            .any(|unit| unit.item_id.is_nil() || unit.occurrence_id.is_nil())
    {
        return Err(RoutineOccurrenceError::Invalid);
    }
    let definitions: BTreeMap<_, _> = aggregate
        .manifest
        .members
        .iter()
        .map(|member| (member.item_id, member))
        .collect();
    let mut sources = BTreeMap::new();
    for source in &evidence.sources {
        let definition = definitions
            .get(&source.item_id)
            .ok_or(RoutineOccurrenceError::Invalid)?;
        if sources.insert(source.item_id, source).is_some()
            || source
                .current_revision
                .is_some_and(|revision| !valid_revision(revision))
            || (source.eligible
                && source
                    .current_revision
                    .is_none_or(|revision| revision < definition.source_revision))
        {
            return Err(RoutineOccurrenceError::Invalid);
        }
    }
    Ok(sources)
}

fn evaluate(
    manifest: &RoutineOccurrenceManifest,
    members: &[RoutineOccurrenceMemberState],
) -> Result<HierarchyCompletionEvaluation, RoutineOccurrenceError> {
    let definitions: BTreeMap<_, _> = manifest
        .members
        .iter()
        .map(|member| (member.item_id, member))
        .collect();
    let nodes = members
        .iter()
        .map(|member| {
            let definition = definitions
                .get(&member.item_id)
                .ok_or(RoutineOccurrenceError::Invalid)?;
            Ok(HierarchyCompletionItem {
                id: ItemId(member.item_id),
                parent_id: definition.parent_id.map(ItemId),
                status: core_status(member.status),
                required_for_parent: member.required_for_parent,
                recurs: definition.recurs,
                has_children_outside_plan: false,
                manual_override: match member.mode {
                    ItemCompletionMode::Automatic => HierarchyCompletionOverride::Automatic,
                    ItemCompletionMode::KeepOpen => HierarchyCompletionOverride::KeepOpen,
                    ItemCompletionMode::Complete => HierarchyCompletionOverride::Complete,
                },
                provenance: core_provenance(member.provenance.as_ref()),
            })
        })
        .collect::<Result<Vec<_>, RoutineOccurrenceError>>()?;
    evaluate_hierarchy_completion(&nodes, scope(manifest))
        .map_err(|_| RoutineOccurrenceError::Invalid)
}

fn scope(manifest: &RoutineOccurrenceManifest) -> HierarchyCompletionScope {
    HierarchyCompletionScope::Occurrence {
        root_id: ItemId(manifest.series_item_id),
        occurrence_id: OccurrenceId(manifest.occurrence_id),
    }
}

fn core_provenance(
    provenance: Option<&ItemCompletionProvenance>,
) -> Option<HierarchyCompletionProvenance> {
    provenance.map(|value| match value.kind {
        ItemCompletionProvenanceKind::Automatic => HierarchyCompletionProvenance::Automatic {
            prior_open_status: core_status(value.reopen.status),
        },
        ItemCompletionProvenanceKind::Manual => HierarchyCompletionProvenance::Manual {
            prior_open_status: core_status(value.reopen.status),
        },
    })
}

fn member_evaluations(
    evaluation: &HierarchyCompletionEvaluation,
) -> Vec<RoutineOccurrenceMemberEvaluation> {
    evaluation
        .decisions
        .iter()
        .map(|(id, decision)| RoutineOccurrenceMemberEvaluation {
            item_id: id.0,
            counts: ItemCompletionCounts {
                required_descendants: decision.counts.required_descendants,
                completed: decision.counts.completed,
                incomplete: decision.counts.incomplete,
                occurrence_evidence_required: decision.counts.occurrence_evidence_required,
            },
            occurrence_evidence_required: decision.occurrence_evidence_required,
            reason: action_reason(decision.action),
        })
        .collect()
}

fn action_reason(action: HierarchyCompletionAction) -> RoutineOccurrenceReason {
    match action {
        HierarchyCompletionAction::Unchanged => RoutineOccurrenceReason::Unchanged,
        HierarchyCompletionAction::OccurrenceEvidenceRequired => {
            RoutineOccurrenceReason::OccurrenceEvidenceRequired
        }
        HierarchyCompletionAction::AutomaticallyCompleted => {
            RoutineOccurrenceReason::AutomaticallyCompleted
        }
        HierarchyCompletionAction::AutomaticallyReopened => {
            RoutineOccurrenceReason::AutomaticallyReopened
        }
        HierarchyCompletionAction::ManuallyCompleted => RoutineOccurrenceReason::ManuallyCompleted,
        HierarchyCompletionAction::ManuallyKeptOpen => RoutineOccurrenceReason::ManuallyKeptOpen,
        HierarchyCompletionAction::ManualCompletionReleased => {
            RoutineOccurrenceReason::ManualCompletionReleased
        }
    }
}

const fn core_status(status: ItemStatus) -> HierarchyProgressStatus {
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

const fn item_status(status: HierarchyProgressStatus) -> ItemStatus {
    match status {
        HierarchyProgressStatus::Inbox => ItemStatus::Inbox,
        HierarchyProgressStatus::Planned => ItemStatus::Planned,
        HierarchyProgressStatus::Scheduled => ItemStatus::Scheduled,
        HierarchyProgressStatus::InProgress => ItemStatus::InProgress,
        HierarchyProgressStatus::Paused => ItemStatus::Paused,
        HierarchyProgressStatus::Completed => ItemStatus::Completed,
        HierarchyProgressStatus::Skipped => ItemStatus::Skipped,
        HierarchyProgressStatus::Cancelled => ItemStatus::Cancelled,
        HierarchyProgressStatus::Blocked => ItemStatus::Blocked,
    }
}

const fn is_open(status: ItemStatus) -> bool {
    matches!(
        status,
        ItemStatus::Inbox | ItemStatus::Planned | ItemStatus::Blocked
    )
}

fn valid_instant(value: DateTime<Utc>) -> bool {
    (1..=9_999).contains(&value.year()) && value.timestamp_subsec_nanos().is_multiple_of(1_000)
}

const fn valid_revision(value: u64) -> bool {
    value > 0 && value <= i64::MAX as u64
}

fn next_revision(value: u64) -> Result<u64, RoutineOccurrenceError> {
    value
        .checked_add(1)
        .filter(|revision| valid_revision(*revision))
        .ok_or(RoutineOccurrenceError::Unavailable)
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

fn hash_value(value: &impl Serialize) -> Result<String, RoutineOccurrenceError> {
    let mut writer = BoundedDigest {
        digest: Sha256::new(),
        remaining: MAX_ROUTINE_OCCURRENCE_BYTES,
        exceeded: false,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        if writer.exceeded {
            RoutineOccurrenceError::TooLarge
        } else {
            RoutineOccurrenceError::Unavailable
        }
    })?;
    Ok(format!("sha256:{:x}", writer.digest.finalize()))
}

struct BoundedDigest {
    digest: Sha256,
    remaining: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedDigest {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("occurrence payload limit exceeded"));
        };
        self.remaining = remaining;
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn identity_is_valid(manifest: &RoutineOccurrenceManifest, timezone: chrono_tz::Tz) -> bool {
    let local = manifest.nominal_start.with_timezone(&timezone).date_naive();
    let local_last = manifest
        .nominal_end
        .checked_sub_signed(chrono::Duration::microseconds(1))
        .map(|instant| instant.with_timezone(&timezone).date_naive());
    let same_day = local_last == Some(local);
    let matches_date = |date: time::Date| {
        same_day
            && date.year() == local.year()
            && u32::from(u8::from(date.month())) == local.month()
            && u32::from(date.day()) == local.day()
    };
    let valid_anchor = |anchor: time::OffsetDateTime| {
        (1..=9_999).contains(&anchor.year())
            && anchor.nanosecond().is_multiple_of(1_000)
            && anchor.offset().whole_seconds().unsigned_abs() <= 86_340
    };
    match manifest.identity {
        RecurrenceOccurrenceIdentity::CalendarDay {
            date,
            bucket_ordinal,
        } => matches_date(date) && bucket_ordinal < u16::MAX,
        RecurrenceOccurrenceIdentity::CalendarWeek {
            week_key,
            bucket_ordinal,
        } => {
            let date = u8::try_from(local.month())
                .ok()
                .and_then(|month| time::Month::try_from(month).ok())
                .and_then(|month| {
                    u8::try_from(local.day()).ok().and_then(|day| {
                        time::Date::from_calendar_date(local.year(), month, day).ok()
                    })
                });
            same_day
                && bucket_ordinal < u16::MAX
                && date.is_some_and(|date| {
                    week_key
                        .checked_add(6)
                        .is_some_and(|last| (week_key..=last).contains(&date.to_julian_day()))
                })
        }
        RecurrenceOccurrenceIdentity::CalendarMonth {
            year,
            month,
            bucket_ordinal,
        } => {
            same_day
                && year == local.year()
                && u32::from(month) == local.month()
                && bucket_ordinal < u16::MAX
        }
        RecurrenceOccurrenceIdentity::RollingMinutes { index, anchor } => {
            u32::try_from(index).is_ok() && valid_anchor(anchor)
        }
        RecurrenceOccurrenceIdentity::AfterCompletion { anchor } => valid_anchor(anchor),
        RecurrenceOccurrenceIdentity::RollingMonth {
            cycle,
            index,
            anchor,
        } => (0..=i64::from(i32::MAX)).contains(&cycle) && index < u16::MAX && valid_anchor(anchor),
        RecurrenceOccurrenceIdentity::Custom => false,
        RecurrenceOccurrenceIdentity::CustomRule {
            rule_id,
            sequence,
            date,
        } => {
            rule_id.get_version_num() == 5
                && rule_id.get_variant() == uuid::Variant::RFC4122
                && sequence < 10_000
                && matches_date(date)
        }
    }
}
