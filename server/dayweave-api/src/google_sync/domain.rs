use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

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
    pub revision: u64,
    pub discovered_at: DateTime<Utc>,
    pub configured_at: Option<DateTime<Utc>>,
    pub last_import_at: Option<DateTime<Utc>>,
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
    pub item_id: Uuid,
    pub item_revision: u64,
    pub entity_kind: String,
    pub operation: OutboundOperation,
    pub remote_resource_id: Option<String>,
    pub expected_etag: Option<String>,
    pub payload: Value,
    pub claim_id: Uuid,
    pub attempts: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct OutboundResult {
    pub remote_resource_id: String,
    pub remote_etag: Option<String>,
    pub remote_updated_at: Option<DateTime<Utc>>,
    pub payload_hash: [u8; 32],
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
    pub requested_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct GoogleOutboundAccepted {
    pub outbox_id: Uuid,
    pub replayed: bool,
}
