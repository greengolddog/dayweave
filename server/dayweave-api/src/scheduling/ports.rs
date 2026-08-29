use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::proposals::{Proposal, ProposalChangeSet, ProposalCommand, ProposalKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleAccess {
    pub subject: String,
    pub include_sensitive: bool,
    pub workspace_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleDetail {
    BusyOnly,
    Summary,
    Full,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleQuery {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub detail: ScheduleDetail,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredSchedule {
    pub revision: String,
    pub timezone: String,
    pub blocks: Vec<StoredScheduleBlock>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredScheduleBlock {
    pub id: String,
    pub item_id: Option<String>,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub kind: String,
    pub status: String,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScheduleView {
    pub revision: String,
    pub timezone: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub blocks: Vec<ScheduleBlockView>,
    pub redacted_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScheduleBlockView {
    pub id: Option<String>,
    pub item_id: Option<String>,
    pub title: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub kind: String,
    pub status: String,
    pub redacted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemSearchQuery {
    pub text: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub project_id: Option<String>,
    pub goal_id: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub limit: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub goal_id: Option<String>,
    pub deadline: Option<DateTime<Utc>>,
    pub scheduled_start: Option<DateTime<Utc>>,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ItemSearchResult {
    pub revision: String,
    pub items: Vec<ItemSummary>,
    pub redacted_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ItemSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub goal_id: Option<String>,
    pub deadline: Option<DateTime<Utc>>,
    pub scheduled_start: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementExplanation {
    pub block_id: String,
    pub summary: String,
    pub reasons: Vec<PlacementReason>,
    pub active_constraints: Vec<String>,
    pub alternatives: Vec<PlacementAlternative>,
    pub stability_cost: u64,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementReason {
    pub code: String,
    pub message: String,
    pub strength: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementAlternative {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub tradeoff: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictQuery {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConflictReport {
    pub revision: String,
    pub conflicts: Vec<ScheduleConflict>,
    pub redacted_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScheduleConflict {
    pub id: String,
    pub kind: String,
    pub severity: String,
    pub message: String,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub related_item_ids: Vec<String>,
    pub penalty: u64,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanOperation {
    pub kind: PlanOperationKind,
    pub target_id: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanOperationKind {
    CreateItem,
    UpdateItem,
    MoveBlock,
    CompleteItem,
    DeleteItem,
    UpdateConstraint,
    CreateEvent,
    GoalBreakdown,
    ReplaceSchedule,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SimulationRequest {
    pub base_revision: String,
    pub operations: Vec<PlanOperation>,
    pub assumptions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SimulationResult {
    pub simulation_token: String,
    pub request_digest: String,
    pub base_revision: String,
    /// Whether the exact simulated request can become a typed proposal that an
    /// authorized `DayWeave` device may preview and apply.
    pub application_ready: bool,
    /// The executable payload schema, when [`Self::application_ready`] is true.
    pub change_set_schema: Option<String>,
    pub moved_blocks: Vec<SimulatedBlockMove>,
    pub unscheduled_item_ids: Vec<String>,
    pub violations: Vec<SimulationIssue>,
    pub warnings: Vec<SimulationIssue>,
}

/// Server-only evidence produced with a simulation and consumed when an MCP
/// proposal is submitted. This value is persisted inside the hidden simulation
/// snapshot, never serialized into an MCP response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationProposalEvidence {
    schema_version: u8,
    proposal_kind: Option<ProposalKind>,
    change_set: Option<ProposalChangeSet>,
    manual_review_reasons: Vec<String>,
}

impl SimulationProposalEvidence {
    const SCHEMA_VERSION: u8 = 1;

    #[must_use]
    pub fn actionable(proposal_kind: ProposalKind, change_set: ProposalChangeSet) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            proposal_kind: Some(proposal_kind),
            change_set: Some(change_set),
            manual_review_reasons: Vec::new(),
        }
    }

    #[must_use]
    pub fn manual_review(reasons: Vec<String>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            proposal_kind: None,
            change_set: None,
            manual_review_reasons: reasons,
        }
    }

    #[must_use]
    pub fn change_set(&self) -> Option<&ProposalChangeSet> {
        self.change_set.as_ref()
    }

    #[must_use]
    pub const fn proposal_kind(&self) -> Option<ProposalKind> {
        self.proposal_kind
    }

    #[must_use]
    pub fn manual_review_reasons(&self) -> &[String] {
        &self.manual_review_reasons
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        let shape_valid = match &self.change_set {
            Some(change_set) => {
                self.proposal_kind.is_some()
                    && self.manual_review_reasons.is_empty()
                    && change_set.validate().is_ok()
            }
            None => {
                self.proposal_kind.is_none()
                    && !self.manual_review_reasons.is_empty()
                    && self.manual_review_reasons.len() <= 100
                    && self.manual_review_reasons.iter().all(|reason| {
                        !reason.trim().is_empty()
                            && reason.len() <= 100
                            && reason.bytes().all(|byte| {
                                byte.is_ascii_lowercase()
                                    || byte.is_ascii_digit()
                                    || matches!(byte, b'_' | b'-')
                            })
                    })
            }
        };
        self.schema_version == Self::SCHEMA_VERSION
            && shape_valid
            && self
                .proposal_kind
                .zip(self.change_set.as_ref())
                .is_none_or(|(kind, change_set)| proposal_kind_matches_change_set(kind, change_set))
    }
}

pub(crate) fn proposal_kind_matches_change_set(
    kind: ProposalKind,
    change_set: &ProposalChangeSet,
) -> bool {
    match kind {
        ProposalKind::CreateItem => {
            change_set.commands.len() == 1
                && matches!(
                    change_set.commands.first(),
                    Some(ProposalCommand::CreateItem { .. })
                )
        }
        ProposalKind::CalendarEvent => change_set.commands.iter().all(|command| {
            matches!(
                command,
                ProposalCommand::CreateItem { item, .. }
                    if item.kind == crate::items::ItemKind::Event
            )
        }),
        ProposalKind::UpdateItem => change_set.commands.iter().all(|command| {
            matches!(
                command,
                ProposalCommand::ReplaceItem { .. }
                    | ProposalCommand::TrashItem { .. }
                    | ProposalCommand::RestoreItem { .. }
            )
        }),
        ProposalKind::ConstraintChange => change_set
            .commands
            .iter()
            .all(|command| matches!(command, ProposalCommand::ReplaceItem { .. })),
        ProposalKind::GoalBreakdown => change_set
            .commands
            .iter()
            .all(|command| matches!(command, ProposalCommand::CreateItem { .. })),
        ProposalKind::SchedulePlan | ProposalKind::Recommendation => false,
    }
}

/// Atomic result of consuming a single-use simulation token.
#[derive(Clone, Debug, PartialEq)]
pub struct SimulationConsumption {
    pub result: SimulationResult,
    pub proposal_evidence: SimulationProposalEvidence,
    /// Durable database proof copied into an immutable MCP submission receipt.
    /// Deterministic in-memory adapters do not provide persistence proof.
    pub persistence_proof: Option<SimulationPersistenceProof>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulationPersistenceProof {
    pub simulation_id: Uuid,
    pub subject_hash: [u8; 32],
    pub request_digest: [u8; 16],
    pub request_hash: [u8; 32],
    pub base_revision_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub evidence_schema: i16,
    pub evidence_hash: [u8; 32],
    pub compilation_outcome: String,
    pub compiled_payload_hash: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SimulatedBlockMove {
    pub block_id: String,
    pub previous_start: DateTime<Utc>,
    pub previous_end: DateTime<Utc>,
    pub proposed_start: DateTime<Utc>,
    pub proposed_end: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SimulationIssue {
    pub code: String,
    pub message: String,
    pub related_ids: Vec<String>,
}

#[async_trait]
pub trait ScheduleQueryPort: Send + Sync {
    async fn get_schedule(
        &self,
        access: &ScheduleAccess,
        query: ScheduleQuery,
    ) -> Result<ScheduleView, SchedulingPortError>;

    async fn search_items(
        &self,
        access: &ScheduleAccess,
        query: ItemSearchQuery,
    ) -> Result<ItemSearchResult, SchedulingPortError>;

    async fn explain_placement(
        &self,
        access: &ScheduleAccess,
        block_id: &str,
    ) -> Result<PlacementExplanation, SchedulingPortError>;

    async fn get_conflicts(
        &self,
        access: &ScheduleAccess,
        query: ConflictQuery,
    ) -> Result<ConflictReport, SchedulingPortError>;
}

#[async_trait]
pub trait PlanningSimulationPort: Send + Sync {
    async fn simulate(
        &self,
        access: &ScheduleAccess,
        request: SimulationRequest,
    ) -> Result<SimulationResult, SchedulingPortError>;

    async fn consume_simulation(
        &self,
        access: &ScheduleAccess,
        token: &str,
        expected_request_digest: &str,
    ) -> Result<SimulationConsumption, SchedulingPortError>;
}

#[derive(Clone, Debug)]
pub struct ProposalSubmissionSpec {
    pub idempotency_key: String,
    pub request_fingerprint: [u8; 32],
    pub simulation_token: String,
    pub request: SimulationRequest,
    pub title: String,
    pub explanation: String,
    pub source_conversation_label: String,
    pub source_client_label: Option<String>,
    pub source_request_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ProposalSubmissionResult {
    pub proposal: Proposal,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProposalSubmissionError {
    #[error("the authenticated principal does not own this proposal scope")]
    AccessDenied,
    #[error("the idempotency key was already used for different proposal content")]
    IdempotencyConflict,
    #[error(transparent)]
    Simulation(#[from] SchedulingPortError),
    #[error("proposal submission storage is temporarily unavailable")]
    Unavailable,
}

#[async_trait]
pub trait ProposalSubmissionPort: Send + Sync {
    async fn submit_proposal(
        &self,
        access: &ScheduleAccess,
        spec: ProposalSubmissionSpec,
    ) -> Result<ProposalSubmissionResult, ProposalSubmissionError>;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SchedulingPortError {
    #[error("invalid scheduling query: {0}")]
    InvalidQuery(String),
    #[error("schedule object was not found")]
    NotFound,
    #[error("schedule revision changed; current revision is {current_revision}")]
    RevisionConflict { current_revision: String },
    #[error("the published schedule predates durable evidence; publish a fresh schedule first")]
    RepublishRequired,
    #[error("scheduling data is temporarily unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableScheduleQueryPort;

#[async_trait]
impl ScheduleQueryPort for UnavailableScheduleQueryPort {
    async fn get_schedule(
        &self,
        _access: &ScheduleAccess,
        _query: ScheduleQuery,
    ) -> Result<ScheduleView, SchedulingPortError> {
        Err(unavailable())
    }

    async fn search_items(
        &self,
        _access: &ScheduleAccess,
        _query: ItemSearchQuery,
    ) -> Result<ItemSearchResult, SchedulingPortError> {
        Err(unavailable())
    }

    async fn explain_placement(
        &self,
        _access: &ScheduleAccess,
        _block_id: &str,
    ) -> Result<PlacementExplanation, SchedulingPortError> {
        Err(unavailable())
    }

    async fn get_conflicts(
        &self,
        _access: &ScheduleAccess,
        _query: ConflictQuery,
    ) -> Result<ConflictReport, SchedulingPortError> {
        Err(unavailable())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableSimulationPort;

#[async_trait]
impl PlanningSimulationPort for UnavailableSimulationPort {
    async fn simulate(
        &self,
        _access: &ScheduleAccess,
        _request: SimulationRequest,
    ) -> Result<SimulationResult, SchedulingPortError> {
        Err(unavailable())
    }

    async fn consume_simulation(
        &self,
        _access: &ScheduleAccess,
        _token: &str,
        _expected_request_digest: &str,
    ) -> Result<SimulationConsumption, SchedulingPortError> {
        Err(unavailable())
    }
}

fn unavailable() -> SchedulingPortError {
    SchedulingPortError::Unavailable(
        "the canonical schedule adapter has not been configured".to_owned(),
    )
}
