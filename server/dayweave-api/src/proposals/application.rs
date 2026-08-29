use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::items::{Item, NewItem, ReplaceItem};

/// The largest proposal batch accepted by the application contract.
pub const MAX_PROPOSAL_COMMANDS: usize = 100;
/// The largest proposal group reviewed and applied as one transaction.
pub const MAX_PROPOSALS_PER_PREVIEW: usize = 20;
/// Stable identifier for the only executable proposal payload schema.
pub const PROPOSAL_CHANGE_SET_SCHEMA_V1: &str = "dayweave.proposal-change-set/1";

/// The only proposal change-set schema this server is permitted to execute.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub enum ProposalChangeSetSchema {
    #[serde(rename = "dayweave.proposal-change-set/1")]
    V1,
}

/// A strictly typed, atomic set of canonical item commands.
///
/// Deserialization validates the command count, command identifiers, target
/// identifiers, and optimistic revisions. Generic or legacy proposal payloads
/// therefore cannot be treated as executable change sets.
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct ProposalChangeSet {
    pub schema: ProposalChangeSetSchema,
    pub commands: Vec<ProposalCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalChangeSetWire {
    schema: ProposalChangeSetSchema,
    commands: Vec<ProposalCommand>,
}

impl<'de> Deserialize<'de> for ProposalChangeSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProposalChangeSetWire::deserialize(deserializer)?;
        let change_set = Self {
            schema: wire.schema,
            commands: wire.commands,
        };
        change_set.validate().map_err(serde::de::Error::custom)?;
        Ok(change_set)
    }
}

impl ProposalChangeSet {
    /// Constructs a validated v1 change set.
    ///
    /// # Errors
    ///
    /// Returns [`ProposalChangeSetError`] if the batch is empty or too large,
    /// identifiers are duplicated, or an optimistic revision is zero.
    pub fn new(commands: Vec<ProposalCommand>) -> Result<Self, ProposalChangeSetError> {
        let change_set = Self {
            schema: ProposalChangeSetSchema::V1,
            commands,
        };
        change_set.validate()?;
        Ok(change_set)
    }

    /// Parses a generic stored proposal payload using the executable contract.
    ///
    /// This is the fail-closed boundary for legacy and recommendation payloads.
    ///
    /// # Errors
    ///
    /// Returns [`ProposalChangeSetError::InvalidPayload`] unless the value is a
    /// complete, strictly typed, validated v1 change set.
    pub fn from_payload(payload: &Value) -> Result<Self, ProposalChangeSetError> {
        serde_json::from_value(payload.clone()).map_err(|_| ProposalChangeSetError::InvalidPayload)
    }

    /// Revalidates a programmatically assembled change set.
    ///
    /// # Errors
    ///
    /// Returns a field-specific [`ProposalChangeSetError`] for an unsafe batch.
    pub fn validate(&self) -> Result<(), ProposalChangeSetError> {
        if self.commands.is_empty() {
            return Err(ProposalChangeSetError::Empty);
        }
        if self.commands.len() > MAX_PROPOSAL_COMMANDS {
            return Err(ProposalChangeSetError::TooManyCommands {
                maximum: MAX_PROPOSAL_COMMANDS,
            });
        }

        let mut command_ids = HashSet::with_capacity(self.commands.len());
        let mut target_ids = HashSet::with_capacity(self.commands.len());
        for command in &self.commands {
            if !command_ids.insert(command.command_id()) {
                return Err(ProposalChangeSetError::DuplicateCommandId(
                    command.command_id(),
                ));
            }
            if !target_ids.insert(command.target_item_id()) {
                return Err(ProposalChangeSetError::DuplicateTargetId(
                    command.target_item_id(),
                ));
            }
            if command.expected_revision() == Some(0) {
                return Err(ProposalChangeSetError::ExpectedRevisionMustBePositive(
                    command.command_id(),
                ));
            }
        }
        Ok(())
    }
}

/// A command that can be translated directly into an ordinary item mutation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposalCommand {
    CreateItem {
        command_id: Uuid,
        item: NewItem,
    },
    ReplaceItem {
        command_id: Uuid,
        item_id: Uuid,
        expected_revision: u64,
        item: ReplaceItem,
    },
    TrashItem {
        command_id: Uuid,
        item_id: Uuid,
        expected_revision: u64,
    },
    RestoreItem {
        command_id: Uuid,
        item_id: Uuid,
        expected_revision: u64,
    },
}

impl ProposalCommand {
    #[must_use]
    pub const fn command_id(&self) -> Uuid {
        match self {
            Self::CreateItem { command_id, .. }
            | Self::ReplaceItem { command_id, .. }
            | Self::TrashItem { command_id, .. }
            | Self::RestoreItem { command_id, .. } => *command_id,
        }
    }

    #[must_use]
    pub const fn operation(&self) -> ProposalOperation {
        match self {
            Self::CreateItem { .. } => ProposalOperation::CreateItem,
            Self::ReplaceItem { .. } => ProposalOperation::ReplaceItem,
            Self::TrashItem { .. } => ProposalOperation::TrashItem,
            Self::RestoreItem { .. } => ProposalOperation::RestoreItem,
        }
    }

    #[must_use]
    pub const fn target_item_id(&self) -> Uuid {
        match self {
            Self::CreateItem { item, .. } => item.id,
            Self::ReplaceItem { item_id, .. }
            | Self::TrashItem { item_id, .. }
            | Self::RestoreItem { item_id, .. } => *item_id,
        }
    }

    #[must_use]
    pub const fn expected_revision(&self) -> Option<u64> {
        match self {
            Self::CreateItem { .. } => None,
            Self::ReplaceItem {
                expected_revision, ..
            }
            | Self::TrashItem {
                expected_revision, ..
            }
            | Self::RestoreItem {
                expected_revision, ..
            } => Some(*expected_revision),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalOperation {
    CreateItem,
    ReplaceItem,
    TrashItem,
    RestoreItem,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProposalChangeSetError {
    #[error("payload does not match dayweave.proposal-change-set/1")]
    InvalidPayload,
    #[error("proposal change set must contain at least one command")]
    Empty,
    #[error("proposal change set exceeds the maximum of {maximum} commands")]
    TooManyCommands { maximum: usize },
    #[error("proposal command id {0} is duplicated")]
    DuplicateCommandId(Uuid),
    #[error("proposal target item id {0} is duplicated")]
    DuplicateTargetId(Uuid),
    #[error("proposal command {0} must use a positive expected revision")]
    ExpectedRevisionMustBePositive(Uuid),
}

/// One whole proposal selected for an atomic grouped review.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalPreviewMember {
    pub proposal_id: Uuid,
    pub expected_revision: u64,
}

/// Whole proposals to review. Commands cannot be cherry-picked because doing
/// so could break dependencies inside an AI-authored goal decomposition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalPreviewRequest {
    pub proposals: Vec<ProposalPreviewMember>,
}

/// A content-bound, expiring review of a proposal change set.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalChangeSetPreview {
    pub preview_id: Uuid,
    pub proposals: Vec<ProposalPreviewMember>,
    pub change_set_schema: ProposalChangeSetSchema,
    pub command_ids: Vec<Uuid>,
    pub review_hash: String,
    pub expires_at: DateTime<Utc>,
    pub can_apply: bool,
    pub maximum_risk: ProposalRiskLevel,
    pub requires_explicit_approval: bool,
    pub diffs: Vec<ProposalItemDiff>,
    pub implicit_diffs: Vec<ProposalImplicitItemDiff>,
    pub risks: Vec<ProposalRisk>,
    pub conflicts: Vec<ProposalConflict>,
}

/// The stable item fields that can be called out by a review diff.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalItemField {
    IsSensitive,
    Kind,
    Status,
    Title,
    Notes,
    TimezoneName,
    DurationSeconds,
    DeadlineAt,
    EarliestStartAt,
    Recurrence,
    FlexibleConstraints,
    SplitPolicy,
    Importance,
    Urgency,
    ParentId,
    SiblingOrder,
    IsExecutable,
    Revision,
    CompletedAt,
    DeletedAt,
}

/// The before/after state produced by simulating one command.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalItemDiff {
    pub command_id: Uuid,
    pub operation: ProposalOperation,
    pub item_id: Uuid,
    pub changed_fields: Vec<ProposalItemField>,
    pub before: Option<Item>,
    pub after: Option<Item>,
}

/// A canonical item changed by command side effects rather than by a direct
/// command target. Hierarchy refreshes are shown separately so approval never
/// hides parent revisions or derived executability changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalImplicitItemDiff {
    pub item_id: Uuid,
    pub reason: ProposalImplicitChangeReason,
    pub changed_fields: Vec<ProposalItemField>,
    pub before: Item,
    pub after: Item,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalImplicitChangeReason {
    HierarchyRefresh,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// Machine-readable reasons why a proposal needs attention before application.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalRiskCode {
    CreatesItem,
    ReplacesItem,
    TrashesItem,
    RestoresItem,
    ChangesDeadline,
    RelaxesDeadline,
    ChangesHierarchy,
    ChangesSensitivity,
    ChangesRecurrence,
    ChangesExecutionState,
    SensitiveContent,
    BulkChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalRisk {
    pub code: ProposalRiskCode,
    pub level: ProposalRiskLevel,
    pub command_id: Option<Uuid>,
    pub item_id: Option<Uuid>,
    pub requires_explicit_approval: bool,
    pub summary: String,
}

/// Machine-readable reasons why a preview cannot currently be applied.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalConflictCode {
    ProposalNotPending,
    ProposalExpired,
    ProposalRevisionMismatch,
    ItemAlreadyExists,
    ItemNotFound,
    ItemRevisionMismatch,
    ParentNotFound,
    HierarchyCycle,
    InvalidParentState,
    NonLeafExecutable,
    HasChildren,
    DeletedParent,
    InvalidItem,
    ProviderManagedItem,
    PreviewExpired,
    PreviewMismatch,
    PreviewNotApplicable,
    AlreadyApplied,
    UndoExpired,
    UndoDiverged,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalConflict {
    pub code: ProposalConflictCode,
    pub command_id: Option<Uuid>,
    pub item_id: Option<Uuid>,
    pub expected_revision: Option<u64>,
    pub actual_revision: Option<u64>,
    pub summary: String,
}

/// Approval of one exact, previously reviewed preview.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalApplyRequest {
    pub expected_review_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalApplicationStatus {
    Applied,
    Undone,
}

/// Durable confirmation and undo fence for one successful atomic application.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalApplicationReceipt {
    pub application_id: Uuid,
    pub proposals: Vec<ProposalAppliedMember>,
    pub application_revision: u64,
    pub status: ProposalApplicationStatus,
    pub command_ids: Vec<Uuid>,
    pub affected_item_ids: Vec<Uuid>,
    pub applied_at: DateTime<Utc>,
    pub undo_expires_at: DateTime<Utc>,
    pub undone_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalAppliedMember {
    pub proposal_id: Uuid,
    pub applied_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalApplyResponse {
    pub application: ProposalApplicationReceipt,
    pub replayed: bool,
}

/// Requests an undo only if the durable application revision still matches.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalUndoRequest {
    pub expected_application_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalUndoResponse {
    pub application: ProposalApplicationReceipt,
    pub replayed: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::items::{ItemKind, ItemStatus, SplitPolicy};

    fn new_item(id: Uuid) -> NewItem {
        NewItem {
            id,
            is_sensitive: false,
            kind: ItemKind::Task,
            status: ItemStatus::Inbox,
            title: "Review plan".to_owned(),
            notes: None,
            timezone_name: "Europe/Madrid".to_owned(),
            duration_seconds: Some(1_800),
            deadline_at: None,
            earliest_start_at: None,
            recurrence: None,
            flexible_constraints: json!({}),
            split_policy: SplitPolicy::Indivisible,
            importance: 5,
            urgency: 4,
            parent_id: None,
            sibling_order: 0,
        }
    }

    fn create_command(command_id: Uuid, item_id: Uuid) -> ProposalCommand {
        ProposalCommand::CreateItem {
            command_id,
            item: new_item(item_id),
        }
    }

    fn serialized_change_set(commands: Vec<ProposalCommand>) -> Value {
        serde_json::to_value(ProposalChangeSet {
            schema: ProposalChangeSetSchema::V1,
            commands,
        })
        .expect("change set should serialize")
    }

    #[test]
    fn rejects_unknown_top_level_and_command_fields() {
        let command_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let mut top_level = serialized_change_set(vec![create_command(command_id, item_id)]);
        top_level
            .as_object_mut()
            .expect("object")
            .insert("legacy_hint".to_owned(), json!(true));
        assert!(ProposalChangeSet::from_payload(&top_level).is_err());

        let mut command_level = serialized_change_set(vec![create_command(command_id, item_id)]);
        command_level["commands"][0]
            .as_object_mut()
            .expect("command object")
            .insert("provider_write".to_owned(), json!(true));
        assert!(ProposalChangeSet::from_payload(&command_level).is_err());
    }

    #[test]
    fn rejects_duplicate_command_ids() {
        let command_id = Uuid::new_v4();
        let value = serialized_change_set(vec![
            create_command(command_id, Uuid::new_v4()),
            create_command(command_id, Uuid::new_v4()),
        ]);

        assert_eq!(
            ProposalChangeSet::from_payload(&value),
            Err(ProposalChangeSetError::InvalidPayload)
        );
        assert_eq!(
            ProposalChangeSet::new(vec![
                create_command(command_id, Uuid::new_v4()),
                create_command(command_id, Uuid::new_v4()),
            ]),
            Err(ProposalChangeSetError::DuplicateCommandId(command_id))
        );
    }

    #[test]
    fn rejects_duplicate_target_ids_across_operations() {
        let item_id = Uuid::new_v4();
        let value = serialized_change_set(vec![
            create_command(Uuid::new_v4(), item_id),
            ProposalCommand::TrashItem {
                command_id: Uuid::new_v4(),
                item_id,
                expected_revision: 1,
            },
        ]);

        assert!(ProposalChangeSet::from_payload(&value).is_err());
    }

    #[test]
    fn enforces_command_count_bounds() {
        assert_eq!(
            ProposalChangeSet::new(Vec::new()),
            Err(ProposalChangeSetError::Empty)
        );

        let commands = (0..=MAX_PROPOSAL_COMMANDS)
            .map(|_| create_command(Uuid::new_v4(), Uuid::new_v4()))
            .collect();
        assert_eq!(
            ProposalChangeSet::new(commands),
            Err(ProposalChangeSetError::TooManyCommands {
                maximum: MAX_PROPOSAL_COMMANDS
            })
        );
    }

    #[test]
    fn rejects_legacy_and_wrong_schema_payloads() {
        let legacy = json!({
            "schema_version": 1,
            "operations": [{"operation": "create_item", "parameters": {}}]
        });
        assert_eq!(
            ProposalChangeSet::from_payload(&legacy),
            Err(ProposalChangeSetError::InvalidPayload)
        );

        let wrong_schema = json!({
            "schema": "dayweave.proposal-change-set/2",
            "commands": []
        });
        assert_eq!(
            ProposalChangeSet::from_payload(&wrong_schema),
            Err(ProposalChangeSetError::InvalidPayload)
        );
    }

    #[test]
    fn rejects_zero_expected_revision() {
        let command_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        let command = ProposalCommand::TrashItem {
            command_id,
            item_id,
            expected_revision: 0,
        };
        assert_eq!(
            ProposalChangeSet::new(vec![command]),
            Err(ProposalChangeSetError::ExpectedRevisionMustBePositive(
                command_id
            ))
        );
    }
}
