use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyncCursor(pub String);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalChange {
    pub external_id: String,
    pub etag: Option<String>,
    pub deleted: bool,
    pub modified_at: DateTime<Utc>,
    pub body: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangePage {
    pub changes: Vec<ExternalChange>,
    pub next_cursor: SyncCursor,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExternalMutation {
    pub external_id: Option<String>,
    pub expected_etag: Option<String>,
    pub body: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MutationReceipt {
    pub external_id: String,
    pub etag: String,
    pub modified_at: DateTime<Utc>,
}

#[async_trait]
pub trait GoogleCalendarPort: Send + Sync {
    async fn pull_calendar_changes(
        &self,
        account_id: &str,
        cursor: Option<&SyncCursor>,
    ) -> Result<ChangePage, IntegrationError>;

    async fn apply_calendar_mutation(
        &self,
        account_id: &str,
        mutation: ExternalMutation,
    ) -> Result<MutationReceipt, IntegrationError>;
}

#[async_trait]
pub trait GoogleTasksPort: Send + Sync {
    async fn pull_task_changes(
        &self,
        account_id: &str,
        cursor: Option<&SyncCursor>,
    ) -> Result<ChangePage, IntegrationError>;

    async fn apply_task_mutation(
        &self,
        account_id: &str,
        mutation: ExternalMutation,
    ) -> Result<MutationReceipt, IntegrationError>;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CodexRequest {
    pub conversation_id: Option<String>,
    pub instructions: String,
    pub context: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CodexProposalDraft {
    pub conversation_id: String,
    pub explanation: String,
    pub proposal_payloads: Vec<Value>,
}

#[async_trait]
pub trait CodexAppServerPort: Send + Sync {
    async fn request_proposals(
        &self,
        request: CodexRequest,
    ) -> Result<CodexProposalDraft, IntegrationError>;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IntegrationError {
    #[error("integration authorization is required")]
    Unauthorized,
    #[error("integration rate limit reached")]
    RateLimited,
    #[error("integration is temporarily unavailable")]
    Unavailable,
    #[error("integration returned an invalid response")]
    InvalidResponse,
    #[error("integration rejected a conflicting update")]
    Conflict,
}
