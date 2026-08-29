use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::proposals::Proposal;

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
    pub moved_blocks: Vec<SimulatedBlockMove>,
    pub unscheduled_item_ids: Vec<String>,
    pub violations: Vec<SimulationIssue>,
    pub warnings: Vec<SimulationIssue>,
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
    ) -> Result<SimulationResult, SchedulingPortError>;
}

#[derive(Clone, Debug)]
pub struct ProposalSubmissionSpec {
    pub idempotency_key: String,
    pub request_fingerprint: [u8; 32],
    pub expected_simulation_digest: String,
    pub simulation_token: Option<String>,
    pub proposal: Proposal,
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
    ) -> Result<SimulationResult, SchedulingPortError> {
        Err(unavailable())
    }
}

fn unavailable() -> SchedulingPortError {
    SchedulingPortError::Unavailable(
        "the canonical schedule adapter has not been configured".to_owned(),
    )
}
