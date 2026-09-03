use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    auth::{Principal, PrincipalAudience, Scope},
    mcp_oauth::{
        SCOPE_SCHEDULE_READ as SCOPE_FOR_READ, SCOPE_SCHEDULE_SIMULATE as SCOPE_FOR_SIMULATE,
        SCOPE_SUGGESTIONS_SUBMIT as SCOPE_FOR_SUBMIT, scope_name,
    },
    proposals::{PROPOSAL_CHANGE_SET_SCHEMA_V1, ProposalChangeSet, ProposalService},
    scheduling::{
        ConflictQuery, ItemSearchQuery, PlanOperation, PlanningSimulationPort,
        ProposalSubmissionError, ProposalSubmissionPort, ProposalSubmissionResult,
        ProposalSubmissionSpec, ScheduleAccess, ScheduleDetail, ScheduleQuery, ScheduleQueryPort,
        SchedulingPortError, SimulationRequest, has_postgres_timestamp_precision,
        materialize_proposal, proposal_kind_matches_change_set, simulation_request_digest,
        truncate_to_postgres_timestamp_precision,
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
    submissions: Arc<dyn ProposalSubmissionPort>,
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
        let submissions = Arc::new(InMemoryProposalSubmissionPort::new(
            simulations.clone(),
            proposals.clone(),
        ));
        Self::new_with_submissions(
            schedule,
            simulations,
            proposals,
            submissions,
            allowed_origins,
        )
    }

    #[must_use]
    pub fn new_with_submissions(
        schedule: Arc<dyn ScheduleQueryPort>,
        simulations: Arc<dyn PlanningSimulationPort>,
        proposals: Arc<ProposalService>,
        submissions: Arc<dyn ProposalSubmissionPort>,
        allowed_origins: Arc<Vec<String>>,
    ) -> Self {
        Self {
            schedule,
            simulations,
            proposals,
            submissions,
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
        self.authorize_tool(&context.principal, name)?;
        let access = ScheduleAccess {
            subject: context.principal.subject.clone(),
            include_sensitive: false,
            workspace_id: context.principal.workspace_id,
            user_id: context.principal.user_id,
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
                validate_optional_instant(input.start)?;
                validate_optional_instant(input.end)?;
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

    /// Performs tool authorization before argument processing or any port call.
    ///
    /// # Errors
    ///
    /// Returns an unknown-tool error for unknown or native-hidden tools, or an
    /// insufficient-scope error for a known OAuth tool needing step-up consent.
    pub fn authorize_tool(&self, principal: &Principal, name: &str) -> Result<(), ToolCallError> {
        let Some(required) = required_tool_scope(name) else {
            return Err(ToolCallError::UnknownTool(name.to_owned()));
        };
        if principal.has_scope(required) {
            return Ok(());
        }
        if principal.audience == PrincipalAudience::McpOAuth {
            let Some(scope) = scope_name(required) else {
                return Err(ToolCallError::UnknownTool(name.to_owned()));
            };
            return Err(ToolCallError::InsufficientScope { scope });
        }
        Err(ToolCallError::UnknownTool(name.to_owned()))
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

        let simulation_token = input.simulation_token.clone();
        let request = SimulationRequest {
            base_revision: input.base_revision,
            operations: input.operations,
            assumptions: input.assumptions,
        };
        let result = self
            .submissions
            .submit_proposal(
                access,
                ProposalSubmissionSpec {
                    idempotency_key: input.idempotency_key,
                    request_fingerprint,
                    simulation_token,
                    request,
                    title: input.title,
                    explanation: input.explanation,
                    source_conversation_label: input.source_conversation_label,
                    source_client_label: context.client_name.clone(),
                    source_request_id: context.request_id.clone(),
                    expires_at,
                },
            )
            .await
            .map_err(ToolCallError::from_submission)?;
        proposal_output(&result.proposal, result.duplicate)
    }
}

#[derive(Clone, Debug)]
pub enum ToolCallError {
    UnknownTool(String),
    InsufficientScope {
        scope: &'static str,
    },
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
            Self::InsufficientScope { scope } => {
                write!(formatter, "additional OAuth scope is required: {scope}")
            }
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
            SchedulingPortError::RepublishRequired => Self::execution(
                "republish_required",
                "This schedule predates durable planning evidence; publish a fresh schedule in DayWeave first",
            ),
            SchedulingPortError::Unavailable(message) => {
                Self::execution("temporarily_unavailable", message)
            }
        }
    }

    fn from_submission(error: ProposalSubmissionError) -> Self {
        match error {
            ProposalSubmissionError::AccessDenied => {
                Self::execution("not_found", "The proposal submission scope was not found")
            }
            ProposalSubmissionError::IdempotencyConflict => Self::execution(
                "idempotency_conflict",
                "idempotency_key was already used for different proposal content",
            ),
            ProposalSubmissionError::Simulation(error) => Self::from_port(error),
            ProposalSubmissionError::Unavailable => Self::execution(
                "proposal_unavailable",
                "proposal submission storage is temporarily unavailable",
            ),
        }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::UnknownTool(_) => "unknown_tool",
            Self::InsufficientScope { .. } => "insufficient_scope",
            Self::Execution { code, .. } => code,
        }
    }

    #[must_use]
    pub fn details(&self) -> Option<&Value> {
        match self {
            Self::UnknownTool(_) | Self::InsufficientScope { .. } => None,
            Self::Execution { details, .. } => details.as_ref(),
        }
    }

    #[must_use]
    pub const fn is_unknown_tool(&self) -> bool {
        matches!(self, Self::UnknownTool(_))
    }

    #[must_use]
    pub const fn insufficient_scope(&self) -> Option<&'static str> {
        match self {
            Self::InsufficientScope { scope } => Some(scope),
            _ => None,
        }
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
    simulation_token: String,
    operations: Vec<PlanOperation>,
    #[serde(default)]
    assumptions: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct SubmissionRecord {
    proposal: crate::proposals::Proposal,
    request_fingerprint: [u8; 32],
}

/// Deterministic test/local adapter. Production wiring replaces this with the
/// `PostgreSQL` transaction port; this adapter intentionally makes no restart or
/// cross-process durability claim.
struct InMemoryProposalSubmissionPort {
    simulations: Arc<dyn PlanningSimulationPort>,
    proposals: Arc<ProposalService>,
    submissions: Mutex<HashMap<(String, String), SubmissionRecord>>,
}

impl InMemoryProposalSubmissionPort {
    fn new(simulations: Arc<dyn PlanningSimulationPort>, proposals: Arc<ProposalService>) -> Self {
        Self {
            simulations,
            proposals,
            submissions: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ProposalSubmissionPort for InMemoryProposalSubmissionPort {
    async fn submit_proposal(
        &self,
        access: &ScheduleAccess,
        spec: ProposalSubmissionSpec,
    ) -> Result<ProposalSubmissionResult, ProposalSubmissionError> {
        let key = (access.subject.clone(), spec.idempotency_key.clone());
        let mut submissions = self.submissions.lock().await;
        if let Some(existing) = submissions.get(&key) {
            if existing.request_fingerprint != spec.request_fingerprint {
                return Err(ProposalSubmissionError::IdempotencyConflict);
            }
            return Ok(ProposalSubmissionResult {
                proposal: existing.proposal.clone(),
                duplicate: true,
            });
        }
        let expected_digest = simulation_request_digest(&spec.request)?;
        let consumption = self
            .simulations
            .consume_simulation(access, &spec.simulation_token, &expected_digest)
            .await?;
        if consumption.result.base_revision != spec.request.base_revision
            || consumption.result.request_digest != expected_digest
        {
            return Err(ProposalSubmissionError::Simulation(
                SchedulingPortError::InvalidQuery(
                    "simulation token does not match the proposal base revision".to_owned(),
                ),
            ));
        }
        let prepared = materialize_proposal(
            &access.subject,
            &spec,
            &consumption.proposal_evidence,
            self.proposals.current_time(),
        )?;
        let proposal = self
            .proposals
            .persist_prepared(prepared)
            .await
            .map_err(|_| ProposalSubmissionError::Unavailable)?;
        submissions.insert(
            key,
            SubmissionRecord {
                proposal: proposal.clone(),
                request_fingerprint: spec.request_fingerprint,
            },
        );
        Ok(ProposalSubmissionResult {
            proposal,
            duplicate: false,
        })
    }
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
    if !has_postgres_timestamp_precision(start) || !has_postgres_timestamp_precision(end) {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            "date boundaries must use PostgreSQL microsecond precision",
        ));
    }
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

fn validate_optional_instant(value: Option<DateTime<Utc>>) -> Result<(), ToolCallError> {
    if value.is_some_and(|value| !has_postgres_timestamp_precision(value)) {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            "date boundaries must use PostgreSQL microsecond precision",
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
        || assumptions.iter().any(|assumption| {
            assumption.chars().count() > 500 || assumption.chars().any(char::is_control)
        })
    {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            format!("at most {MAX_ASSUMPTIONS} assumptions of 500 characters are allowed"),
        ));
    }
    if operations.iter().any(|operation| {
        operation.target_id.as_ref().is_some_and(|target| {
            target.chars().count() > 100 || target.chars().any(char::is_control)
        }) || operation
            .parameters
            .iter()
            .any(|(key, value)| key.chars().any(char::is_control) || unsafe_json_text(value, 0))
    }) {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            "operation targets and parameters contain unsupported text",
        ));
    }
    Ok(())
}

fn unsafe_json_text(value: &Value, depth: usize) -> bool {
    if depth > 64 {
        return true;
    }
    match value {
        Value::String(value) => value.chars().any(char::is_control),
        Value::Array(values) => values
            .iter()
            .any(|value| unsafe_json_text(value, depth + 1)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            key.chars().any(char::is_control) || unsafe_json_text(value, depth + 1)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
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

    let expires_at = match input.expires_at {
        Some(value) if !has_postgres_timestamp_precision(value) => {
            return Err(ToolCallError::execution(
                "invalid_expiration",
                "proposal expiry must use PostgreSQL microsecond precision",
            ));
        }
        Some(value) => value,
        None => truncate_to_postgres_timestamp_precision(maximum_expiration),
    };
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
    if value.trim().is_empty()
        || value.chars().count() > maximum_length
        || value.chars().any(char::is_control)
    {
        return Err(ToolCallError::execution(
            "invalid_arguments",
            format!("{field} must contain between 1 and {maximum_length} characters"),
        ));
    }
    Ok(())
}

fn submission_fingerprint(input: &SubmitProposalInput) -> Result<[u8; 32], ToolCallError> {
    let bytes = serde_json::to_vec(input).map_err(|_| {
        ToolCallError::execution(
            "encoding_failed",
            "proposal content could not be fingerprinted",
        )
    })?;
    Ok(Sha256::digest(bytes).into())
}

fn proposal_output(
    proposal: &crate::proposals::Proposal,
    duplicate: bool,
) -> Result<ToolOutput, ToolCallError> {
    let application_ready = ProposalChangeSet::from_payload(&proposal.payload)
        .is_ok_and(|change_set| proposal_kind_matches_change_set(proposal.kind, &change_set));
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
            "application_ready": application_ready,
            "change_set_schema": application_ready.then_some(PROPOSAL_CHANGE_SET_SCHEMA_V1),
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

fn required_tool_scope(name: &str) -> Option<Scope> {
    match name {
        "get_schedule" | "search_items" | "explain_placement" | "get_conflicts" => {
            Some(Scope::ScheduleRead)
        }
        "simulate_plan" => Some(Scope::ScheduleSimulate),
        "submit_proposal" => Some(Scope::SuggestionsSubmit),
        _ => None,
    }
}

#[must_use]
pub fn requires_idempotency_header(name: &str) -> bool {
    name == "submit_proposal"
}

fn tool_definitions(principal: &Principal) -> Vec<Value> {
    let mut tools = Vec::new();
    let oauth = principal.audience == PrincipalAudience::McpOAuth;
    if oauth || principal.has_scope(Scope::ScheduleRead) {
        tools.extend([
            tool(
                "get_schedule",
                "Get schedule",
                "Read a bounded schedule interval. Sensitive content is redacted by server policy. This tool never changes schedule state.",
                &interval_schema(true),
                &read_annotations(),
                oauth.then_some(SCOPE_FOR_READ),
            ),
            tool(
                "search_items",
                "Search items",
                "Find non-sensitive tasks, habits, routines, goals, breaks, or events by text and filters without changing them.",
                &search_schema(),
                &read_annotations(),
                oauth.then_some(SCOPE_FOR_READ),
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
                oauth.then_some(SCOPE_FOR_READ),
            ),
            tool(
                "get_conflicts",
                "Get conflicts",
                "Read hard violations, soft penalties, overload, and deadline risk in a bounded interval without changing the schedule.",
                &interval_schema(false),
                &read_annotations(),
                oauth.then_some(SCOPE_FOR_READ),
            ),
        ]);
    }
    if oauth || principal.has_scope(Scope::ScheduleSimulate) {
        tools.push(tool(
            "simulate_plan",
            "Simulate plan",
            "Run a side-effect-free what-if plan against a specific schedule revision. Returns an opaque single-use token plus application readiness; canonical state is never changed.",
            &simulation_schema(),
            &read_annotations(),
            oauth.then_some(SCOPE_FOR_SIMULATE),
        ));
    }
    if oauth || principal.has_scope(Scope::SuggestionsSubmit) {
        tools.push(tool(
            "submit_proposal",
            "Submit proposal",
            "Consume the exact simulate_plan token and create only a reviewable Suggestions Inbox proposal. It never applies, creates, edits, moves, completes, deletes, RSVPs, or publishes canonical items. Only an authorized DayWeave device can preview or apply a typed proposal.",
            &proposal_schema(),
            &json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            }),
            oauth.then_some(SCOPE_FOR_SUBMIT),
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
    oauth_scope: Option<&str>,
) -> Value {
    let mut definition = json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input_schema,
        "outputSchema": {
            "type": "object",
            "additionalProperties": true
        },
        "annotations": annotations,
    });
    if let Some(scope) = oauth_scope {
        definition["securitySchemes"] = json!([{
            "type": "oauth2",
            "scopes": [scope],
        }]);
    }
    definition
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
            "simulation_token": { "type": "string", "description": "Required opaque single-use token from the exact simulate_plan request" },
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
            "simulation_token",
            "operations",
        ],
    )
}

fn operation_array_schema() -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "maxItems": MAX_OPERATIONS,
        "description": "The exact simulated operations. Application-ready plans are homogeneous: one create_item, one or more create_event, complete_item, delete_item, or update_constraint operations. Mixed or unsupported plans remain manual-review-only.",
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
                "target_id": {
                    "type": "string",
                    "description": "Canonical item UUID for complete_item, delete_item, update_constraint, or update_item; block UUID for move_block. Omit for creates."
                },
                "parameters": {
                    "type": "object",
                    "description": "Strict parameters. create_item accepts kind (task|habit|routine|goal|break), title, timezone_name, optional is_sensitive/status/notes/duration_seconds/deadline_at/earliest_start_at/recurrence/flexible_constraints/split_policy/importance/urgency/parent_id/sibling_order; IDs are server-generated. create_event uses the same fields but kind is omitted or event. Authorable recurrence supports daily, weekly, monthly, every_interval, after_completion, and frequency. Existing custom RRULE values remain readable but create, replacement, constraint-update, and application-ready proposal writes reject them until bounded RFC 5545 expansion exists. flexible_constraints is the closed DayWeave scheduling-metadata object: core hard/soft constraints, energy/tags/goals, kind-specific habit/routine/goal/break metadata, event representation, split extensions, and legacy preferred_start_minute. Semantic contradictions and unknown, semantically duplicate UUID/dependency sets, or wrong-kind fields are rejected before an application-ready proposal is accepted. Inbox may omit recurrence, duration, or event timing, but fields that are present remain strict. complete_item and delete_item require an empty object. update_constraint accepts only timezone_name, duration_seconds, deadline_at, earliest_start_at, recurrence, flexible_constraints, and split_policy. Unknown fields are rejected for application-ready operations."
                }
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
