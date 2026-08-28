use std::{collections::HashMap, fmt::Write, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    auth::{Principal, Scope},
    proposals::{NewProposal, ProposalKind, ProposalService, ProposalSource},
    scheduling::{
        ConflictQuery, ItemSearchQuery, PlanOperation, PlanOperationKind, PlanningSimulationPort,
        ScheduleAccess, ScheduleDetail, ScheduleQuery, ScheduleQueryPort, SchedulingPortError,
        SimulationRequest, simulation_request_digest,
    },
};

const TOOL_CACHE_TTL_MS: u64 = 5 * 60 * 1_000;
const MAX_OPERATIONS: usize = 100;
const MAX_ASSUMPTIONS: usize = 20;

#[derive(Clone, Debug)]
pub struct McpRequestContext {
    pub principal: Principal,
    pub request_id: String,
    pub client_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolOutput {
    pub summary: String,
    pub structured: Value,
}

#[derive(Clone)]
pub struct McpService {
    pub(crate) schedule: Arc<dyn ScheduleQueryPort>,
    pub(crate) simulations: Arc<dyn PlanningSimulationPort>,
    proposals: Arc<ProposalService>,
    submissions: Arc<Mutex<HashMap<(String, String), SubmissionRecord>>>,
    allowed_origins: Arc<Vec<String>>,
}

impl std::fmt::Debug for McpService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpService")
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

impl McpService {
    #[must_use]
    pub fn new(
        schedule: Arc<dyn ScheduleQueryPort>,
        simulations: Arc<dyn PlanningSimulationPort>,
        proposals: Arc<ProposalService>,
        allowed_origins: Arc<Vec<String>>,
    ) -> Self {
        Self {
            schedule,
            simulations,
            proposals,
            submissions: Arc::new(Mutex::new(HashMap::new())),
            allowed_origins,
        }
    }

    #[must_use]
    pub fn is_origin_allowed(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|allowed| allowed == origin)
    }

    #[must_use]
    pub fn tool_catalog(&self, principal: &Principal, modern: bool) -> Value {
        let tools = tool_definitions(principal);
        if modern {
            json!({
                "resultType": "complete",
                "tools": tools,
                "ttlMs": TOOL_CACHE_TTL_MS,
                "cacheScope": "private",
            })
        } else {
            json!({ "tools": tools })
        }
    }

    /// Invokes a permission-filtered MCP tool.
    ///
    /// # Errors
    ///
    /// Returns [`ToolCallError`] for hidden/unknown tools, malformed arguments,
    /// query failures, or proposal validation and persistence failures.
    #[allow(clippy::too_many_lines)]
    pub async fn call_tool(
        &self,
        context: &McpRequestContext,
        name: &str,
        arguments: Value,
    ) -> Result<ToolOutput, ToolCallError> {
        if !tool_is_visible(&context.principal, name) {
            return Err(ToolCallError::UnknownTool(name.to_owned()));
        }
        let access = ScheduleAccess {
            subject: context.principal.subject.clone(),
            include_sensitive: false,
        };
        match name {
            "get_schedule" => {
                let input: GetScheduleInput = decode(arguments)?;
                validate_range(input.start, input.end)?;
                let result = self
                    .schedule
                    .get_schedule(
                        &access,
                        ScheduleQuery {
                            start: input.start,
                            end: input.end,
                            detail: input.detail,
                        },
                    )
                    .await
                    .map_err(ToolCallError::from_port)?;
                output("Schedule read without changing any item.", &result)
            }
            "search_items" => {
                let input: SearchItemsInput = decode(arguments)?;
                if let (Some(start), Some(end)) = (input.start, input.end) {
                    validate_range(start, end)?;
                }
                let result = self
                    .schedule
                    .search_items(
                        &access,
                        ItemSearchQuery {
                            text: input.text,
                            status: input.status,
                            kind: input.kind,
                            project_id: input.project_id,
                            goal_id: input.goal_id,
                            start: input.start,
                            end: input.end,
                            limit: input.limit.unwrap_or(20),
                        },
                    )
                    .await
                    .map_err(ToolCallError::from_port)?;
                output("Items matched without changing schedule state.", &result)
            }
            "explain_placement" => {
                let input: ExplainPlacementInput = decode(arguments)?;
                let result = self
                    .schedule
                    .explain_placement(&access, &input.block_id)
                    .await
                    .map_err(ToolCallError::from_port)?;
                output(
                    "Placement explanation read from scheduler evidence.",
                    &result,
                )
            }
            "get_conflicts" => {
                let input: GetConflictsInput = decode(arguments)?;
                validate_range(input.start, input.end)?;
                let result = self
                    .schedule
                    .get_conflicts(
                        &access,
                        ConflictQuery {
                            start: input.start,
                            end: input.end,
                        },
                    )
                    .await
                    .map_err(ToolCallError::from_port)?;
                output(
                    "Conflict report read without changing schedule state.",
                    &result,
                )
            }
            "simulate_plan" => {
                let input: SimulatePlanInput = decode(arguments)?;
                validate_plan_contents(&input.operations, &input.assumptions)?;
                let result = self
                    .simulations
                    .simulate(
                        &access,
                        SimulationRequest {
                            base_revision: input.base_revision,
                            operations: input.operations,
                            assumptions: input.assumptions,
                        },
                    )
                    .await
                    .map_err(ToolCallError::from_port)?;
                output(
                    "What-if simulation completed. Canonical schedule state was not changed.",
                    &result,
                )
            }
            "submit_proposal" => {
                let input: SubmitProposalInput = decode(arguments)?;
                self.submit_proposal(context, &access, input).await
            }
            _ => Err(ToolCallError::UnknownTool(name.to_owned())),
        }
    }

    async fn submit_proposal(
        &self,
        context: &McpRequestContext,
        access: &ScheduleAccess,
        input: SubmitProposalInput,
    ) -> Result<ToolOutput, ToolCallError> {
        let maximum_expiration = self.proposals.default_expiration();
        let expires_at = validate_submission(&input, maximum_expiration)?;
        let request_fingerprint = submission_fingerprint(&input)?;

        let mut submissions = self.submissions.lock().await;
        let idempotency_scope = (
            context.principal.subject.clone(),
            input.idempotency_key.clone(),
        );
        if let Some(record) = submissions.get(&idempotency_scope) {
            if record.request_fingerprint != request_fingerprint {
                return Err(ToolCallError::execution(
                    "idempotency_conflict",
                    "idempotency_key was already used for different proposal content",
                ));
            }
            let proposal = self
                .proposals
                .get(record.proposal_id)
                .await
                .map_err(|error| {
                    ToolCallError::execution("proposal_unavailable", error.to_string())
                })?;
            return proposal_output(&proposal, true);
        }

        if let Some(simulation_token) = input.simulation_token.as_deref() {
            let expected_digest = simulation_request_digest(&SimulationRequest {
                base_revision: input.base_revision.clone(),
                operations: input.operations.clone(),
                assumptions: input.assumptions.clone(),
            })
            .map_err(ToolCallError::from_port)?;
            let simulation = self
                .simulations
                .consume_simulation(access, simulation_token, &expected_digest)
                .await
                .map_err(ToolCallError::from_port)?;
            if simulation.base_revision != input.base_revision
                || simulation.request_digest != expected_digest
            {
                return Err(ToolCallError::execution(
                    "simulation_mismatch",
                    "simulation token does not match the proposal base revision",
                ));
            }
        }

        let proposal_kind = proposal_kind(&input.operations);
        let payload = json!({
            "schema_version": 1,
            "idempotency_key": input.idempotency_key,
            "base_revision": input.base_revision,
            "simulation_token": input.simulation_token,
            "assumptions": input.assumptions,
            "operations": input.operations,
            "source": {
                "client": context.client_name,
                "conversation": input.source_conversation_label,
                "request_id": context.request_id,
            },
            "safety": {
                "proposal_only": true,
                "requires_app_review": true,
                "canonical_state_mutated": false,
            }
        });
        let proposal = self
            .proposals
            .create(NewProposal {
                submitted_by: context.principal.subject.clone(),
                source: ProposalSource::ExternalMcp,
                source_reference: Some(input.source_conversation_label),
                kind: proposal_kind,
                title: input.title,
                explanation: Some(input.explanation),
                payload,
                expires_at,
            })
            .await
            .map_err(|error| ToolCallError::execution("proposal_rejected", error.to_string()))?;
        submissions.insert(
            idempotency_scope,
            SubmissionRecord {
                proposal_id: proposal.id,
                request_fingerprint,
            },
        );
        proposal_output(&proposal, false)
    }
}

#[derive(Clone, Debug)]
pub enum ToolCallError {
    UnknownTool(String),
    Execution {
        code: &'static str,
        message: String,
        details: Option<Value>,
    },
}

// A manual Display implementation avoids exposing transport details through a
// generic error derive while keeping call sites concise.
impl std::fmt::Display for ToolCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(formatter, "unknown tool: {name}"),
            Self::Execution { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ToolCallError {}

impl ToolCallError {
    fn execution(code: &'static str, message: impl Into<String>) -> Self {
        Self::Execution {
            code,
            message: message.into(),
            details: None,
        }
    }

    fn from_port(error: SchedulingPortError) -> Self {
        match error {
            SchedulingPortError::InvalidQuery(message) => {
                Self::execution("invalid_arguments", message)
            }
            SchedulingPortError::NotFound => {
                Self::execution("not_found", "The requested schedule object was not found")
            }
            SchedulingPortError::RevisionConflict { current_revision } => Self::Execution {
                code: "revision_conflict",
                message: "The schedule changed; simulate again from the current revision"
                    .to_owned(),
                details: Some(json!({ "current_revision": current_revision })),
            },
            SchedulingPortError::Unavailable(message) => {
                Self::execution("temporarily_unavailable", message)
            }
        }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::UnknownTool(_) => "unknown_tool",
            Self::Execution { code, .. } => code,
        }
    }

    #[must_use]
    pub fn details(&self) -> Option<&Value> {
        match self {
            Self::UnknownTool(_) => None,
            Self::Execution { details, .. } => details.as_ref(),
        }
    }

    #[must_use]
    pub const fn is_unknown_tool(&self) -> bool {
        matches!(self, Self::UnknownTool(_))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetScheduleInput {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    detail: ScheduleDetail,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchItemsInput {
    text: Option<String>,
    status: Option<String>,
    kind: Option<String>,
    project_id: Option<String>,
    goal_id: Option<String>,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplainPlacementInput {
    block_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetConflictsInput {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SimulatePlanInput {
    base_revision: String,
    operations: Vec<PlanOperation>,
    #[serde(default)]
    assumptions: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SubmitProposalInput {
    idempotency_key: String,
    title: String,
    explanation: String,
    source_conversation_label: String,
    base_revision: String,
    simulation_token: Option<String>,
    operations: Vec<PlanOperation>,
    #[serde(default)]
    assumptions: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct SubmissionRecord {
    proposal_id: Uuid,
    request_fingerprint: String,
}

fn decode<T: DeserializeOwned>(arguments: Value) -> Result<T, ToolCallError> {
    serde_json::from_value(arguments).map_err(|error| {
        ToolCallError::execution(
            "invalid_arguments",
            format!("invalid tool arguments: {error}"),
        )
    })
}

fn validate_range(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<(), ToolCallError> {
    if end <= start {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            "end must be after start",
        ));
    }
    if end - start > chrono::Duration::days(90) {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            "date range must not exceed 90 days",
        ));
    }
    Ok(())
}

fn validate_plan_contents(
    operations: &[PlanOperation],
    assumptions: &[String],
) -> Result<(), ToolCallError> {
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            format!("operations must contain between 1 and {MAX_OPERATIONS} entries"),
        ));
    }
    if assumptions.len() > MAX_ASSUMPTIONS
        || assumptions
            .iter()
            .any(|assumption| assumption.chars().count() > 500)
    {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            format!("at most {MAX_ASSUMPTIONS} assumptions of 500 characters are allowed"),
        ));
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), ToolCallError> {
    let valid = (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if valid {
        Ok(())
    } else {
        Err(ToolCallError::execution(
            "invalid_arguments",
            "idempotency_key must be 8-128 ASCII letters, digits, '.', '_', ':' or '-'",
        ))
    }
}

fn validate_submission(
    input: &SubmitProposalInput,
    maximum_expiration: DateTime<Utc>,
) -> Result<DateTime<Utc>, ToolCallError> {
    validate_idempotency_key(&input.idempotency_key)?;
    validate_plan_contents(&input.operations, &input.assumptions)?;
    validate_bounded_text(&input.title, "title", 200)?;
    validate_bounded_text(&input.explanation, "explanation", 4_000)?;
    validate_bounded_text(
        &input.source_conversation_label,
        "source_conversation_label",
        500,
    )?;
    validate_bounded_text(&input.base_revision, "base_revision", 200)?;

    let expires_at = input.expires_at.unwrap_or(maximum_expiration);
    if expires_at > maximum_expiration {
        return Err(ToolCallError::execution(
            "invalid_expiration",
            "proposal expiry exceeds the server maximum",
        ));
    }
    Ok(expires_at)
}

fn validate_bounded_text(
    value: &str,
    field: &str,
    maximum_length: usize,
) -> Result<(), ToolCallError> {
    if value.trim().is_empty() || value.chars().count() > maximum_length {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            format!("{field} must contain between 1 and {maximum_length} characters"),
        ));
    }
    Ok(())
}

fn submission_fingerprint(input: &SubmitProposalInput) -> Result<String, ToolCallError> {
    let bytes = serde_json::to_vec(input).map_err(|_| {
        ToolCallError::execution(
            "encoding_failed",
            "proposal content could not be fingerprinted",
        )
    })?;
    let digest = Sha256::digest(bytes);
    Ok(digest[..16]
        .iter()
        .fold(String::with_capacity(32), |mut encoded, byte| {
            let _ = write!(encoded, "{byte:02x}");
            encoded
        }))
}

fn proposal_kind(operations: &[PlanOperation]) -> ProposalKind {
    if operations
        .iter()
        .all(|operation| operation.kind == PlanOperationKind::GoalBreakdown)
    {
        ProposalKind::GoalBreakdown
    } else if operations
        .iter()
        .all(|operation| operation.kind == PlanOperationKind::UpdateConstraint)
    {
        ProposalKind::ConstraintChange
    } else if operations
        .iter()
        .all(|operation| operation.kind == PlanOperationKind::CreateEvent)
    {
        ProposalKind::CalendarEvent
    } else {
        ProposalKind::SchedulePlan
    }
}

fn proposal_output(
    proposal: &crate::proposals::Proposal,
    duplicate: bool,
) -> Result<ToolOutput, ToolCallError> {
    output(
        if duplicate {
            "This idempotency key already submitted the same Suggestions Inbox proposal."
        } else {
            "Proposal saved to the Suggestions Inbox. No canonical schedule state was changed."
        },
        &json!({
            "proposal_id": proposal.id,
            "status": proposal.status,
            "revision": proposal.revision,
            "expires_at": proposal.expires_at,
            "duplicate": duplicate,
            "canonical_state_mutated": false,
            "review_required": true,
        }),
    )
}

fn output(summary: &str, value: &impl Serialize) -> Result<ToolOutput, ToolCallError> {
    Ok(ToolOutput {
        summary: summary.to_owned(),
        structured: serde_json::to_value(value).map_err(|_| {
            ToolCallError::execution("encoding_failed", "tool result could not be encoded")
        })?,
    })
}

fn tool_is_visible(principal: &Principal, name: &str) -> bool {
    match name {
        "get_schedule" | "search_items" | "explain_placement" | "get_conflicts" => {
            principal.has_scope(Scope::ScheduleRead)
        }
        "simulate_plan" => principal.has_scope(Scope::ScheduleSimulate),
        "submit_proposal" => principal.has_scope(Scope::SuggestionsSubmit),
        _ => false,
    }
}

#[must_use]
pub fn requires_idempotency_header(name: &str) -> bool {
    name == "submit_proposal"
}

fn tool_definitions(principal: &Principal) -> Vec<Value> {
    let mut tools = Vec::new();
    if principal.has_scope(Scope::ScheduleRead) {
        tools.extend([
            tool(
                "get_schedule",
                "Get schedule",
                "Read a bounded schedule interval. Sensitive content is redacted by server policy. This tool never changes schedule state.",
                &interval_schema(true),
                &read_annotations(),
            ),
            tool(
                "search_items",
                "Search items",
                "Find non-sensitive tasks, habits, routines, goals, breaks, or events by text and filters without changing them.",
                &search_schema(),
                &read_annotations(),
            ),
            tool(
                "explain_placement",
                "Explain placement",
                "Read optimizer reasons, constraints, alternatives, and stability cost for one scheduled block. Never invents a reason.",
                &object_schema(
                    &json!({ "block_id": string_schema("Scheduled block identifier") }),
                    &["block_id"],
                ),
                &read_annotations(),
            ),
            tool(
                "get_conflicts",
                "Get conflicts",
                "Read hard violations, soft penalties, overload, and deadline risk in a bounded interval without changing the schedule.",
                &interval_schema(false),
                &read_annotations(),
            ),
        ]);
    }
    if principal.has_scope(Scope::ScheduleSimulate) {
        tools.push(tool(
            "simulate_plan",
            "Simulate plan",
            "Run a side-effect-free what-if plan against a specific schedule revision. Returns an explicit simulation token; canonical state is never changed.",
            &simulation_schema(),
            &read_annotations(),
        ));
    }
    if principal.has_scope(Scope::SuggestionsSubmit) {
        tools.push(tool(
            "submit_proposal",
            "Submit proposal",
            "Create only a reviewable Suggestions Inbox proposal. It never creates, edits, moves, completes, deletes, RSVPs, or publishes canonical items. The user must review it in DayWeave.",
            &proposal_schema(),
            &json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            }),
        ));
    }
    tools
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    input_schema: &Value,
    annotations: &Value,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input_schema,
        "outputSchema": {
            "type": "object",
            "additionalProperties": true
        },
        "annotations": annotations,
    })
}

fn read_annotations() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

fn interval_schema(include_detail: bool) -> Value {
    let mut properties = serde_json::Map::from_iter([
        (
            "start".to_owned(),
            string_schema("RFC 3339 interval start, including offset"),
        ),
        (
            "end".to_owned(),
            string_schema("RFC 3339 interval end, including offset; maximum range is 90 days"),
        ),
    ]);
    let mut required = vec!["start", "end"];
    if include_detail {
        properties.insert(
            "detail".to_owned(),
            json!({
                "type": "string",
                "enum": ["busy_only", "summary", "full"],
                "description": "Requested detail; server privacy policy may further redact output"
            }),
        );
        required.push("detail");
    }
    object_schema(&Value::Object(properties), &required)
}

fn search_schema() -> Value {
    object_schema(
        &json!({
            "text": { "type": "string" },
            "status": { "type": "string" },
            "kind": { "type": "string" },
            "project_id": { "type": "string" },
            "goal_id": { "type": "string" },
            "start": string_schema("Optional RFC 3339 scheduled-start lower bound"),
            "end": string_schema("Optional RFC 3339 scheduled-start upper bound"),
            "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
        }),
        &[],
    )
}

fn simulation_schema() -> Value {
    object_schema(
        &json!({
            "base_revision": string_schema("Schedule revision returned by a read tool"),
            "operations": operation_array_schema(),
            "assumptions": {
                "type": "array",
                "maxItems": MAX_ASSUMPTIONS,
                "items": { "type": "string", "maxLength": 500 }
            }
        }),
        &["base_revision", "operations"],
    )
}

fn proposal_schema() -> Value {
    object_schema(
        &json!({
            "idempotency_key": {
                "type": "string",
                "minLength": 8,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9._:-]+$",
                "x-mcp-header": "Idempotency-Key",
                "description": "Stable client-generated key; retries return the existing Inbox proposal"
            },
            "title": { "type": "string", "minLength": 1, "maxLength": 200 },
            "explanation": { "type": "string", "minLength": 1, "maxLength": 4000 },
            "source_conversation_label": { "type": "string", "minLength": 1, "maxLength": 500 },
            "base_revision": string_schema("Schedule revision used to formulate the proposal"),
            "simulation_token": { "type": "string", "description": "Optional single-use token from simulate_plan" },
            "operations": operation_array_schema(),
            "assumptions": {
                "type": "array",
                "maxItems": MAX_ASSUMPTIONS,
                "items": { "type": "string", "maxLength": 500 }
            },
            "expires_at": string_schema("Optional RFC 3339 expiry not exceeding the server maximum")
        }),
        &[
            "idempotency_key",
            "title",
            "explanation",
            "source_conversation_label",
            "base_revision",
            "operations",
        ],
    )
}

fn operation_array_schema() -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "maxItems": MAX_OPERATIONS,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": [
                        "create_item", "update_item", "move_block", "complete_item",
                        "delete_item", "update_constraint", "create_event",
                        "goal_breakdown", "replace_schedule"
                    ]
                },
                "target_id": { "type": "string" },
                "parameters": { "type": "object" }
            },
            "required": ["kind"]
        }
    })
}

fn object_schema(properties: &Value, required: &[&str]) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn string_schema(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}
