use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::{
    DiscoveredCollection, GoogleOutboundAccepted, GoogleSyncCollection, GoogleSyncRole,
    GoogleSyncRunStatus, ImportOutcome, OutboundResult, OutboundWork, PreparedOutbound,
    RemoteItemChange, StoredCursor, SyncClaim, SyncCounts, SyncFailureKind,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OutboxCounts {
    pub import_conflicts: u64,
    pub pending: u64,
    pub conflicted: u64,
    pub failed: u64,
    pub last_error_code: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub(crate) trait GoogleSyncRepository: Send + Sync {
    async fn replace_discovered(
        &self,
        account_id: Uuid,
        claim: Option<&SyncClaim>,
        kind: super::GoogleCollectionKind,
        collections: Vec<DiscoveredCollection>,
        now: DateTime<Utc>,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncRepositoryError>;

    async fn collections(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<GoogleSyncCollection>, GoogleSyncRepositoryError>;

    async fn collection(
        &self,
        account_id: Uuid,
        collection_id: Uuid,
    ) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError>;

    #[allow(clippy::too_many_arguments)] // Atomic optimistic configuration mutation.
    async fn configure_collection(
        &self,
        account_id: Uuid,
        collection_id: Uuid,
        expected_revision: u64,
        selected: bool,
        visible: bool,
        role: GoogleSyncRole,
        now: DateTime<Utc>,
    ) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError>;

    async fn recover_startup(&self, now: DateTime<Utc>) -> Result<(), GoogleSyncRepositoryError>;

    async fn request_refresh(
        &self,
        account_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> Result<Option<SyncClaim>, GoogleSyncRepositoryError>;

    async fn renew_claim(
        &self,
        claim: &SyncClaim,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn complete_claim(
        &self,
        claim: &SyncClaim,
        counts: &SyncCounts,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn fail_claim(
        &self,
        claim: &SyncClaim,
        kind: SyncFailureKind,
        code: &'static str,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn run_status(
        &self,
        account_id: Uuid,
    ) -> Result<Option<GoogleSyncRunStatus>, GoogleSyncRepositoryError>;

    async fn cursor(
        &self,
        account_id: Uuid,
        collection_key: &str,
    ) -> Result<Option<StoredCursor>, GoogleSyncRepositoryError>;

    #[allow(clippy::too_many_arguments)] // Cursor CAS carries its complete encryption envelope.
    async fn store_cursor(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        collection_key: &str,
        expected_revision: Option<u64>,
        encrypted: Vec<u8>,
        key_version: u32,
        watermark_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn clear_cursor(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        collection_key: &str,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn apply_remote_item(
        &self,
        claim: &SyncClaim,
        change: RemoteItemChange,
        now: DateTime<Utc>,
    ) -> Result<ImportOutcome, GoogleSyncRepositoryError>;

    async fn mark_rejected(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        remote_id: &str,
        reason: &'static str,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn sweep_full_snapshot(
        &self,
        claim: &SyncClaim,
        collection_id: Uuid,
        collection_revision: u64,
        seen_remote_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<SyncCounts, GoogleSyncRepositoryError>;

    async fn enqueue_outbound(
        &self,
        account_id: Uuid,
        prepared: PreparedOutbound,
        collection_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<GoogleOutboundAccepted, GoogleSyncRepositoryError>;

    async fn claim_outbound(
        &self,
        claim: &SyncClaim,
        now: DateTime<Utc>,
    ) -> Result<Option<OutboundWork>, GoogleSyncRepositoryError>;

    async fn renew_outbound(
        &self,
        work: &OutboundWork,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn complete_outbound(
        &self,
        work: &OutboundWork,
        result: OutboundResult,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn fail_outbound(
        &self,
        work: &OutboundWork,
        terminal_state: &'static str,
        code: &'static str,
        available_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

    async fn outbox_counts(
        &self,
        account_id: Uuid,
    ) -> Result<OutboxCounts, GoogleSyncRepositoryError>;
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum GoogleSyncRepositoryError {
    #[error("Google account was not found")]
    AccountNotFound,
    #[error("Google collection was not found")]
    CollectionNotFound,
    #[error("canonical item was not found")]
    ItemNotFound,
    #[error("revision conflict: expected {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("Google collection cannot be configured for that role")]
    InvalidCollectionRole,
    #[error("Google collection is deleted")]
    CollectionDeleted,
    #[error("Google collection is not selected and writable")]
    CollectionNotWritable,
    #[error("Google account is missing the required write authorization")]
    WriteScopeMissing,
    #[error("Google account is missing the required read authorization")]
    ReadScopeMissing,
    #[error("Google resource cannot be changed without a retained provider ETag")]
    ConditionalWriteUnavailable,
    #[error("outbound mutation is not DayWeave-owned")]
    ExternalMutationForbidden,
    #[error("sync lease is no longer owned by this worker")]
    ClaimLost,
    #[error("sync cursor changed concurrently")]
    CursorConflict,
    #[error("Google sync persistence failed")]
    Internal,
}
