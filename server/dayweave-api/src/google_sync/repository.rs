use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::{
    CalendarProjectionBatch, CalendarProjectionResult, DiscoveredCollection, GoogleCalendarPolicy,
    GoogleOutboundAccepted, GoogleOutboundPreview, GoogleSyncCollection, GoogleSyncRefreshAccepted,
    GoogleSyncRole, GoogleSyncRunStatus, ImportOutcome, OutboundApprovalSpec,
    OutboundDispatchPermit, OutboundEnqueueSpec, OutboundPreviewSpec, OutboundResult, OutboundWork,
    RemoteCalendarSeriesChange, RemoteItemChange, StoredCursor, SyncClaim, SyncCounts,
    SyncFailureKind,
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
    async fn verify_or_initialize_identity_root(
        &self,
        identity_key_version: u32,
        root_verifier: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<(), GoogleSyncRepositoryError>;

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
        calendar_policy: GoogleCalendarPolicy,
        now: DateTime<Utc>,
    ) -> Result<GoogleSyncCollection, GoogleSyncRepositoryError>;

    async fn recover_startup(&self, now: DateTime<Utc>) -> Result<(), GoogleSyncRepositoryError>;

    /// Returns the immutable acceptance record for an exact manual-refresh
    /// request identity. Implementations must scope the lookup to the current
    /// workspace and user, but must not require the provider account to remain
    /// active: response-loss recovery outlives later pause or disconnect
    /// mutations.
    async fn refresh_request(
        &self,
        account_id: Uuid,
        request_id: Uuid,
    ) -> Result<Option<GoogleSyncRefreshAccepted>, GoogleSyncRepositoryError>;

    async fn request_refresh(
        &self,
        account_id: Uuid,
        request_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<GoogleSyncRefreshAccepted, GoogleSyncRepositoryError>;

    /// Invalidates every selected Calendar projection before discovery or any
    /// provider read. A failed/partial refresh can therefore never leave stale
    /// complete coverage available to the scheduler.
    async fn begin_calendar_projection_refresh(
        &self,
        claim: &SyncClaim,
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

    /// Atomically replaces one complete bounded Calendar occurrence window.
    /// Rejections invalidate coverage without partially applying the batch.
    async fn replace_calendar_projection(
        &self,
        claim: &SyncClaim,
        batch: CalendarProjectionBatch,
        now: DateTime<Utc>,
    ) -> Result<CalendarProjectionResult, GoogleSyncRepositoryError>;

    /// Retains recurrence-series provider/version metadata without treating a
    /// live series master as either a canonical event or a deletion.
    async fn apply_calendar_series_metadata(
        &self,
        claim: &SyncClaim,
        change: RemoteCalendarSeriesChange,
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

    async fn create_outbound_preview(
        &self,
        spec: OutboundPreviewSpec,
        now: DateTime<Utc>,
    ) -> Result<GoogleOutboundPreview, GoogleSyncRepositoryError>;

    async fn approve_outbound(
        &self,
        spec: OutboundApprovalSpec,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, GoogleSyncRepositoryError>;

    async fn enqueue_outbound(
        &self,
        spec: OutboundEnqueueSpec,
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

    async fn authorize_outbound_dispatch(
        &self,
        work: &OutboundWork,
        provider_write: bool,
        now: DateTime<Utc>,
    ) -> Result<OutboundDispatchPermit, GoogleSyncRepositoryError>;

    async fn cancel_outbound_before_send(
        &self,
        work: &OutboundWork,
        code: &'static str,
        available_at: DateTime<Utc>,
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
    #[error("outbound preview or approval capability is invalid")]
    ApprovalInvalid,
    #[error("outbound preview or approval capability expired")]
    ApprovalExpired,
    #[error("outbound preview was already approved")]
    ApprovalAlreadyIssued,
    #[error("configured Google provider-identity root does not match its durable binding")]
    IdentityRootMismatch,
    #[error("sync lease is no longer owned by this worker")]
    ClaimLost,
    #[error("sync cursor changed concurrently")]
    CursorConflict,
    #[error("canonical item is targeted by an active execution session")]
    ItemExecutionActive,
    #[error("expanded Calendar projection batch is invalid")]
    InvalidProjectionBatch,
    #[error("Google sync persistence failed")]
    Internal,
}
