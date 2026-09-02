//! Advisory-only assistant contracts and provider boundary.
//!
//! The assistant receives an already-redacted planner projection and can only
//! return text. It has no repository, scheduling, proposal, or tool handle.

pub mod http;
mod openai;
mod strict_json;

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

pub use openai::{OpenAiAssistantProvider, OpenAiAssistantProviderBuildError};

pub const MAX_USER_MESSAGE_BYTES: usize = 8 * 1024;
pub const MAX_HISTORY_ENTRIES: usize = 20;
pub const MAX_HISTORY_BYTES: usize = 32 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 64 * 1024;
pub const MAX_REPLY_BYTES: usize = 32 * 1024;
pub const MAX_MODEL_BYTES: usize = 128;

const MAX_SCHEDULED_BLOCKS: usize = 48;
const MAX_PRIVATE_BUSY_SPANS: usize = 48;
const MAX_PLANNER_ITEMS: usize = 64;
const MAX_TITLE_BYTES: usize = 160;
const MAX_PROJECT_BYTES: usize = 80;
const MAX_TIMEZONE_BYTES: usize = 64;
const MAX_ENUM_BYTES: usize = 32;
const MAX_SPLIT_POLICY_BYTES: usize = 80;

const REQUIRED_OMITTED_FIELDS: [&str; 6] = [
    "account identity and credentials",
    "app-storage paths and server configuration",
    "notes and placement diagnostics",
    "raw recurrence and flexible-constraint payloads",
    "stable item, occurrence, and revision identifiers",
    "sensitive item content; occupancy is represented only as generic busy spans",
];

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantHistoryRole {
    User,
    Assistant,
}

#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantHistoryEntry {
    pub role: AssistantHistoryRole,
    pub content: String,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub enum AssistantContextSchema {
    #[serde(rename = "dayweave.assistant-context/1")]
    #[schema(rename = "dayweave.assistant-context/1")]
    V1,
}

#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantScheduledBlock {
    pub reference: String,
    pub title: String,
    pub kind: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub duration_minutes: i32,
    pub status: String,
    pub project: Option<String>,
    pub energy: String,
    pub is_flexible: bool,
    pub is_hard_constraint: bool,
}

/// Occupancy-only representation. Deliberately has no reference, title, item
/// identity, kind, status, source/provider, or other correlatable metadata.
#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantPrivateBusySpan {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub duration_minutes: i32,
}

#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantPlannerItem {
    pub reference: String,
    pub parent_reference: Option<String>,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub timezone: String,
    pub duration_minutes: Option<i32>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub earliest_start_at: Option<DateTime<Utc>>,
    pub split_policy: String,
    pub importance: i32,
    pub urgency: i32,
    pub is_recurring: bool,
    pub is_executable: bool,
}

/// Versioned context shared by Android and the server. Stable identities and
/// free-form private fields do not exist at this type boundary.
#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantContext {
    pub schema: AssistantContextSchema,
    pub generated_at: DateTime<Utc>,
    pub timezone: String,
    pub scheduled_blocks: Vec<AssistantScheduledBlock>,
    pub private_busy_spans: Vec<AssistantPrivateBusySpan>,
    pub total_scheduled_block_count: u32,
    pub planner_items: Vec<AssistantPlannerItem>,
    pub total_planner_item_count: u32,
    pub pending_suggestion_count: u32,
    pub omitted_fields: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantTurnRequest {
    pub request_id: Uuid,
    pub message: String,
    #[serde(default)]
    pub history: Vec<AssistantHistoryEntry>,
    pub context: AssistantContext,
}

#[derive(Clone, Serialize, ToSchema)]
pub struct AssistantTurnResponse {
    pub request_id: Uuid,
    pub reply: String,
    pub model: String,
    pub generated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AssistantProviderRequest {
    pub request_id: Uuid,
    pub message: String,
    pub history: Vec<AssistantHistoryEntry>,
    pub context: AssistantContext,
    pub(crate) principal_key: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssistantTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

pub struct AssistantProviderResponse {
    pub reply: String,
    pub model: String,
    pub generated_at: DateTime<Utc>,
    pub usage: AssistantTokenUsage,
}

/// Provider failures are deliberately content-free so upstream bodies,
/// credentials, and planner context can never enter API errors or logs.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AssistantProviderError {
    #[error("assistant provider is unavailable")]
    Unavailable,
    #[error("assistant provider is temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("assistant provider rejected the request")]
    Rejected,
    #[error("assistant provider returned an invalid response")]
    InvalidResponse,
    #[error("assistant provider request limit reached")]
    RateLimited,
}

#[async_trait]
pub trait AssistantProvider: Send + Sync {
    async fn respond(
        &self,
        request: AssistantProviderRequest,
    ) -> Result<AssistantProviderResponse, AssistantProviderError>;
}

#[derive(Debug, Default)]
pub struct UnavailableAssistantProvider;

#[async_trait]
impl AssistantProvider for UnavailableAssistantProvider {
    async fn respond(
        &self,
        _request: AssistantProviderRequest,
    ) -> Result<AssistantProviderResponse, AssistantProviderError> {
        Err(AssistantProviderError::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AssistantRequestValidationError {
    #[error("assistant message is invalid")]
    InvalidMessage,
    #[error("assistant history is invalid")]
    InvalidHistory,
    #[error("assistant context is invalid")]
    InvalidContext,
}

impl AssistantTurnRequest {
    pub(crate) fn validate_and_normalize(
        mut self,
        principal_key: [u8; 32],
    ) -> Result<AssistantProviderRequest, AssistantRequestValidationError> {
        if self.request_id.is_nil() || self.message.len() > MAX_USER_MESSAGE_BYTES {
            return Err(AssistantRequestValidationError::InvalidMessage);
        }
        self.message = self.message.trim().to_owned();
        if self.message.is_empty() || contains_forbidden_controls(&self.message) {
            return Err(AssistantRequestValidationError::InvalidMessage);
        }
        validate_history(&self.history)?;
        validate_context(&self.context)?;
        Ok(AssistantProviderRequest {
            request_id: self.request_id,
            message: self.message,
            history: self.history,
            context: self.context,
            principal_key,
        })
    }
}

pub(crate) fn validate_provider_response(
    response: &AssistantProviderResponse,
) -> Result<(), AssistantProviderError> {
    if response.reply.trim().is_empty()
        || response.reply.len() > MAX_REPLY_BYTES
        || contains_forbidden_controls(&response.reply)
        || !valid_model_name(&response.model)
        || response.usage.input_tokens == 0
        || response.usage.output_tokens == 0
        || response
            .usage
            .input_tokens
            .checked_add(response.usage.output_tokens)
            != Some(response.usage.total_tokens)
    {
        return Err(AssistantProviderError::InvalidResponse);
    }
    Ok(())
}

pub(crate) fn valid_model_name(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_MODEL_BYTES
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn validate_history(
    history: &[AssistantHistoryEntry],
) -> Result<(), AssistantRequestValidationError> {
    if history.len() > MAX_HISTORY_ENTRIES {
        return Err(AssistantRequestValidationError::InvalidHistory);
    }
    let mut total_bytes = 0_usize;
    for entry in history {
        let bytes = entry.content.len();
        if bytes > MAX_USER_MESSAGE_BYTES
            || entry.content.trim().is_empty()
            || contains_forbidden_controls(&entry.content)
        {
            return Err(AssistantRequestValidationError::InvalidHistory);
        }
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or(AssistantRequestValidationError::InvalidHistory)?;
        if total_bytes > MAX_HISTORY_BYTES {
            return Err(AssistantRequestValidationError::InvalidHistory);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keeps the redaction contract at one validation boundary.
fn validate_context(context: &AssistantContext) -> Result<(), AssistantRequestValidationError> {
    let encoded =
        serde_json::to_vec(context).map_err(|_| AssistantRequestValidationError::InvalidContext)?;
    if encoded.len() > MAX_CONTEXT_BYTES
        || context.scheduled_blocks.len() > MAX_SCHEDULED_BLOCKS
        || context.private_busy_spans.len() > MAX_PRIVATE_BUSY_SPANS
        || context.planner_items.len() > MAX_PLANNER_ITEMS
        || usize::try_from(context.total_scheduled_block_count)
            .ok()
            .is_none_or(|total| {
                total
                    < context
                        .scheduled_blocks
                        .len()
                        .saturating_add(context.private_busy_spans.len())
            })
        || usize::try_from(context.total_planner_item_count)
            .ok()
            .is_none_or(|total| total < context.planner_items.len())
        || !valid_timezone(&context.timezone)
        || context.omitted_fields.as_slice() != REQUIRED_OMITTED_FIELDS
    {
        return Err(AssistantRequestValidationError::InvalidContext);
    }

    for (index, block) in context.scheduled_blocks.iter().enumerate() {
        if block.reference != format!("block-{}", index + 1)
            || !valid_safe_text(&block.title, MAX_TITLE_BYTES, true)
            || !valid_safe_text(&block.kind, MAX_ENUM_BYTES, false)
            || !valid_interval(block.starts_at, block.ends_at, block.duration_minutes)
            || !valid_safe_text(&block.status, MAX_ENUM_BYTES, false)
            || block
                .project
                .as_ref()
                .is_some_and(|project| !valid_safe_text(project, MAX_PROJECT_BYTES, true))
            || !valid_safe_text(&block.energy, MAX_ENUM_BYTES, false)
        {
            return Err(AssistantRequestValidationError::InvalidContext);
        }
    }

    for span in &context.private_busy_spans {
        if !valid_interval(span.starts_at, span.ends_at, span.duration_minutes) {
            return Err(AssistantRequestValidationError::InvalidContext);
        }
    }

    let references = context
        .planner_items
        .iter()
        .map(|item| item.reference.as_str())
        .collect::<HashSet<_>>();
    if references.len() != context.planner_items.len() {
        return Err(AssistantRequestValidationError::InvalidContext);
    }
    for (index, item) in context.planner_items.iter().enumerate() {
        if item.reference != format!("item-{}", index + 1)
            || item
                .parent_reference
                .as_ref()
                .is_some_and(|parent| !references.contains(parent.as_str()))
            || !valid_safe_text(&item.title, MAX_TITLE_BYTES, true)
            || !valid_safe_text(&item.kind, MAX_ENUM_BYTES, false)
            || !valid_safe_text(&item.status, MAX_ENUM_BYTES, false)
            || !valid_timezone(&item.timezone)
            || item.duration_minutes.is_some_and(|duration| duration < 0)
            || !valid_safe_text(&item.split_policy, MAX_SPLIT_POLICY_BYTES, false)
            || !(0..=100).contains(&item.importance)
            || !(0..=100).contains(&item.urgency)
        {
            return Err(AssistantRequestValidationError::InvalidContext);
        }
    }
    Ok(())
}

fn valid_timezone(timezone: &str) -> bool {
    valid_safe_text(timezone, MAX_TIMEZONE_BYTES, false)
}

fn valid_interval(start: DateTime<Utc>, end: DateTime<Utc>, duration_minutes: i32) -> bool {
    let seconds = (end - start).num_seconds();
    seconds > 0 && duration_minutes > 0 && seconds / 60 == i64::from(duration_minutes)
}

fn valid_safe_text(value: &str, maximum_bytes: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.trim().is_empty())
        && value.len() <= maximum_bytes
        && !value
            .chars()
            .any(|character| character.is_control() || is_directional_control(character))
}

fn contains_forbidden_controls(value: &str) -> bool {
    value.chars().any(|character| {
        (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
            || is_directional_control(character)
    })
}

fn is_directional_control(character: char) -> bool {
    matches!(
        character,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> AssistantContext {
        AssistantContext {
            schema: AssistantContextSchema::V1,
            generated_at: "2026-09-03T08:00:00Z".parse().unwrap(),
            timezone: "Europe/Paris".to_owned(),
            scheduled_blocks: vec![AssistantScheduledBlock {
                reference: "block-1".to_owned(),
                title: "Public meeting".to_owned(),
                kind: "event".to_owned(),
                starts_at: "2026-09-03T09:00:00Z".parse().unwrap(),
                ends_at: "2026-09-03T10:00:00Z".parse().unwrap(),
                duration_minutes: 60,
                status: "planned".to_owned(),
                project: None,
                energy: "medium".to_owned(),
                is_flexible: false,
                is_hard_constraint: true,
            }],
            private_busy_spans: vec![AssistantPrivateBusySpan {
                starts_at: "2026-09-03T11:00:00Z".parse().unwrap(),
                ends_at: "2026-09-03T12:00:00Z".parse().unwrap(),
                duration_minutes: 60,
            }],
            total_scheduled_block_count: 2,
            planner_items: vec![AssistantPlannerItem {
                reference: "item-1".to_owned(),
                parent_reference: None,
                title: "Write the report".to_owned(),
                kind: "task".to_owned(),
                status: "active".to_owned(),
                timezone: "Europe/Paris".to_owned(),
                duration_minutes: Some(45),
                deadline_at: None,
                earliest_start_at: None,
                split_policy: "indivisible".to_owned(),
                importance: 70,
                urgency: 60,
                is_recurring: false,
                is_executable: true,
            }],
            total_planner_item_count: 1,
            pending_suggestion_count: 0,
            omitted_fields: REQUIRED_OMITTED_FIELDS
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    fn request() -> AssistantTurnRequest {
        AssistantTurnRequest {
            request_id: Uuid::from_u128(1),
            message: "What can I move?".to_owned(),
            history: vec![AssistantHistoryEntry {
                role: AssistantHistoryRole::Assistant,
                content: "I can inspect the redacted plan.".to_owned(),
            }],
            context: context(),
        }
    }

    #[test]
    fn accepts_the_android_redacted_context_contract() {
        let validated = request()
            .validate_and_normalize([7; 32])
            .expect("valid request");
        assert_eq!(validated.message, "What can I move?");
        assert_eq!(validated.context.scheduled_blocks[0].reference, "block-1");
    }

    #[test]
    fn rejects_unbounded_or_directional_conversation_text() {
        let mut oversized = request();
        oversized.message = "x".repeat(MAX_USER_MESSAGE_BYTES + 1);
        assert_eq!(
            oversized.validate_and_normalize([7; 32]).err(),
            Some(AssistantRequestValidationError::InvalidMessage)
        );

        let mut directional = request();
        directional.history[0].content = "hidden\u{202E}text".to_owned();
        assert_eq!(
            directional.validate_and_normalize([7; 32]).err(),
            Some(AssistantRequestValidationError::InvalidHistory)
        );
    }

    #[test]
    fn rejects_manifest_alias_and_private_metadata_drift() {
        let mut omission_drift = request();
        omission_drift.context.omitted_fields.pop();
        assert_eq!(
            omission_drift.validate_and_normalize([7; 32]).err(),
            Some(AssistantRequestValidationError::InvalidContext)
        );

        let mut stable_identifier = request();
        stable_identifier.context.planner_items[0].reference = Uuid::new_v4().to_string();
        assert_eq!(
            stable_identifier.validate_and_normalize([7; 32]).err(),
            Some(AssistantRequestValidationError::InvalidContext)
        );

        let mut directional_title = request();
        directional_title.context.planner_items[0].title = "spoofed\u{202e}title".to_owned();
        assert_eq!(
            directional_title.validate_and_normalize([7; 32]).err(),
            Some(AssistantRequestValidationError::InvalidContext)
        );

        let mut private_metadata = serde_json::to_value(context()).unwrap();
        private_metadata["private_busy_spans"][0]["title"] = serde_json::json!("secret canary");
        assert!(serde_json::from_value::<AssistantContext>(private_metadata).is_err());
    }

    #[test]
    fn rejects_contexts_over_the_serialized_byte_limit() {
        let mut oversized = request();
        oversized.context.omitted_fields = vec!["x".repeat(MAX_CONTEXT_BYTES)];
        assert_eq!(
            oversized.validate_and_normalize([7; 32]).err(),
            Some(AssistantRequestValidationError::InvalidContext)
        );
    }
}
