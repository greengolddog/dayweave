use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::items::{Item, NewItem};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoogleCollectionKind {
    Calendar,
    TaskList,
}

impl GoogleCollectionKind {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::TaskList => "task_list",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoogleSyncRole {
    /// Import provider content for reference, but do not make it a scheduling
    /// constraint and never publish local changes to this collection.
    ReadOnly,
    /// Import busy Calendar events as fixed scheduling constraints.
    Blocking,
    /// Import content and permit guarded writes of DayWeave-owned records.
    Writable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoogleEventDisposition {
    /// Do not retain the provider record in the canonical planning model.
    Ignore,
    /// Retain it for context without reserving schedule capacity.
    VisibleNonblocking,
    /// Retain it and reserve its complete interval when the collection role
    /// permits blocking constraints.
    Blocking,
}

impl GoogleEventDisposition {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::VisibleNonblocking => "visible_nonblocking",
            Self::Blocking => "blocking",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GoogleCalendarPolicy {
    pub confirmed_busy: GoogleEventDisposition,
    pub tentative: GoogleEventDisposition,
    pub free: GoogleEventDisposition,
    pub all_day: GoogleEventDisposition,
    /// All-day publication is opt-in because Google uses date-only exclusive
    /// bounds whose elapsed UTC duration changes across DST transitions.
    pub publish_all_day: bool,
    /// Tentative `DayWeave` blocks stay app-only unless explicitly enabled.
    pub publish_tentative: bool,
    /// Non-busy provider publication is opt-in.
    pub publish_free: bool,
}

impl Default for GoogleCalendarPolicy {
    fn default() -> Self {
        Self {
            confirmed_busy: GoogleEventDisposition::Blocking,
            tentative: GoogleEventDisposition::VisibleNonblocking,
            free: GoogleEventDisposition::VisibleNonblocking,
            all_day: GoogleEventDisposition::VisibleNonblocking,
            publish_all_day: false,
            publish_tentative: false,
            publish_free: false,
        }
    }
}

impl GoogleSyncRole {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Blocking => "blocking",
            Self::Writable => "writable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[allow(clippy::struct_excessive_bools)] // Mirrors independent Google collection flags.
pub struct GoogleSyncCollection {
    pub id: Uuid,
    pub account_id: Uuid,
    pub kind: GoogleCollectionKind,
    pub remote_collection_id: String,
    pub display_name: String,
    pub provider_access_role: Option<String>,
    pub provider_primary: bool,
    pub provider_selected: bool,
    pub provider_hidden: bool,
    pub provider_deleted: bool,
    pub selected: bool,
    pub visible: bool,
    pub sync_role: GoogleSyncRole,
    pub calendar_policy: GoogleCalendarPolicy,
    pub revision: u64,
    pub discovered_at: DateTime<Utc>,
    pub configured_at: Option<DateTime<Utc>>,
    pub last_import_at: Option<DateTime<Utc>>,
    pub planning_projection_state: CalendarProjectionState,
    pub planning_generation: u64,
    pub planning_collection_revision: Option<u64>,
    pub planning_window_start: Option<DateTime<Utc>>,
    pub planning_window_end: Option<DateTime<Utc>>,
    pub planning_window_refreshed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // Preserves the provider discovery contract exactly.
pub(crate) struct DiscoveredCollection {
    pub kind: GoogleCollectionKind,
    pub remote_id: String,
    pub display_name: String,
    pub provider_access_role: Option<String>,
    pub provider_primary: bool,
    pub provider_selected: bool,
    pub provider_hidden: bool,
    pub provider_deleted: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoogleSyncRunState {
    Idle,
    Running,
    Backoff,
    ReauthorizationRequired,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleSyncRunStatus {
    pub account_id: Uuid,
    pub state: GoogleSyncRunState,
    pub requested_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub next_attempt_at: DateTime<Utc>,
    pub consecutive_failures: u32,
    /// Stable, non-secret machine code. Provider response bodies are never
    /// persisted or returned.
    pub last_error_code: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub imported_count: u64,
    pub updated_count: u64,
    pub deleted_count: u64,
    pub conflict_count: u64,
    pub rejected_count: u64,
    /// Monotonic generation of every durable refresh signal accepted for this
    /// account. This is a causal fence; timestamps are presentation metadata.
    pub refresh_generation: u64,
    /// Refresh generation captured by the currently running or most recently
    /// claimed worker run.
    pub claimed_refresh_generation: u64,
    /// Highest refresh generation incorporated by a successful worker run.
    pub completed_refresh_generation: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SyncCounts {
    pub imported: u64,
    pub updated: u64,
    pub deleted: u64,
    pub conflicts: u64,
    pub rejected: u64,
}

impl SyncCounts {
    pub(crate) fn add(&mut self, outcome: ImportOutcome) {
        match outcome {
            ImportOutcome::Created => self.imported += 1,
            ImportOutcome::Updated => self.updated += 1,
            ImportOutcome::Deleted => self.deleted += 1,
            ImportOutcome::Conflict => self.conflicts += 1,
            ImportOutcome::Unchanged => {}
        }
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        self.imported += other.imported;
        self.updated += other.updated;
        self.deleted += other.deleted;
        self.conflicts += other.conflicts;
        self.rejected += other.rejected;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportOutcome {
    Created,
    Updated,
    Deleted,
    Conflict,
    Unchanged,
}

#[derive(Clone, Debug)]
pub(crate) struct SyncClaim {
    pub account_id: Uuid,
    pub claim_id: Uuid,
    pub claim_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredCursor {
    pub encrypted: Vec<u8>,
    pub key_version: u32,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CursorValue {
    Calendar { sync_token: String },
    Tasks { updated_min: DateTime<Utc> },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalendarProjectionState {
    Uninitialized,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CalendarProjectionWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct RejectedRemoteItem {
    pub remote_id: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct CalendarProjectionBatch {
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub changes: Vec<RemoteItemChange>,
    pub rejected: Vec<RejectedRemoteItem>,
    pub window: CalendarProjectionWindow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CalendarProjectionResult {
    pub generation: u64,
    pub complete: bool,
    pub counts: SyncCounts,
}

/// Provider/version state for a recurring Calendar series. This is distinct
/// from a deletion and deliberately contains no canonical item projection.
#[derive(Clone, Debug)]
pub(crate) struct RemoteCalendarSeriesChange {
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub dayweave_item_id: Option<Uuid>,
    pub remote_id: String,
    pub remote_etag: Option<String>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub remote_payload_hash: [u8; 32],
    pub remote_projection_hash: [u8; 32],
    /// Complete normalized provider representation used only to recover an
    /// authenticated DayWeave-owned create after a lost provider response.
    pub reviewed_provider_projection: Option<Value>,
    pub deleted: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RemoteItemChange {
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub dayweave_item_id: Option<Uuid>,
    pub remote_id: String,
    pub remote_parent_id: Option<String>,
    pub remote_etag: Option<String>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub remote_payload_hash: [u8; 32],
    pub remote_projection_hash: [u8; 32],
    /// Complete provider representation used only for authenticated Calendar
    /// create recovery, with provider-assigned version/timestamp fields removed.
    pub reviewed_provider_projection: Option<Value>,
    pub item: Option<NewItem>,
}

impl RemoteItemChange {
    #[must_use]
    pub(crate) const fn is_deleted(&self) -> bool {
        self.item.is_none()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundRequest {
    pub collection_id: Uuid,
    pub item_id: Uuid,
    pub expected_item_revision: u64,
    pub operation: OutboundOperation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleOutboundPreview {
    pub id: Uuid,
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub collection_display_name: String,
    pub item_id: Uuid,
    pub item_revision: u64,
    pub entity_kind: String,
    pub operation: OutboundOperation,
    /// Existing provider object that will be conditionally changed. `None`
    /// means this is a create. This value is part of the approval binding.
    pub provider_resource_id: Option<String>,
    /// Last-seen provider version used for `If-Match`. It is part of the
    /// approval binding and is absent only for a create.
    pub provider_etag: Option<String>,
    /// SHA-256 review binding represented as lower-case hexadecimal. Clients
    /// must display the preview and echo this exact value to approve it.
    pub preview_hash: String,
    pub provider_payload: Value,
    pub expires_at: DateTime<Utc>,
}

// Deliberately not `Debug`: this value carries the one-time bearer capability
// returned to the approving client. Keeping it out of derived debug output
// prevents otherwise-benign request/response logging from disclosing it.
#[derive(Clone, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleOutboundApproval {
    pub preview_id: Uuid,
    /// Returned exactly once. The server stores only its SHA-256 hash.
    pub approval_capability: String,
    pub expires_at: DateTime<Utc>,
}

impl Drop for GoogleOutboundApproval {
    fn drop(&mut self) {
        self.approval_capability.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleGooglePublicationOperation {
    Create,
    Update,
    Delete,
    Noop,
}

impl ScheduleGooglePublicationOperation {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Noop => "noop",
        }
    }
}

/// Review-safe representation of one desired generated-schedule slot change.
///
/// Provider payloads and private ownership markers deliberately stay inside the
/// durable server-side approval record. Sensitive slots arrive with a generic
/// summary, so this review surface never needs canonical item content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ScheduleGooglePublicationChange {
    pub ordinal: u32,
    pub slot_id: Uuid,
    pub source_block_id: Option<Uuid>,
    pub operation: ScheduleGooglePublicationOperation,
    pub provider_resource_id: Option<String>,
    pub provider_etag: Option<String>,
    pub summary: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// Exact, immutable review projection for one generated-schedule publication.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ScheduleGooglePublicationPreview {
    pub id: Uuid,
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub collection_display_name: String,
    pub schedule_revision_id: Uuid,
    pub schedule_revision_number: u64,
    pub preview_hash: String,
    pub create_count: u32,
    pub update_count: u32,
    pub delete_count: u32,
    pub noop_count: u32,
    pub changes: Vec<ScheduleGooglePublicationChange>,
    pub expires_at: DateTime<Utc>,
}

// Deliberately not `Debug`: this value carries the one-time bearer capability
// returned to the approving device. The server stores only its hash.
#[derive(Clone, Eq, PartialEq, Serialize, ToSchema)]
pub struct ScheduleGooglePublicationApproval {
    pub preview_id: Uuid,
    pub approval_capability: String,
    pub expires_at: DateTime<Utc>,
}

impl Drop for ScheduleGooglePublicationApproval {
    fn drop(&mut self) {
        self.approval_capability.zeroize();
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ScheduleGooglePublicationAccepted {
    pub publication_id: Uuid,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleGooglePublicationState {
    Pending,
    Delivering,
    Backoff,
    PartiallyPublished,
    Published,
    Conflict,
    Failed,
    Superseded,
}

/// Content-free aggregate delivery state for one generated-schedule
/// publication. Exact slot content remains available only in its expiring
/// preview and in server-internal dispatch records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ScheduleGooglePublicationStatus {
    pub publication_id: Uuid,
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub schedule_revision_id: Uuid,
    pub state: ScheduleGooglePublicationState,
    pub total_count: u32,
    pub pending_count: u32,
    pub delivering_count: u32,
    pub published_count: u32,
    pub conflicted_count: u32,
    pub failed_count: u32,
    pub superseded_count: u32,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
}

// Deliberately not `Debug`: this is the only publication model that still
// carries the canonical title for a block marked sensitive.
#[derive(Clone)]
pub(crate) struct SchedulePublicationBlock {
    pub source_block_id: Uuid,
    pub item_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub session_index: u16,
    pub incarnation: u32,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_sensitive: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduleBlockMapping {
    pub mapping_id: Uuid,
    pub slot_id: Uuid,
    pub item_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub session_index: u16,
    pub incarnation: u32,
    pub source_block_id: Uuid,
    pub remote_resource_id: String,
    pub remote_etag: String,
    pub desired_payload_hash: [u8; 32],
    pub last_starts_at: DateTime<Utc>,
    pub last_ends_at: DateTime<Utc>,
}

// Deliberately not `Debug`: this transitively carries raw block titles.
#[derive(Clone)]
pub(crate) struct SchedulePublicationSource {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub account_id: Uuid,
    pub collection: GoogleSyncCollection,
    pub schedule_revision_id: Uuid,
    pub schedule_revision_number: u64,
    pub schedule_publication_hash: [u8; 32],
    pub timezone_name: String,
    pub horizon_start: DateTime<Utc>,
    pub horizon_end: DateTime<Utc>,
    pub blocks: Vec<SchedulePublicationBlock>,
    pub mappings: Vec<ScheduleBlockMapping>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedSchedulePublicationChange {
    pub ordinal: u32,
    pub slot_id: Uuid,
    pub source_block_id: Option<Uuid>,
    pub item_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub session_index: u16,
    pub incarnation: u32,
    pub operation: ScheduleGooglePublicationOperation,
    pub mapping_id: Option<Uuid>,
    pub remote_resource_id: Option<String>,
    pub expected_etag: Option<String>,
    pub desired_payload_hash: [u8; 32],
    pub payload: Value,
    pub review_summary: Value,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub intent_hash: [u8; 32],
}

// Deliberately not `Debug`: this transitively carries the raw source snapshot.
#[derive(Clone)]
pub(crate) struct SchedulePublicationPreviewSpec {
    pub id: Uuid,
    pub source: SchedulePublicationSource,
    pub changes: Vec<PreparedSchedulePublicationChange>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct SchedulePublicationApprovalSpec {
    pub account_id: Uuid,
    pub preview_id: Uuid,
    pub expected_preview_hash: [u8; 32],
    pub capability_hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct SchedulePublicationEnqueueSpec {
    pub account_id: Uuid,
    pub preview_id: Uuid,
    pub collection_id: Uuid,
    pub expected_schedule_revision_id: Uuid,
    pub capability_hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct SchedulePublicationWork {
    pub outbox_id: Uuid,
    pub publication_id: Uuid,
    pub change_id: Uuid,
    pub ordinal: u32,
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub collection_remote_id: String,
    pub schedule_revision_id: Uuid,
    pub schedule_revision_number: u64,
    pub schedule_publication_hash: [u8; 32],
    pub slot_id: Uuid,
    pub source_block_id: Option<Uuid>,
    pub item_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub session_index: u16,
    pub incarnation: u32,
    pub operation: ScheduleGooglePublicationOperation,
    pub mapping_id: Option<Uuid>,
    pub remote_resource_id: Option<String>,
    pub expected_etag: Option<String>,
    pub desired_payload_hash: [u8; 32],
    pub payload: Value,
    pub required_scope: String,
    pub intent_hash: [u8; 32],
    pub claim_id: Uuid,
    pub run_claim_id: Uuid,
    pub run_claim_generation: u64,
    pub provider_post_may_have_started: bool,
    pub attempts: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct SchedulePublicationDispatchPermit {
    pub nonce: Uuid,
    pub intent_hash: [u8; 32],
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchedulePublicationObservationSource {
    ProviderResponse,
    ReconciliationRead,
}

impl SchedulePublicationObservationSource {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::ProviderResponse => "provider_response",
            Self::ReconciliationRead => "reconciliation_read",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SchedulePublicationResult {
    pub remote_resource_id: String,
    pub remote_etag: Option<String>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub payload_hash: [u8; 32],
    pub dispatch_nonce: Uuid,
    pub observation_source: SchedulePublicationObservationSource,
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundPreviewSpec {
    pub id: Uuid,
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub collection_remote_id: String,
    pub collection_display_name: String,
    pub required_scope: &'static str,
    pub prepared: PreparedOutbound,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundApprovalSpec {
    pub account_id: Uuid,
    pub preview_id: Uuid,
    pub expected_preview_hash: [u8; 32],
    pub capability_hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundEnqueueSpec {
    pub account_id: Uuid,
    pub request: OutboundRequest,
    pub capability_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutboundOperation {
    Upsert,
    Delete,
}

impl OutboundOperation {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedOutbound {
    pub entity_kind: &'static str,
    pub item: Item,
    pub operation: OutboundOperation,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundWork {
    pub id: Uuid,
    pub account_id: Uuid,
    pub collection_id: Uuid,
    pub collection_remote_id: String,
    pub collection_revision: u64,
    pub item_id: Uuid,
    pub item_revision: u64,
    pub entity_kind: String,
    pub operation: OutboundOperation,
    pub remote_resource_id: Option<String>,
    pub expected_etag: Option<String>,
    pub payload: Value,
    pub required_scope: String,
    pub intent_hash: [u8; 32],
    pub approval_id: Uuid,
    /// Per-outbox delivery claim.
    pub claim_id: Uuid,
    /// Parent sync-run ownership; both values must still match the unexpired
    /// run at every stage, preventing stale-worker and ABA takeover.
    pub run_claim_id: Uuid,
    pub run_claim_generation: u64,
    pub provider_post_may_have_started: bool,
    pub attempts: u32,
}

/// Short-lived immutable authorization lease minted by the final database
/// fence. No database transaction is held over provider network I/O; the nonce
/// is required again by the post-response guardian.
pub(crate) struct OutboundDispatchPermit {
    pub(crate) nonce: Uuid,
    pub(crate) intent_hash: [u8; 32],
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundResult {
    pub remote_resource_id: String,
    pub remote_etag: Option<String>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub payload_hash: [u8; 32],
    pub dispatch_nonce: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SyncFailureKind {
    Backoff,
    ReauthorizationRequired,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleSyncStatus {
    pub run: Option<GoogleSyncRunStatus>,
    pub import_conflicts: u64,
    pub pending_outbound: u64,
    pub conflicted_outbound: u64,
    pub failed_outbound: u64,
    /// Latest stable, redacted outbox error code, if any queued publication is
    /// backing off or needs operator action.
    pub last_outbound_error_code: Option<String>,
    pub last_outbound_error_at: Option<DateTime<Utc>>,
    pub next_outbound_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleSyncRefreshAccepted {
    pub account_id: Uuid,
    pub request_id: Uuid,
    pub refresh_generation: u64,
    pub requested_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleOutboundAccepted {
    pub outbox_id: Uuid,
    pub replayed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schedule_publication_preview_is_review_safe_and_attests_noops() {
        let starts_at = "2026-09-03T09:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("valid start");
        let ends_at = "2026-09-03T10:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("valid end");
        let preview = ScheduleGooglePublicationPreview {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            collection_id: Uuid::new_v4(),
            collection_revision: 7,
            collection_display_name: "Published schedule".to_owned(),
            schedule_revision_id: Uuid::new_v4(),
            schedule_revision_number: 11,
            preview_hash: "00".repeat(32),
            create_count: 0,
            update_count: 0,
            delete_count: 0,
            noop_count: 1,
            changes: vec![ScheduleGooglePublicationChange {
                ordinal: 0,
                slot_id: Uuid::new_v4(),
                source_block_id: Some(Uuid::new_v4()),
                operation: ScheduleGooglePublicationOperation::Noop,
                provider_resource_id: Some("opaque-provider-id".to_owned()),
                provider_etag: Some("opaque-etag".to_owned()),
                summary: "Busy".to_owned(),
                starts_at,
                ends_at,
            }],
            expires_at: ends_at,
        };

        let serialized = serde_json::to_value(preview).expect("serialize preview");
        assert_eq!(serialized.as_object().expect("preview object").len(), 14);
        assert_eq!(serialized["schedule_revision_number"], json!(11));
        assert_eq!(serialized["noop_count"], json!(1));
        let change = &serialized["changes"][0];
        assert_eq!(change.as_object().expect("change object").len(), 9);
        assert_eq!(change["operation"], json!("noop"));
        assert_eq!(change["summary"], json!("Busy"));
        for internal_field in [
            "item_id",
            "occurrence_id",
            "session_index",
            "incarnation",
            "payload",
            "review_summary",
            "intent_hash",
            "ownership_marker",
            "notes",
            "location",
        ] {
            assert!(
                change.get(internal_field).is_none(),
                "leaked {internal_field}"
            );
        }

        for operation in [
            ScheduleGooglePublicationOperation::Create,
            ScheduleGooglePublicationOperation::Update,
            ScheduleGooglePublicationOperation::Delete,
            ScheduleGooglePublicationOperation::Noop,
        ] {
            assert_eq!(
                serde_json::to_value(operation).expect("serialize operation"),
                json!(operation.as_db())
            );
        }
    }

    #[test]
    fn schedule_publication_status_contains_only_aggregate_delivery_metadata() {
        let created_at = "2026-09-03T09:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("valid created time");
        let serialized = serde_json::to_value(ScheduleGooglePublicationStatus {
            publication_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            collection_id: Uuid::new_v4(),
            schedule_revision_id: Uuid::new_v4(),
            state: ScheduleGooglePublicationState::Delivering,
            total_count: 4,
            pending_count: 1,
            delivering_count: 1,
            published_count: 1,
            conflicted_count: 0,
            failed_count: 0,
            superseded_count: 1,
            created_at,
            completed_at: None,
            last_error_code: None,
        })
        .expect("serialize status");

        assert_eq!(serialized.as_object().expect("status object").len(), 15);
        assert_eq!(serialized["state"], json!("delivering"));
        for content_field in [
            "changes",
            "summary",
            "starts_at",
            "ends_at",
            "provider_resource_id",
            "provider_etag",
            "payload",
        ] {
            assert!(
                serialized.get(content_field).is_none(),
                "status leaked {content_field}"
            );
        }
    }
}
